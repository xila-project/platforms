# Espressif boards

Each folder is one board: a small binary crate that sets up its chip, pins and
drivers, describes the board, and calls `firmware_espressif::run`. The shared
firmware in [`firmware/espressif`](../../firmware/espressif) runs Xila's task
manager, virtual file system, serial console and command-line shell over the
`drivers/espressif` runtime drivers (executor, time, serial console, memory,
random and power), backed by LittleFS in internal flash.

| Board | Chip | Flash | Console | Checked by |
|---|---|---|---|---|
| [`esp32-s3-qemu`](esp32-s3-qemu) | ESP32-S3 | 4 MB | UART0 | `mise run test:esp32-s3-qemu` (Espressif QEMU) |
| [`esp32-c6-qemu`](esp32-c6-qemu) | ESP32-C6 | 4 MB | UART0 | `mise run test:esp32-c6-qemu` (Espressif QEMU) |
| [`esp32-p4-function-ev-board`](esp32-p4-function-ev-board) | ESP32-P4 | 16 MB | UART0 | `mise run build:esp32-p4-function-ev-board` |
| [`wt32-sc01-plus`](wt32-sc01-plus) | ESP32-S3 | 16 MB | USB Serial/JTAG | `mise run build:wt32-sc01-plus` |

Each board crate:

- selects exactly one chip feature of esp-hal, `xila-drivers-espressif`,
  esp-storage and the bootloader crate in its own `Cargo.toml`;
- keeps its partition table, `partitions.csv`, beside it. Its build script
  (`build_support.rs`) passes the LittleFS partition of that table to the
  firmware, so the table and the firmware cannot disagree;
- provides its panic handler and its application descriptor.

`mise run build:BOARD` builds a board, copies its firmware to
`target/espressif-emulator/BOARD.elf` and merges a flash image,
`target/espressif-emulator/BOARD-flash.bin`. To add a board, add a folder
with these files, a `build:BOARD` task in `mise.toml` and a CI matrix entry.

Default account creation requires a secure entropy source and a freshly
formatted file system. If you keep the account-free file system from the old
boot-only firmware, it will stay account-free after upgrading. Back up any
files you need before using a fresh image to create the default account on
hardware with an entropy source.

## Build and flash the ESP32-P4 firmware

Only chip revisions **v3.0 and later** are supported by `esp-hal` 1.2.2.
The image assumes **16 MiB of flash**, as on the ESP32-P4X-Function-EV-Board (chip revision v3.1 or later, 32 MiB of PSRAM). The older ESP32-P4-Function-EV-Board carries pre-v3 silicon, which this image refuses to boot.
Check the chip revision and flash capacity on your board before flashing.
The firmware adds detected external pseudo-static memory (PSRAM) to the
internal heap and keeps LittleFS in the 12.8 MB `storage` partition of
`esp32-p4-function-ev-board/partitions.csv`.
Because [`esp-hal` 1.2.2 cannot enable the onboard ESP32-P4 entropy source][entropy],
nonempty reads of `/devices/random/source` return `DeviceUnavailable`.
The firmware skips default account creation on a fresh file system. A fresh
board reaches the login prompt but has no account to sign in with. Existing
accounts can still authenticate with their stored password salts. A future
firmware with a secure entropy source will also need to handle initial account
setup on boards that already booted this image.

From the repository root on Linux with Clang and unzip installed:

```sh
mise run build:esp32-p4-function-ev-board
```

This builds for `riscv32imafc-unknown-none-elf` with the pinned Rust
toolchain and uses the pinned espflash to merge the bootloader, partition
table, and firmware into `target/espressif-emulator/esp32-p4-function-ev-board-flash.bin`.
Build with `--features native-dev` on the board crate to add the developer-only
handler for raw ELF executables.

