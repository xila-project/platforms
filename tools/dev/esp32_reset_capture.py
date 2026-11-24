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
import math
import os
import select
import struct
import sys
import termios
import time

from serial_shell import open_raw_port


def set_modem_lines(descriptor, set_bits, clear_bits):
    """Set and clear modem control lines (TIOCM_RTS, TIOCM_DTR, ...) of the port."""
    if clear_bits:
        fcntl.ioctl(descriptor, termios.TIOCMBIC, struct.pack("I", clear_bits))
    if set_bits:
        fcntl.ioctl(descriptor, termios.TIOCMBIS, struct.pack("I", set_bits))


def reset_and_capture(port, seconds):
    """Reset the chip through RTS and return everything it prints in `seconds`."""
    output = b""
    with open_raw_port(port) as descriptor:
        try:
            # DTR low keeps GPIO0 high (normal boot); RTS asserted holds EN low.
            set_modem_lines(descriptor, termios.TIOCM_RTS, termios.TIOCM_DTR)
            time.sleep(0.2)
        finally:
            # Release the reset on every exit path, so an interrupt never leaves the chip held in reset.
            set_modem_lines(descriptor, 0, termios.TIOCM_RTS)
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([descriptor], [], [], 0.2)
            if ready:
                try:
                    output += os.read(descriptor, 4096)
                except BlockingIOError:
                    pass
    return output.decode("utf-8", "replace")


def capture_duration(text):
    """Parse the capture duration: a finite, positive number of seconds."""
    try:
        seconds = float(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a number of seconds: {text!r}")
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("the capture must last a finite, positive number of seconds")
    return seconds


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
        type=capture_duration,
        help="how long to capture the boot log after the reset, in seconds",
    )
    arguments = parser.parse_args()
    sys.stdout.write(reset_and_capture(arguments.port, arguments.seconds))


if __name__ == "__main__":
    main()
