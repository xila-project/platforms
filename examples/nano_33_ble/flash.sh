#!/usr/bin/env bash
# Flash a firmware ELF to an Arduino Nano 33 BLE through its bootloader.
#
# Usage: flash.sh FIRMWARE.elf [PORT]
#
# Needs Arduino's fork of bossac (1.9.1-arduino2: the mainline one does not know
# the nRF52840) and an objcopy that reads ARM ELF files. The board must run
# Xila or an Arduino sketch, or sit in its bootloader (double-tap reset).
set -euo pipefail

elf="${1:?usage: flash.sh FIRMWARE.elf [PORT]}"
port="${2:-/dev/ttyACM0}"
bossac="${BOSSAC:-bossac}"

# USB identifier of the board in its bootloader (boards.txt of ArduinoCore-mbed).
bootloader="2341:005a"

objcopy=""
for candidate in rust-objcopy llvm-objcopy arm-none-eabi-objcopy; do
  if command -v "$candidate" >/dev/null; then
    objcopy="$candidate"
    break
  fi
done
if [[ -z "$objcopy" ]]; then
  echo "No objcopy found: install cargo-binutils (rust-objcopy) or LLVM." >&2
  exit 1
fi

image="$(mktemp --suffix=.bin)"
trap 'rm -f "$image"' EXIT
"$objcopy" -O binary "$elf" "$image"
echo "Image: $(stat -c %s "$image") bytes"

in_bootloader() {
  lsusb -d "$bootloader" >/dev/null 2>&1
}

if ! in_bootloader; then
  # The "1200 bps touch" of Arduino: open the port at 1200 baud and close it.
  echo "Asking the board to enter its bootloader"
  stty -F "$port" 1200 hupcl
  for _ in $(seq 1 50); do
    in_bootloader && break
    sleep 0.2
  done
fi
if ! in_bootloader; then
  echo "The board is not in its bootloader: double-tap its reset button and run again." >&2
  exit 1
fi
# The time for the serial port of the bootloader to appear.
sleep 1

# bossac takes the port without /dev/, and addresses the application from 0:
# the bootloader adds the 0x10000 it occupies. Pages the image does not cover,
# the file system among them, are left as they are.
"$bossac" --port="$(basename "$port")" -U -i -e -w -v "$image" -R
