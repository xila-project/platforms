#!/usr/bin/env python3
"""Provision Wi-Fi on a Xila board by writing /configuration/system/wifi through its serial shell.

The file holds one value per line: the Wi-Fi network name, the passphrase
(an empty line means an open network) and optionally a static IPv4 address
as ADDRESS/PREFIX, gateway and name server; without those three lines the
board uses DHCP. The file must be owned by root with no group or other
permissions, so the tool logs in with the administrator account, switches
to a root command-line shell and writes the file there. The lines go to a
temporary file that is restricted to mode 600 before any secret is written,
and it is then renamed over the Wi-Fi file, so an existing profile is replaced
only once the new one is complete.

The network name and the passphrase are read from the files given as
arguments, never from the command line, and are never printed: everything
this tool outputs has them replaced by ***. The static address lines are
plain network settings and are shown as sent.

    python3 tools/dev/provision_wifi.py /dev/ttyUSB0 network-name.txt passphrase.txt
    python3 tools/dev/provision_wifi.py /dev/ttyUSB0 network-name.txt passphrase.txt 192.0.2.2/24 192.0.2.1 192.0.2.1

Standard library only, Python 3. The serial device has no default: pass the
board's console port explicitly.
"""

import argparse
import ipaddress
import string
import sys
from pathlib import Path

from serial_shell import run_commands, strip_ansi_sequences

WIFI_PATH = "/configuration/system/wifi"
WIFI_TEMPORARY_PATH = "/configuration/system/.wifi-new"  # the firmware's own temporary name
ROOT_SHELL = "/applications/executables/command_line_shell"
MAXIMUM_NETWORK_NAME_BYTES = 32
MINIMUM_PASSPHRASE_CHARACTERS = 8
MAXIMUM_PASSPHRASE_CHARACTERS = 63
HEXADECIMAL_KEY_CHARACTERS = 64  # a WPA2 key written as 64 hexadecimal digits instead of a passphrase
MAXIMUM_FILE_BYTES = 160  # the firmware rejects a larger /configuration/system/wifi
BROADCAST_ADDRESS = ipaddress.IPv4Address("255.255.255.255")
MASK = "***"


def read_single_line(path, description):
    """Read one line from a file, without its line ending."""
    try:
        value = path.read_text(encoding="utf-8-sig").strip("\r\n")
    except UnicodeDecodeError:
        # The decoder's message would quote the offending byte, which may be part of a secret.
        raise SystemExit(f"the {description} file {path} must hold UTF-8 text")
    except OSError as error:
        raise SystemExit(f"cannot read the {description} file {path}: {error}")
    if "\n" in value or "\x00" in value:
        raise SystemExit(f"the {description} file {path} must hold a single line without NUL characters")
    return value


def is_valid_passphrase(passphrase):
    """Check a WPA2 passphrase as the firmware does: printable ASCII, or a 64-digit hexadecimal key."""
    if len(passphrase) == HEXADECIMAL_KEY_CHARACTERS and all(
        character in string.hexdigits for character in passphrase
    ):
        return True
    return (
        MINIMUM_PASSPHRASE_CHARACTERS <= len(passphrase) <= MAXIMUM_PASSPHRASE_CHARACTERS
        and passphrase.isascii()
        and passphrase.isprintable()
    )


def validate_static_address(lines, parser):
    """Check the optional ADDRESS/PREFIX GATEWAY NAME_SERVER lines."""
    if not lines:
        return
    if len(lines) != 3:
        parser.error("static IPv4 settings need exactly three lines: ADDRESS/PREFIX GATEWAY NAME_SERVER")
    try:
        ipaddress.IPv4Interface(lines[0])
        for line in lines[1:]:
            address = ipaddress.IPv4Address(line)
            if address.is_unspecified or address.is_multicast or address == BROADCAST_ADDRESS:
                raise ValueError(f"{line} cannot be a gateway or name server")
    except ValueError as error:
        parser.error(f"invalid static IPv4 settings: {error}")


