# Development tools

Helper scripts for working on Xila boards from a host computer. The Python
tools are standard library only, run with Python 3 and are executable
directly; pass the board's serial device explicitly, for example
`/dev/ttyUSB0` — none of them has a default device path.

The build and flash helpers (`build_espressif_image.sh`,
`check_espressif.sh`, `install_espressif_emulator.sh`,
`generate_espressif_pins.py`, `espressif_smoke.py`, `local_overrides.py`)
are driven by the `mise` tasks documented in the
[repository README](../../README.md); the board helpers below are run by
hand against real hardware.

## Board serial helpers

All three helpers talk to the board's UART console (115200 baud, 8N1, raw)
over a USB-to-UART bridge on the host, using only the standard library
(`termios`, and `fcntl` for the modem lines) — no pyserial or other
dependency. Each helper restores the port's settings when it exits. Linux only.

### `serial_shell.py` — run commands in the board's serial shell

Sends the lines read from standard input to the board's shell, one per line.
Start them with the login: the user name, then the password. The line
`<empty>` sends an empty line, for prompts that take no input. The transcript
of everything the board prints is written to standard output with ANSI escape
sequences stripped.

```sh
python3 tools/dev/serial_shell.py /dev/ttyUSB0 <<'COMMANDS'
administrator

whoami
COMMANDS
```

### `esp32_reset_capture.py` — reset an ESP32 and capture its boot log

Resets the chip through the RTS line of the USB-to-UART bridge and prints
the boot log. It works for any ESP32 behind a bridge that follows the
classic auto-reset circuit (RTS wired to EN, DTR wired to GPIO0): RTS is
asserted to hold the chip in reset while DTR is lowered to keep GPIO0 high
(normal boot), then RTS is released and the UART output is captured for the
given number of seconds.

```sh
python3 tools/dev/esp32_reset_capture.py /dev/ttyUSB0 5
```

### `provision_wifi.py` — write the board's Wi-Fi configuration

Writes `/configuration/system/wifi` through the serial shell: it logs in with
the administrator account, switches to a root command-line shell (the file
must be owned by root with no group or other permissions) and writes one
value per line — the network name (1 to 32 bytes), the passphrase (an empty
line means an open network; otherwise 8 to 63 printable ASCII characters, or
64 hexadecimal digits for a key) and optionally a static IPv4 address as
`ADDRESS/PREFIX GATEWAY NAME_SERVER` (without those three lines the board
uses DHCP). The file is written under a temporary name, restricted to mode
600 before any secret goes into it, and renamed over the Wi-Fi file, so an
existing profile stays in place until the new one is complete.

The network name and the passphrase are read from the files given as
arguments, never from the command line, and are never printed — everything
the tool outputs has them replaced by `***`.

```sh
python3 tools/dev/provision_wifi.py /dev/ttyUSB0 network-name.txt passphrase.txt
python3 tools/dev/provision_wifi.py /dev/ttyUSB0 network-name.txt passphrase.txt 192.0.2.2/24 192.0.2.1 192.0.2.1
```

Useful options: `--user NAME` picks another login account (default
`administrator`) and `--already-root` skips the login when the shell on the
port already runs as root. See `--help` of each tool for the full list.
