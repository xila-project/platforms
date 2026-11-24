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
import os
import re
import select
import sys
import termios
import time

BAUD_RATE = termios.B115200
ANSI_ESCAPE_SEQUENCE = re.compile(rb"\x1b\[[0-9;]*[A-Za-z]")
EMPTY_LINE = "<empty>"


def open_port(port):
    """Open the serial device in raw mode at the console baud rate."""
    descriptor = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    attributes = termios.tcgetattr(descriptor)
    attributes[0] = 0  # input flags
    attributes[1] = 0  # output flags
    attributes[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    attributes[3] = 0  # local flags: raw mode
    attributes[4] = attributes[5] = BAUD_RATE
    termios.tcsetattr(descriptor, termios.TCSANOW, attributes)
    return descriptor


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
    descriptor = open_port(port)
    try:
        transcript = read_for(descriptor, 1.0)
        for command in commands:
            transcript += send(descriptor, "" if command == EMPTY_LINE else command, wait)
    finally:
        os.close(descriptor)
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
