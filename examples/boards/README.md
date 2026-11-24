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
| [`wt32-sc01`](wt32-sc01) | ESP32 | 4 MB | UART0 via CP210x | `mise run build:wt32-sc01` |
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

To compile and copy only the ELF, run
`bash tools/dev/build_espressif_image.sh ESPFLASH_VERSION RUST_VERSION ESP_GCC_VERSION BOARD elf`
from the Platforms root. Supply the version pins from `mise.toml`. The script
uses the installed compiler and skips image packaging.

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
FT6336U touch panel is polled over I²C, and the backlight is on GPIO45,
dimmed by a 25 kHz LEDC PWM signal (see "Backlight brightness" below). Two
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

Add `-- --features debug-log` for debug-level console logging with the display
summary described for the [WT32-SC01](#build-and-flash-the-wt32-sc01-firmware).
The theme keeps its default rendering mode on this board: its bus sends a
whole frame in 15 ms against 61 ms, and its frame times have not been measured
yet.

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
Writing the image at the partition offset replaces its existing contents.

Digits uses the neural network loader at `/devices/nn/cpu`. Nanoglia's portable
kernels run the model's floating-point fully connected and softmax operators.
The 102,644-byte model needs about 201 KiB of PSRAM during loading and 100 KiB
while open. The guest keeps its own 100 KiB model copy in WebAssembly linear
memory and reserves a 64 KiB stack there. With tensor and guest runtime memory,
budget roughly 300 KiB steady and 400–450 KiB during loading. These are estimates
from the allocation sizes. The loader caps the staging buffer at 256 KiB.
Weather requires a Wi-Fi connection.

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

**Single-application device.** A board can boot directly into one application
instead of the graphical shell, a clock or a dashboard for example, when its
storage holds `/configuration/system/startup.json` (see
[the startup application](../../firmware/startup_application/README.md)). The
firmware stays the same. The `wt32-sc01-plus-clock` profile builds a storage
image with the Clock as the startup application, run by `administrator`, and
without the other WebAssembly applications:

```sh
XILA_IMAGE_PROFILE=wt32-sc01-plus-clock XILA_BUNDLES_DIR=/path/to/bundles mise run image:wt32-sc01-plus
mise run run:espflash -- write-bin --chip esp32s3 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode dio 0x410000 target/seed/wt32-sc01-plus-clock/storage.img
```

The Clock then fills the screen with no login, status bar or dock, and starts
again whenever it ends. The serial console session keeps running for
maintenance. If the configuration is invalid or the application cannot start,
the console shows why and the graphical shell starts instead. Boards with a
touch display get Core's system on-screen keyboard in this mode too, for the
startup application's text fields.

[board]: https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32p4/esp32-p4x-function-ev-board/user_guide.html
[entropy]: https://github.com/esp-rs/esp-hal/issues/5982

## Build and flash the WT32-SC01 firmware

The classic Wireless-Tag WT32-SC01 uses the ESP32-WROVER-B module and runs
the CPU at 240 MHz. It starts the command-line shell on UART0 at 115200 baud
through the CP210x bridge and starts the graphical shell when the display
initializes.
The 192 KiB runtime heap comprises 96 KiB in primary DRAM and 96 KiB in
DRAM2 reclaimed after ESP-IDF bootloader handoff. The rest of primary DRAM
holds static data and the main stack (about 77 KiB; the link fails below
40 KiB, see [main_stack.x](wt32-sc01/main_stack.x)). The ESP32 can
directly map 4 MiB of the module's 8 MiB of PSRAM. The firmware adds that
mapping to the heap as region 2: allocations of 4 KiB or more go there first,
and smaller ones only when the internal regions are full. See the
[Wireless-Tag datasheet][classic-board], [Espressif module datasheet][wrover]
and [ESP32 external RAM guide][external-ram].

PSRAM on this chip needs the measures that ESP-IDF takes:

- **Cache erratum (revision v1).** A write to PSRAM followed closely by a read
  of the same address can be lost ([erratum CPU-3.9][esp32-errata]). All code
  is built with the compiler workaround: Rust through the target's Rust flags
  in `.cargo/config.toml`, and C (LittleFS, LVGL) through
  `-mfix-esp32-psram-cache-issue` in `tools/dev/build_espressif_image.sh`.
  The ROM's `memcpy`, `memmove` and `memset` are not built with it, so calls
  that write to PSRAM run loops of `xila-drivers-espressif`
  (`src/rom_memory.rs`) in their place.
- **Atomic operations.** Espressif does not support the compare-and-set
  instruction (S32C1I) on PSRAM. One uncontended instruction gives the right
  result on this board, but nothing assures that it is atomic there, and a
  race would raise no exception. The target disables the `s32c1i` feature and
  the C code is built with `-mdisable-hardware-atomics`, so every atomic
  operation calls a function of `xila-drivers-espressif` (`src/atomics.rs`)
  that uses the instruction for internal memory and a critical section for
  PSRAM. The build fails if the instruction appears anywhere else in the
  firmware.
- **DMA and flash.** SPI DMA cannot read PSRAM: the draw buffers are allocated
  in internal SRAM, and the display bus sends any other buffer through the
  FIFO. The flash routines get a transfer buffer on the stack, and the stacks
  are internal.

At boot the PSRAM self-test checks a 1 MiB allocation and readback, the atomic
operations on values in PSRAM, fills and copies, and a reference-counted
allocation in PSRAM.

The graphical shell shows the login screen on the 480 × 320 ST7796S and
accepts input from the FT6336U touch panel. The boot log reports the draw buffer
size, the FT6336U identifiers and the used and free bytes in heap regions 0, 1
and 2 after graphics starts.

| Signal | ESP32 GPIO |
|---|---|
| HSPI MOSI, clock, chip select | 13, 14, 15 |
| LCD command/data, reset, backlight (active high, LEDC PWM) | 21, 22, 23 |
| FT6336U I²C data and clock | 18, 19 |
| FT6336U interrupt (polled instead) | 39 |

The [Wireless-Tag schematic][classic-schematic] and
[reference firmware pin definitions][classic-reference] document the wiring.
The display uses write-only SPI with DMA at 40 MHz (the 80 MHz APB clock
divided by two). Its 25 ns cycle is faster than the
[ST7796S timing specification][st7796s] minimum of 66 ns for writes, as is
common with this panel; it was checked on a WT32-SC01 for a clean image and
working touch. If a panel shows corrupted pixels, set
`BUS_FREQUENCY_KILOHERTZ` to `13_334` (divider six, 75 ns), the fastest
in-specification setting: esp-hal rounds the divider up, so `13_333` would run
at 11.4 MHz.
ESP32 SPI DMA requires internal SRAM, so the two RGB565 draw buffers live
there: 480 × 20 × 2 = 19,200 bytes each, 38,400 bytes in all, about 20% of
the 192 KiB heap. LVGL renders into one buffer while DMA sends the other to
the panel; panel commands go through the 64-byte SPI FIFO instead of DMA.
Invalidated areas are widened to an even number of columns so that every
transfer is a multiple of 4 bytes, which ESP32 SPI DMA needs to read the
buffer in place. LVGL refreshes the display and polls the touch panel every
16 ms.

The theme renders in Xila's light mode on this board (`light_rendering` in
`src/firmware/screen.rs`): state changes and the scrolls that LVGL animates
take one frame, and a modal backdrop does not dim what is behind it. The bus
alone needs 61 ms for a whole frame, so a 400 ms animation over most of the
screen could only show three or four of its steps, each a full redraw.