def build_commands(user, already_root, network_name, passphrase, static_address_lines):
    """Build the shell command lines that log in and write the Wi-Fi file."""
    commands = [
        user,
        "",  # password prompt of the seeded account
        f"sudo {ROOT_SHELL}",
        "",  # password prompt of sudo
        "whoami",
        f"touch {WIFI_TEMPORARY_PATH}",
        f"chmod 600 {WIFI_TEMPORARY_PATH}",  # restricted before the first secret is written
        f"echo '{network_name}' > {WIFI_TEMPORARY_PATH}",
        f"echo '{passphrase}' >> {WIFI_TEMPORARY_PATH}",
    ]
    commands += [f"echo '{line}' >> {WIFI_TEMPORARY_PATH}" for line in static_address_lines]
    commands += [
        f"mv {WIFI_TEMPORARY_PATH} {WIFI_PATH}",
        f"ls {WIFI_PATH}",
        f"wc -l {WIFI_PATH}",
        "exit",
    ]
    if already_root:
        commands = commands[4:]  # the port already runs a root shell
    return commands


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "port",
        help="serial device of the board's console, for example /dev/ttyUSB0 (required, no default)",
    )
    parser.add_argument(
        "network_name_file",
        type=Path,
        help="file whose single line is the Wi-Fi network name (SSID)",
    )
    parser.add_argument(
        "passphrase_file",
        type=Path,
        help="file whose single line is the Wi-Fi passphrase (an empty file means an open network)",
    )
    parser.add_argument(
        "static_address",
        nargs="*",
        metavar="LINE",
        help="optional static IPv4 settings: ADDRESS/PREFIX GATEWAY NAME_SERVER (all three, or none to use DHCP)",
    )
    parser.add_argument(
        "--user",
        default="administrator",
        help="board account used to log in (default: %(default)s)",
    )
    parser.add_argument(
        "--already-root",
        action="store_true",
        help="the shell on the port is already a root shell; skip the login and sudo",
    )
    arguments = parser.parse_args()
    validate_static_address(arguments.static_address, parser)

    network_name = read_single_line(arguments.network_name_file, "network name")
    passphrase = read_single_line(arguments.passphrase_file, "passphrase")
    if not network_name:
        parser.error("the network name must not be empty")
    if len(network_name.encode("utf-8")) > MAXIMUM_NETWORK_NAME_BYTES:
        parser.error(f"the network name must not exceed {MAXIMUM_NETWORK_NAME_BYTES} bytes")
    if not network_name.isprintable():
        parser.error("the network name must contain only printable characters")
    if passphrase and not is_valid_passphrase(passphrase):
        parser.error(
            f"the passphrase must be {MINIMUM_PASSPHRASE_CHARACTERS} to {MAXIMUM_PASSPHRASE_CHARACTERS} "
            f"printable ASCII characters or {HEXADECIMAL_KEY_CHARACTERS} hexadecimal digits"
        )
    file_bytes = sum(len(line.encode("utf-8")) + 1 for line in (network_name, passphrase, *arguments.static_address))
    if file_bytes > MAXIMUM_FILE_BYTES:
        parser.error(f"the Wi-Fi file would exceed {MAXIMUM_FILE_BYTES} bytes, the firmware's limit")
    for description, value in (
        ("network name", network_name),
        ("passphrase", passphrase),
        *(("static address line", line) for line in arguments.static_address),
    ):
        if "'" in value:
            parser.error(f"the {description} contains a single quote; it cannot be written with echo")

    commands = build_commands(
        arguments.user, arguments.already_root, network_name, passphrase, arguments.static_address
    )
    transcript = run_commands(arguments.port, commands)
    text = strip_ansi_sequences(transcript)
    # Longest first, so a value that contains another one is masked whole.
    for value in sorted((passphrase, network_name), key=len, reverse=True):
        if value:
            text = text.replace(value, MASK)
    print(text[-1500:])
    # The writes only succeed in a root shell: the firmware ignores a Wi-Fi
    # file that root does not own. Report a failed login or sudo instead of
    # exiting as if the board had been provisioned.
    if not any(line.strip() == "root" for line in text.splitlines()):
        sys.exit("error: the shell did not report root after sudo; the Wi-Fi file was not provisioned")


if __name__ == "__main__":
    main()