Connect the board's programming port to your computer. On the
[ESP32-P4X-Function-EV-Board][board], use the USB Serial/JTAG port.
If automatic entry into download mode fails, hold **BOOT**, press and
release **RESET**, then release **BOOT**. Set `FLASH_PORT` to the device
path of the programming port, then flash the executable produced by
the build task:

```sh
mise run run:espflash -- flash --chip esp32p4 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode dio \
  --partition-table examples/boards/esp32-p4-function-ev-board/partitions.csv --partition-table-offset 0x8000 \
  target/espressif-emulator/esp32-p4-function-ev-board.elf
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
shell login prompt. Continuous integration builds the flash image.

## Build and flash the WT32-SC01 Plus firmware

The `wt32-sc01-plus` board is Wireless-Tag's WT32-SC01 Plus: 16 MB of flash and 2 MB of quad SPI PSRAM, which the firmware
adds to the heap. The board's USB-C port is wired to the chip's built-in USB
Serial/JTAG controller (USB identifier 303a:1001), with no USB to UART bridge,
so the console, the logger and panic messages use that controller instead of
UART0. At boot, a self-test allocates 1 MiB from the heap and checks that it
lands in PSRAM and keeps its contents.

The 3.5-inch 480 × 320 ST7796 panel runs on the 8-bit 8080 bus of the chip's
LCD_CAM peripheral, with DMA, through `mipidsi`: each LVGL flush is one
`set_pixels_raw` call, one address window and one DMA transfer of LVGL's draw
buffer (40 rows, in internal SRAM, rendered in the panel's byte order). The
FT6336U touch panel is polled over I²C, and the backlight is on GPIO45. Two
sessions run side by side, each with its own login: the graphical shell on the
screen and the command-line shell on the USB console. The firmware mounts the
WebAssembly runtime at `/applications/executables/wasm` with lazy function
translation by default. The pins come from Wireless-Tag's datasheet and are
listed in `src/firmware/screen.rs`.

From the repository root, after `mise run setup:espressif-rust`, install the
locked npm packages once: Xila's graphics converts its fonts at build time
with the `lv_font_conv` they provide, from `node_modules` at the repository
root. Then build:

```sh
npm ci
mise run build:wt32-sc01-plus
```

This writes the firmware to `target/espressif-emulator/wt32-sc01-plus.elf` and
a merged 16 MiB flash image, `target/espressif-emulator/wt32-sc01-plus-flash.bin`.

**The merged image overwrites the stored data.** It spans the whole flash,
including the erased `storage` partition of
[partitions.csv](wt32-sc01-plus/partitions.csv), where the LittleFS file system
lives. A raw write of it at offset zero replaces that partition: the accounts,
the installed applications and every stored file are lost. Use it only to
provision a fresh or wiped board. To upgrade a board that already has the
current partition layout and keep its storage, flash the ELF instead (it writes
the boot loader, the partition table and the application, never the `storage`
partition), or write only the application (below). A board with an older
partition layout does not keep its storage this way: see the migration below.
Connect the board's USB-C port and set `FLASH_PORT` to its device path
(`/dev/ttyACM0` on Linux).

Flash the ELF with the board's partition table:

```sh
mise run run:espflash -- flash --chip esp32s3 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode qio --flash-freq 80mhz \
  --partition-table examples/boards/wt32-sc01-plus/partitions.csv --partition-table-offset 0x8000 \
  --monitor target/espressif-emulator/wt32-sc01-plus.elf
```

With espflash's bundled boot loader, the ELF command and the merged image
write the same headers: DIO at 80 MHz for the second-stage boot loader, which
the ROM requires and which switches the flash chip to quad I/O, and QIO at
80 MHz for the application. A boot loader that you configured locally in
espflash can bypass the selection of the bundled QIO boot loader.

To write only the application, to the `factory` partition, and leave the
installed boot loader, the partition table and the `storage` partition
untouched, on a board whose partition table already matches
[partitions.csv](wt32-sc01-plus/partitions.csv), build the application image and
write it at the partition's offset, read from the partition table:

```sh
factory_offset=$(awk -F ',' '$1 == "factory" { gsub(/ /, "", $4); print $4 }' \
  examples/boards/wt32-sc01-plus/partitions.csv)