Build with `--features debug-log` to enable debug-level console logging,
including a display summary every two seconds while the screen changes:

```sh
mise run build:wt32-sc01 -- --features debug-log
```

A summary has this form (the figures here only show the format):

```text
Display performance: 5.1 frames/s (10 frames, 118 flushes, frame average 96.2 ms, maximum 190.4 ms, flush average 61.0 ms, wait average 22.3 ms; per frame 6.4 invalidations of 131520 pixels, 113280 pixels flushed; largest invalidation 139200 pixels by lv_list)
```

The times tell how long a frame takes; the last part tells why. `pixels
flushed` is what LVGL redrew, once it had joined the invalidated areas, out
of 153,600 for the whole screen. `invalidations` counts the areas as they
were invalidated, and their pixels count overlapping areas once each, so
that sum can exceed the screen. The largest invalidation of the two seconds
is named after the class of the deepest object that contains it. The
WT32-SC01 Plus has the same feature.

[classic-schematic]: https://robu-prod-media.s3.ap-south-1.amazonaws.com/uploads/2021/12/WT32-SC01DataSheetV3.4-with-nuts.pdf
[classic-reference]: https://github.com/wireless-tag-com/WT32-SC01/blob/87f3c38dcfe6ee5a3d0f32776817e7d97576f240/main/board.h
[st7796s]: https://dl.espressif.com/dl/schematics/ST7796S_SPEC_V1.0.pdf

