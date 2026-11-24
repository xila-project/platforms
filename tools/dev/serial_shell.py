#!/usr/bin/env python3
"""Drive a Xila board's serial command-line shell: log in, then run commands.

Commands are read from standard input, one per line, starting with the login
(the board asks for a user name, then a password). The line `<empty>` sends
an empty line, for prompts that take no input. Everything the board prints
is written to standard output with ANSI escape sequences stripped.

    python3 tools/dev/serial_shell.py /dev/ttyUSB0 <<'COMMANDS'
    administrator

    whoami
    COMMANDS

Standard library only, Python 3. The serial device has no default: pass the
board's console port explicitly.
"""

import argparse
import contextlib
import os
import re
import select
import sys
import termios
import time

BAUD_RATE = termios.B115200
# Control sequence introducer: parameter bytes (including private markers such
# as "?"), intermediate bytes, then the final byte.
ANSI_ESCAPE_SEQUENCE = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
EMPTY_LINE = "<empty>"


@contextlib.contextmanager
def open_raw_port(port):
    """Open the serial device in raw mode at the console baud rate.

    The device's original settings are restored, and the device is closed,
    when the block exits, whether it exits normally or by an error.
    """
    descriptor = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        original = termios.tcgetattr(descriptor)
        attributes = list(original)
        attributes[0] = 0  # input flags
        attributes[1] = 0  # output flags
        # HUPCL stays set so that the last close releases RTS and DTR.
        attributes[2] = termios.CS8 | termios.CREAD | termios.CLOCAL | termios.HUPCL
        attributes[3] = 0  # local flags: raw mode
        attributes[4] = attributes[5] = BAUD_RATE
        attributes[6] = list(original[6])  # copied, so the original settings stay untouched
        attributes[6][termios.VMIN] = 0  # reads never block; select() does the waiting
        attributes[6][termios.VTIME] = 0
        termios.tcsetattr(descriptor, termios.TCSANOW, attributes)
        try:
            yield descriptor
        finally:
            termios.tcsetattr(descriptor, termios.TCSANOW, original)
    finally:
        os.close(descriptor)


def read_for(descriptor, seconds):
    """Read everything the port delivers for the given number of seconds."""
    output = b""
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([descriptor], [], [], 0.1)
        if ready:
            try:
                output += os.read(descriptor, 4096)
            except BlockingIOError:
                pass
    return output


def send(descriptor, line, wait=1.5):
    """Send one command line, carriage return terminated, and read the reply."""
    for character in (line + "\r").encode():
        deadline = time.time() + 10
        while True:
            try:
                os.write(descriptor, bytes([character]))
                break
            except BlockingIOError:
                if time.time() > deadline:
                    raise
                time.sleep(0.05)
        time.sleep(0.004)
    return read_for(descriptor, wait)


def run_commands(port, commands, wait=2.0):
    """Open the port, send each command line and return the raw transcript."""
    transcript = b""
    with open_raw_port(port) as descriptor:
        transcript = read_for(descriptor, 1.0)
        for command in commands:
            transcript += send(descriptor, "" if command == EMPTY_LINE else command, wait)
    return transcript


def strip_ansi_sequences(transcript):
    """Decode a raw transcript without ANSI escape sequences."""
    return ANSI_ESCAPE_SEQUENCE.sub(b"", transcript).decode(errors="replace")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "port",
        help="serial device of the board's console, for example /dev/ttyUSB0 (required, no default)",
    )
    arguments = parser.parse_args()
    commands = sys.stdin.read().splitlines()
    sys.stdout.write(strip_ansi_sequences(run_commands(arguments.port, commands)))


if __name__ == "__main__":
    main()