mise run run:espflash -- save-image --chip esp32s3 \
  --flash-size 16mb --flash-mode qio --flash-freq 80mhz \
  target/espressif-emulator/wt32-sc01-plus.elf \
  target/espressif-emulator/wt32-sc01-plus-application.bin
mise run run:espflash -- write-bin --chip esp32s3 --port "$FLASH_PORT" \
  "$factory_offset" target/espressif-emulator/wt32-sc01-plus-application.bin
```

An application-only update keeps the flash mode of the boot loader that is
already installed, so a board that was flashed with a DIO boot loader needs one
`espflash flash` of the ELF, as above, to install the QIO boot loader.

The CPU runs at 240 MHz, where esp-hal starts the ESP32-S3 at 80 MHz, and the
flash and the PSRAM at 80 MHz, the flash in quad I/O mode.

The boot benchmark is opt-in, so that normal boots stay fast and quiet. To
include it, pass the board crate's `boot-benchmark` feature to the build, then
flash as above:

```sh
mise run build:wt32-sc01-plus -- --features boot-benchmark
```

After the PSRAM self-test, the `Xila: boot benchmark` lines give the clocks,
the flash read mode, and the time of a CPU loop (1,000,000 iterations), of a
fixed 256 KiB flash read and of a 512 KiB PSRAM write and read. The memory
lines report the byte count and the throughput. The flash read covers a
read-only buffer that the benchmark links into every build with the feature,
read from a cold cache, so the figures compare between builds. The benchmark
adds the buffer's 256 KiB to the application, and it is absent from the default
build.

The first boot formats the LittleFS partition, which has 3056 blocks of 4096 bytes. Look for the PSRAM
size, `Xila: PSRAM self-test passed`, `Xila: LittleFS mounted`, the graphics
and touch controller lines, the heap report and the `User name:` prompt.
Once the shell session starts, the console shows only warnings and errors from
the log, a few per second at most, so that they do not bury the session.

To seed the `storage` partition with the native application shortcuts
(Activity, File manager, Settings and Terminal), the official package registry,
and the Calculator, Clock, Digits, Pixel Painter, Synth, Text Editor and Weather
WebAssembly application bundles, build the raw
4096-byte-block LittleFS image. The task reads the image size from the
`storage` row of [partitions.csv](wt32-sc01-plus/partitions.csv).

Until format-v2 releases are pinned, build the WebAssembly guests from the
locked Applications revision, package them with `tools/bundle_applications.sh`,
and set `XILA_BUNDLES_DIR` to the resulting bundle directory. See the
[seed image documentation](../../tools/fs_image/README.md) for the bundle inputs.
Digits uses the neural network loader at `/devices/nn/cpu`. The firmware
supports fully connected and softmax operators, with ESP-NN available for
quantized models. It runs the current
floating-point Digits model with Nanoglia’s portable kernels. Weather requires
a Wi-Fi connection.
Writing the image at the partition offset replaces its existing contents.

**Migration from the 3 MiB factory layout.** Back up your storage data before
upgrading. Reflash the partition table with the firmware command above, then
write the storage image at the new offset below or let the firmware format the
partition on first boot. This migration erases the existing storage data.

```sh
XILA_BUNDLES_DIR=/path/to/bundles mise run image:wt32-sc01-plus
mise run run:espflash -- write-bin --chip esp32s3 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode dio 0x410000 target/seed/wt32-sc01-plus/storage.img
```

The seeded image carries the first-installation marker so the firmware creates
the default `administrator` account (empty password) on first boot without
reformatting the partition.

[board]: https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32p4/esp32-p4x-function-ev-board/user_guide.html
[entropy]: https://github.com/esp-rs/esp-hal/issues/5982