```sh
mise run setup:espressif-rust
mise run build:wt32-sc01
```

WT32-SC01 modules carry ESP32 revision v1.0 silicon, older than esp-hal's
default minimum (v3.0), so the build lowers `ESP_HAL_CONFIG_MIN_CHIP_REVISION`
to v1.0; the PSRAM cache workaround above covers that revision's erratum.

On the 4 MiB flash, the factory application occupies 2.25 MiB at `0x10000`.
LittleFS uses the remaining 1.6875 MiB at `0x250000`.

**Migration from the 1.5 MiB factory layout.** Back up your storage data before
upgrading. Flash the firmware with the new partition table, then reflash the
storage image at the new offset. Without a storage image, the firmware cannot
mount the moved partition and formats it on first boot. Restore any saved files
after the upgrade.

```sh
espflash flash --chip esp32 --port /dev/ttyUSB0 \
  --flash-size 4mb --flash-mode dio \
  --partition-table examples/boards/wt32-sc01/partitions.csv --partition-table-offset 0x8000 \
  --monitor target/espressif-emulator/wt32-sc01.elf
```

Build and write the raw LittleFS image to populate the application menu with
Activity, File Manager, Settings and Terminal. The task reads the image size
from the `storage` row in [partitions.csv](wt32-sc01/partitions.csv).
The classic firmware does not enable the WebAssembly runtime (the firmware's
`wasm` feature), so its seed image contains only native application shortcuts
and no WebAssembly bundles. Writing the storage image replaces the partition contents.

```sh
mise run image:wt32-sc01
mise run run:espflash -- write-bin --chip esp32 --port /dev/ttyUSB0 \
  --flash-size 4mb --flash-mode dio 0x250000 target/seed/wt32-sc01/storage.img
```

Look for `Xila: board WT32-SC01`, `Xila: PSRAM 4194304 bytes (registered: true)`,
`Xila: PSRAM self-test passed`, `Xila: PSRAM atomics self-test passed`,
`Xila: PSRAM copy self-test passed`,
`Xila: PSRAM shared allocation self-test passed`, `Xila: LittleFS mounted`,
`Random generator works`, the two internal heap regions and the PSRAM
region, and `Xila: main stack high-water … bytes (estimate)` before the `User name:`
prompt. The estimate covers execution from early application startup through
the shell's first console output. On first boot, the seeded or freshly formatted
file system creates the default `administrator` account with an empty password.
The display and touch bring-up is tracked in [issue #177][classic-issue].

[classic-board]: https://files.seeedstudio.com/products/102991455/WT32-SC01-datasheet.pdf
[wrover]: https://documentation.espressif.com/esp32-wrover-b_datasheet_en.html
[external-ram]: https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/external-ram.html
[esp32-errata]: https://docs.espressif.com/projects/esp-chip-errata/en/latest/esp32/03-errata-description/index.html
[classic-issue]: https://github.com/xila-project/platforms/issues/177

## Backlight brightness

Both WT32-SC01 boards drive their backlight pin with the LED controller
(LEDC, low-speed timer 0 and channel 0) at 25 kHz with a 10-bit duty: above
the audible range and far above any visible flicker.
`firmware::backlight::attach` wraps the panel's screen device in the shared
`BacklightScreenDevice`, which implements the brightness commands of Core's
screen contract. The backlight stays off until the panel holds a black frame,
then starts at full brightness; restoring the saved appearance at boot and at
login then sets the user's level from Settings, which never goes below about
10 %. Other boards, and the native and browser examples, report brightness as
unsupported, and Settings hides its slider there.
