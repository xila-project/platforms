# Espressif ESP32 example

The bare-metal firmware runs Xila's task manager, virtual file system, serial
console and command-line shell over the `drivers/espressif` runtime drivers
(executor, time, serial console, memory, random and power), backed by LittleFS
in internal flash. The ESP32-S3 emulator build also checks `/devices/random/source`
and renders a frame with LVGL.

## Build and flash the ESP32-P4 firmware

Only chip revisions **v3.0 and later** are supported by `esp-hal` 1.2.2.
The image assumes **16 MiB of flash**, as on the ESP32-P4X-Function-EV-Board (chip revision v3.1 or later, 32 MiB of PSRAM). The older ESP32-P4-Function-EV-Board carries pre-v3 silicon, which this image refuses to boot.
Check the chip revision and flash capacity on your board before flashing.
The firmware adds detected external pseudo-static memory (PSRAM) to the
internal heap and uses the existing LittleFS partition in `partitions.csv`.
Because `esp-hal` 1.2.2 cannot enable the onboard ESP32-P4 entropy source yet,
`/devices/random/source` returns unavailable and default account seeding is
skipped until an entropy source is configured.

From the repository root on Linux with Clang and unzip installed:

```sh
mise run build:esp32p4-image
```

This builds for `riscv32imafc-unknown-none-elf` with the pinned Rust
toolchain and uses the pinned espflash to merge the bootloader, partition
table, and firmware into `target/espressif-emulator/esp32p4-flash.bin`.

Connect the board's programming port to your computer. On the
[ESP32-P4X-Function-EV-Board][board], use the USB Serial/JTAG port.
If automatic entry into download mode fails, hold **BOOT**, press and
release **RESET**, then release **BOOT**. Set `FLASH_PORT` to the device
path of the programming port, then flash the executable produced by
the build task:

```sh
mise run run:espflash -- flash --chip esp32p4 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode dio \
  --partition-table examples/esp32/partitions.csv --partition-table-offset 0x8000 \
  target/riscv32imafc-unknown-none-elf/release-embedded/esp32_example
```

Firmware messages and the command-line shell use UART0 at 115200 baud. On
boards that expose UART0 on the header, connect a 3.3 V serial adapter's
receive line to GPIO37, its transmit line to GPIO38, and its ground to board
ground. Set `CONSOLE_PORT` to the adapter's device path, start the monitor,
and reset the board to see the startup messages and shell login prompt:

```sh
mise run run:espflash -- monitor --chip esp32p4 --port "$CONSOLE_PORT"
```

Look for `Xila: LittleFS mounted`, the heap report, and the `User name:`
shell login prompt. The ESP32-P4 continuous integration job builds the flash
image. The ESP32-S3 and ESP32-C6 boot tests run in QEMU and check for the
`User name:` prompt.

[board]: https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32p4/esp32-p4x-function-ev-board/user_guide.html
