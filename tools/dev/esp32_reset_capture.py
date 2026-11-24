#!/usr/bin/env python3
"""Reset an ESP32 through the RTS line of its USB-to-UART bridge and capture the boot log.

Works for any ESP32 board whose USB-to-UART bridge follows the classic
auto-reset circuit: RTS wired to EN (reset) and DTR wired to GPIO0. The tool
asserts RTS to hold the chip in reset, lowers DTR to keep GPIO0 high (normal
boot, not download mode), then releases RTS and prints everything the board
sends on the UART for the given number of seconds.

    python3 tools/dev/esp32_reset_capture.py /dev/ttyUSB0 5

Standard library only (termios and ioctl, no pyserial), Python 3. The serial
device has no default: pass the board's port explicitly.
"""

import argparse
import fcntl
import os
import select
import struct
import sys
import termios
import time

BAUD_RATE = termios.B115200


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


def set_modem_lines(descriptor, set_bits, clear_bits):
    """Set and clear modem control lines (TIOCM_RTS, TIOCM_DTR, ...) of the port."""
    if clear_bits:
        fcntl.ioctl(descriptor, termios.TIOCMBIC, struct.pack("I", clear_bits))
    if set_bits:
        fcntl.ioctl(descriptor, termios.TIOCMBIS, struct.pack("I", set_bits))


def reset_and_capture(port, seconds):
    """Reset the chip through RTS and return everything it prints in `seconds`."""
    descriptor = open_port(port)
    try:
        # DTR low keeps GPIO0 high (normal boot); RTS asserted holds EN low.
        set_modem_lines(descriptor, termios.TIOCM_RTS, termios.TIOCM_DTR)
        time.sleep(0.2)
        set_modem_lines(descriptor, 0, termios.TIOCM_RTS)  # release reset
        output = b""
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([descriptor], [], [], 0.2)
            if ready:
                try:
                    output += os.read(descriptor, 4096)
                except BlockingIOError:
                    pass
    finally:
        os.close(descriptor)
    return output.decode("utf-8", "replace")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "port",
        help="serial device of the board, for example /dev/ttyUSB0 (required, no default)",
    )
    parser.add_argument(
        "seconds",
        type=float,
        help="how long to capture the boot log after the reset, in seconds",
    )
    arguments = parser.parse_args()
    sys.stdout.write(reset_and_capture(arguments.port, arguments.seconds))


if __name__ == "__main__":
    main()
