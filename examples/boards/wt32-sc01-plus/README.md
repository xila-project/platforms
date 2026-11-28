# WT32-SC01 Plus Wi-Fi

The board starts its ESP32-S3 station in 2.4 GHz mode and exposes
`/devices/network/wlan0`. It can scan and associate with open and WPA2 Personal
networks. The Settings Wi-Fi panel can turn the radio on and off, scan, connect
and disconnect when the graphical firmware includes Settings. Core owns
Ethernet frame processing, IP addressing and DNS.

## USB console and human interface emulation

The USB-C port is wired to the chip's USB pins (D- on GPIO 19, D+ on GPIO 20),
with no USB to UART bridge. The ESP32-S3 has one USB PHY, shared by the USB
Serial/JTAG controller and the USB OTG controller, so the firmware uses one of
them:

- Default build: the console, the logger and panic messages use the USB
  Serial/JTAG controller, which the host sees as `303a:1001`. espflash can
  reset the board into download mode over this controller.
- With the `usb-human-interface` feature: the PHY is given to the USB OTG
  controller, and one composite USB device carries both the console (CDC-ACM)
  and the human interface emulation (HID). The device then acts as a USB
  keyboard, media remote, mouse and system control toward the computer it is
  plugged into, and applications drive it through
  `/devices/human_interface/emulation`.

Build as usual, with the feature:

```sh
mise trust
mise run build:wt32-sc01-plus -- --features usb-human-interface
```

Or build the firmware ELF only (as CI does):

```sh
mise run check:wt32-sc01-plus-usb-human-interface
```

Flashing a firmware built with the feature needs the BOOT button: hold BOOT,
press and release RESET, then release BOOT. espflash enters download mode over
the USB Serial/JTAG controller, and that controller no longer answers while
this firmware runs, so the automatic reset-into-download-mode does not work.
Then flash the ELF with the board's partition table, like any other WT32-SC01
Plus firmware (see [the board README](../README.md)); the ELF writes the boot
loader, the partition table and the application, never the `storage` partition,
so the accounts and files survive the update:

```sh
export FLASH_PORT=/dev/ttyACM0   # the port the board's USB-C shows as, in download mode
mise run run:espflash -- flash --chip esp32s3 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode qio --flash-freq 80mhz \
  --partition-table examples/boards/wt32-sc01-plus/partitions.csv --partition-table-offset 0x8000 \
  --monitor target/espressif-emulator/wt32-sc01-plus.elf
```

Do not flash the merged `wt32-sc01-plus-flash.bin` at offset zero on a board
that already has data: it spans the whole flash and replaces the `storage`
partition. To go back to the default firmware, flash it the same way (BOOT held
at reset); the USB Serial/JTAG controller answers again and espflash's
auto-reset works, and the host sees `303a:1001` again.

The USB identifiers of the composite device are the test pair of pid.codes
(`1209:0001`), the same as the Nordic USB console, with manufacturer `Xila`
and product `Xila USB console and human interface`.

Limits of the emulation:

- The HID descriptor does not declare the boot protocol, so the keyboard does
  not work in a BIOS, UEFI or a bootloader, only in a running operating system.
- Remote wake-up is not supported: the device cannot wake a suspended computer.
- The console transmits only while the host asserts DTR on the CDC-ACM port,
  that is, while a terminal program has the port open. Output queued before that
  is sent when the port opens.
- Panic messages are not visible while the feature is on: the panic handler
  writes to the USB Serial/JTAG controller, whose PHY the OTG controller owns.

Check the enumeration:

- Linux: `lsusb -d 1209:0001` lists the composite device, and
  `lsusb -d 1209:0001 -v` shows the CDC-ACM and HID interfaces. The console
  appears as `/dev/ttyACM0`. HID input events appear on an `event` node of
  `/dev/input/by-id/`: list them with `evtest`, then press a key of a shortcut
  to see the events. `ls /devices/human_interface` on the board lists
  `emulation`. ModemManager may probe the test identifier and claim the
  CDC-ACM port; stop it with a udev rule, for example
  `/etc/udev/rules.d/99-xila-test-usb.rules` containing
  `ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", ENV{ID_MM_DEVICE_IGNORE}="1"`,
  then `udevadm control --reload-rules`.
- Windows: Device Manager shows a USB composite device with a USB serial
  device (COM port) and a HID keyboard, mouse and consumer control. The
  console opens on the COM port with any terminal program at 115200 baud.
- macOS: System Information, USB, shows the composite device. The console
  appears as `/dev/tty.usbmodem*`; open it with `screen /dev/tty.usbmodem* 115200`.

Drive the emulation from the board's shell by writing 8-byte messages to
`/devices/human_interface/emulation` (kind 1 keyboard, 2 consumer control,
3 mouse, 4 system control; see Core's `devices/src/human_interface.rs`). For
example, a consumer play/pause message is the 8 bytes `02 cd 00 00 00 00 00 00`,
followed by the release `02 00 00 00 00 00 00 00`. A keyboard message holds the
full pressed state, so send the release `01 00 00 00 00 00 00 00` after a key
press; for example left shift + A is `01 02 00 04 00 00 00 00` and its release
is the all-zero keyboard message. The `GET_STATUS` control command of kind
`b'h'` returns whether a computer is connected and the keyboard LED state. On
the computer, the keyboard LED state (num lock, caps lock, scroll lock) comes
back through `indicators()`.

Hardware validation of the HID reports is open: write 8-byte messages to
`/devices/human_interface/emulation` from the shell and check the key presses,
media keys and mouse moves on the computer with `evtest` on Linux.

## Station profile

A successful connection requested by Settings or another client saves
`/configuration/system/wifi` in LittleFS as root (user ID 0) with mode
`0600`. An unsuccessful attempt leaves the previous profile in place. The
firmware uses the saved profile at the next boot. With no profile, the radio
starts idle and can scan. An explicit disconnect stops retries for the current
radio session. A failed attempt or lost link retries after 2, 4, 8, 16 and
32 seconds, then at intervals of up to 60 seconds. Turning the radio off ends
association, retries and scanning after the current operation completes.

For a manual profile, use the following lines in order. The file accepts LF
or CRLF and at most 160 bytes. Keep credentials in a private file outside the
checkout and out of shell commands and console transcripts.

1. Network name, 1 to 32 bytes, without carriage returns, line feeds or NULs.
2. WPA2 passphrase, 8 to 63 printable ASCII bytes, or a 64-digit hexadecimal
   key. Supply an empty line for an open network.
3. Optional static IPv4 address and prefix, in the form `192.0.2.10/24`.
4. IPv4 gateway, in the form `192.0.2.1`.
5. IPv4 DNS server, in the form `192.0.2.2`.

Use all three address lines together and substitute addresses for the actual
test network. Omitting the address lines enables Core's DHCP client, which
applies the leased address, default route and DNS server. Saving credentials
for the same network name on the device keeps the existing static IPv4
settings. Joining a different network name removes them
and switches to DHCP, so the saved profile has only the first two lines.
Switching between different DHCP network names clears the previous address,
route and DNS lease before reporting the new link up. Reconnecting to the
same network name keeps the DHCP client and its lease. Only the name is
compared, so moving between two access points that share a name but serve
different subnets keeps the old lease until the board reboots.

To restore static addressing after switching networks, provision the
five-line profile for the desired network and reboot. The firmware reads the
profile's address settings at startup. Returning to the network through
Settings continues to use DHCP.

## Time synchronization

When Wi-Fi is enabled, the firmware sets the UTC clock from the network with
SNTP (UDP port 123) once an interface has an address, then resynchronizes
every hour. Until the first success, the clock starts from the build date, and
answers before the build date are rejected. Native and browser builds keep
taking UTC from the host.

The servers come from `/configuration/system/sntp.json`, a JSON object written
by root only (root ownership, and neither the group nor others may write it),
for example:

```json
{
  "servers": ["time.example.com", "192.0.2.1"],
  "resync_interval_seconds": 3600
}
```

`servers` is a list of at most 3 host names or addresses, each at most 253
bytes. `resync_interval_seconds` is clamped to 60 s up to 7 days. Without the
file, the firmware queries `pool.ntp.org` every hour: that name is a
placeholder only, because the NTP Pool asks vendors not to ship the standard
`pool.ntp.org` names as a default configuration; a vendor zone such as
`xila.pool.ntp.org` must be requested from the NTP Pool before release. An
absent or invalid file falls back to the defaults with one warning in the log.

## Hardware smoke test

1. Boot without a profile. Check for `Wi-Fi: wlan0 registered`,
   `Wi-Fi: radio on; internal heap free before startup …, after startup … bytes`
   and the login prompt. Check that `ip address` shows `wlan0`, link down,
   with the station MAC from the board's eFuse.
2. On a graphical image with Settings, open Network → Wi-Fi, scan, select a
   2.4 GHz test network and connect. Verify the connection state, channel and
   signal strength. Reboot to check that the system remembered the network.
   For a console-only image, provision the root-owned profile privately in
   LittleFS and reboot. A valid profile logs `Wi-Fi: station profile loaded`.
3. For end-to-end traffic, use either a DHCP profile or the five-line static
   profile. Check that `ip address` shows `Enabled, Up` and the leased or
   selected IPv4 address. Check the gateway with `ip route`. Run `ping -c 4 <gateway>`, ping the board from a peer and run
   `dns_resolve <known hostname>`.
4. Power off the test access point and watch for `Wi-Fi: link lost (…)`.
   Bring it back and verify that the board reconnects without a reboot. Use
   Settings to disconnect and verify link down. Toggle the radio off and on,
   then scan again. With a wrong passphrase, check for
   `Wi-Fi: connection failed (authentication); retry scheduled` or the timeout
   or access-point error reported by the radio. Check shared captures for
   credential leakage.

The firmware records a startup marker before initializing the physical radio.
A timer-group watchdog covers initialization and the first saved-profile
association, including a vendor stall. If startup is interrupted, the next
boot leaves the radio off and logs
`Wi-Fi: previous startup was interrupted; radio stays off`. An explicit radio
enable in Settings retries startup. ESP32-S3 QEMU has no emulated radio. Its
Wi-Fi-enabled CI build reads a valid profile, reports radio unavailability
and reaches login.

## Internal SRAM

esp-radio 1.0.0-beta.1 and esp-rtos 0.4.0 use esp-hal 1.2.2. The scheduler uses
TIMG0 timer 0 and software interrupt 0. The Xila executor remains on the first
core and keeps SYSTIMER alarm 0. The radio's C hooks and scheduler task stacks
require cache-accessible internal SRAM. Rust allocations made by radio
threads, interrupts and calls into the radio library also use internal SRAM.
Normal Xila thread allocations prefer the 2 MiB PSRAM when it is available.

The Wi-Fi firmware reserves 188 KiB (192,512 bytes) for the internal heap.
The board linker enforces a 40 KiB main stack. LittleFS copies flash transfers
through an internal SRAM buffer so the ROM can access them with caches suspended.
The radio caps frame buffers at six static receive, twelve dynamic receive
and eight dynamic transmit. At about 1.6 KB per frame they can account for
roughly 42 KB, plus scheduler stacks, management frames and WPA state.
Startup checks for 96 KiB of free internal heap. Record the startup heap
counters and the memory report at login, then measure under traffic.

Core's synchronous smoltcp device API offers no receive wake callback. The
Core runner polls at least every 20 ms while idle, and the driver raises the
radio receive queue from five to twelve frames to absorb bursts. An
interrupt-driven receive path needs a wake interface from Core.

## Bluetooth Low Energy

The optional `bluetooth` feature mounts `/devices/bluetooth/hci0` for root.
Build the firmware and flash its ELF using the board partition table.

```sh
mise run build:wt32-sc01-plus -- --features bluetooth
export FLASH_PORT=/dev/ttyACM0 # Set this to the board's flash port.
mise run run:espflash -- flash --chip esp32s3 --port "$FLASH_PORT" \
  --flash-size 16mb --flash-mode qio --flash-freq 80mhz \
  --partition-table examples/boards/wt32-sc01-plus/partitions.csv --partition-table-offset 0x8000 \
  --monitor target/espressif-emulator/wt32-sc01-plus.elf
```

The ESP32-S3 uses one radio for Wi-Fi and Bluetooth. The board starts the
Bluetooth controller during the first successful Wi-Fi radio startup. Both
controllers use the same scheduler, and esp-radio's coexistence scheduler
shares airtime between them. If the startup marker leaves Wi-Fi off at boot,
enable Wi-Fi in Settings to start Bluetooth. After startup, the Bluetooth
controller keeps its radio reference when Wi-Fi is turned off.

1. Turn on a nearby device that sends legacy Bluetooth LE advertisements.
2. Log in to the console as an administrator. Start a root shell with
   `sudo command_line_shell`, then run `bluetooth_scan`. The command opens
   the Bluetooth device, listens for five seconds, and prints advertiser
   addresses, address types and received signal strength in dBm. Check for
   the advertiser and the `Bluetooth scan complete` message.
3. Connect the board to a 2.4 GHz Wi-Fi network. Run `ip address` on the board,
   then run `ping <board-address>` from another machine on the network. Run
   `bluetooth_scan` again in the root shell. Check that the advertiser
   appears and that the board responds to pings during the scan.

### Memory use

These sizes come from the board's release ELF files. All sizes are in bytes.

| Measurement | Bluetooth off | Bluetooth on |
|---|---:|---:|
| Linked flash payload | 3,912,200 | 4,129,928 |
| Static internal SRAM, including the heap reservation | 295,472 | 319,084 |
| Main stack capacity | 46,288 | 47,248 |
| Static PSRAM | 0 | 0 |

The flash payload excludes image headers and alignment padding. The board
moves 24 KiB of its 188 KiB internal heap into SRAM reclaimed from the
bootloader. This keeps the main stack above 40 KiB.

The controller requests 8,192 bytes for its scheduler stack and 10,752 bytes
for its packet buffer payloads in internal SRAM. The Bluetooth device and its
host task future account for 10,368 bytes of PSRAM when PSRAM is available.
These figures exclude allocator overhead, other controller allocations, and
transient scan and connection data.

The boot log reports the free internal heap immediately before and after
Bluetooth controller startup. Compare the internal SRAM and PSRAM heap
reports at login on builds with and without `bluetooth`, then repeat under
Wi-Fi traffic while scanning to measure runtime memory use.

## microSD card

The slot takes a microSD card. The firmware reads the card in native SD mode,
through the SDHOST controller, with a 1-bit bus at 20 MHz. The card holds a
LittleFS file system with 4096-byte blocks (eight sectors of 512 bytes). The
firmware supports no other file system on the card.

Format the card from Xila before first use. Formatting erases the card.

| Signal | GPIO |
|---|---|
| Clock | 39 |
| Command | 40 |
| Data 0 | 38 |
| Data 3 (chip select), held high | 41 |

Data 1 and data 2 are not connected, so the bus cannot be wider than 1 bit.
The firmware holds data 3 high from before the first command: a card that
sees it low at the first command selects SPI mode, which the controller does
not support.

### GPIO 38 and RS-485

The board's datasheet maps the RS-485-B connector to the same signal as data 0
of the card, GPIO 38. This firmware gives GPIO 38 to the card and does not use
the RS-485 connector. Do not connect an RS-485 device to that connector while
a card is in the slot. A future RS-485 driver must not start on a board that
has a card in the slot. The wiring comes from the datasheet and is not
measured.

### Boot and mount

- The firmware looks for a card once, at boot. The slot has no card-detect
  line, so a card inserted or removed later is not noticed before the next
  boot. Removing a card while it is in use can damage its file system.
- With no card, the boot log has one line: `Xila: SD card: none (…)`, and the
  boot continues.
- With a card, the boot log shows its type, product, capacity, bus mode and
  clock: `Xila: SD card: SDHC "…", … GiB (… sectors of 512 bytes), native SD
  mode, 1-bit bus, … kHz`.
- The card is the block device `/devices/storage/sd_card`. Only its owner
  (root) can read and write it. Do not write it while LittleFS is mounted.
- A card that holds LittleFS is mounted at `/media/sd_card`.
- The root of a new card is a shared directory, as `/data/shared`: every
  user can create files and directories in it, and only the owner of an
  entry can remove or rename it. LittleFS creates the root without
  permissions, so the firmware sets them at the first mount. It does not
  change them after that, unless the root user removes all permissions from
  the root directory: the next boot shares it again.
- A card that does not hold LittleFS is not mounted and not changed. The boot
  log says so.

### Format

The firmware never formats a card by itself. To format the card with LittleFS,
write a request file as root and restart. Only root and the members of its
group can write to `/configuration/system`. The `administrator` account
cannot.

`sudo` runs external commands only, and `echo` is a command of the shell, so
start a root shell first. In the command-line shell (the console, or the
Terminal application), type:

```
sudo command_line_shell
echo erase > /configuration/system/format_sd_card
reboot
```

At the next boot, the firmware removes the file and formats the card, whatever
it holds. A file that does not contain the text `erase` is removed without
effect. A format request is for one boot: with no card in the slot, the
firmware discards it. If the firmware cannot remove the file, it does not
format the card, so that a request cannot repeat at each boot. If the format
fails, the card stays unmounted: write the request again to retry.

This file is temporary. platforms#219 tracks a shell command that replaces it.

### Test

To write a 256 KiB file to the mounted card, read it back and compare it, type
these commands:

```
sudo command_line_shell
echo test > /configuration/system/test_sd_card
reboot
```

The boot log shows `Xila: SD card test: passed; wrote 256 KiB in … ms, read it
back in … ms`, or the reason for the failure. The firmware removes the file
`/media/sd_card/xila_test.bin` afterwards.

### Limits

- The firmware transfers one 512-byte sector at a time, and the processor
  waits for each transfer. A write can take tens of milliseconds when the card
  does internal work.
- The firmware does not send erase commands to the card. The card erases
  internally. An `ERASE` request on the block device succeeds and changes
  nothing.
- Single-sector transfers limit the speed to a fraction of the bus speed
  (2.5 MB/s for a 1-bit bus at 20 MHz). platforms#220 tracks multi-sector
  transfers.

### Tested cards

| Card | Date | Result |
|---|---|---|
| SanDisk SDXC "SN64G", 59.47 GiB (manufacturer `0x03`, revision 128, 2019-10) | 2026-11-24 | Detection, format, mount and persistence pass. Test file with 512-byte LittleFS blocks (before the change to 4096-byte blocks): 256 KiB written in 1129 ms (227 KiB/s), read in 1458 ms (176 KiB/s). With 4096-byte blocks: written in 1022 ms (250 KiB/s), read in 378 ms (677 KiB/s). The `administrator` user can list, write and read files on the card. |

## GPIO

The firmware mounts a GPIO device for each pin of the Extended IO connector
(J5), at `/devices/gpio/pin<number>`, where `<number>` is the GPIO number of
the ESP32-S3. The pin table is `src/firmware/gpio.rs`. The tracking issue is
[platforms#221](https://github.com/xila-project/platforms/issues/221), and the
control commands are in Core's `devices/src/gpio.rs`
([core#384](https://github.com/xila-project/core/issues/384)).

Source: Wireless-Tag WT32-SC01 Plus datasheet V1.3, table 2 "Extended IO
Interface", and the schematic of the same document.

| Connector pin | Signal | GPIO | Device |
|---|---|---|---|
| 3 | EXT_IO1 | 10 | `/devices/gpio/pin10` |
| 4 | EXT_IO2 | 11 | `/devices/gpio/pin11` |
| 5 | EXT_IO3 | 12 | `/devices/gpio/pin12` |
| 6 | EXT_IO4 | 13 | `/devices/gpio/pin13` |
| 7 | EXT_IO5 | 14 | `/devices/gpio/pin14` |
| 8 | EXT_IO6 | 21 | `/devices/gpio/pin21` |

Pins 1 and 2 of the connector are +5 V and GND. The pins are 3.3 V logic.

### Withheld pins

The firmware creates a device for no other pin. The `CONFIGURE` command of a
device reconfigures its pin, so the firmware withholds every pin that the board uses. No device
can then change the display, the touch panel, the console, the flash, the PSRAM,
the microSD slot or the RS-485 transceiver. The datasheet names these pins:

| Function | GPIO | Table |
|---|---|---|
| LCD data, write clock, command or data, reset, backlight, frame sync | 0, 3, 4, 8, 9, 15, 16, 17, 18, 45, 46, 47, 48 | 5 |
| Touch panel: interrupt, I2C data, I2C clock | 7, 6, 5 | 5 |
| microSD slot: CS (data 3), MOSI (command), CLK, MISO (data 0) | 41, 40, 39, 38 | 4 |
| RS-485 transceiver: RXD, RTS, TXD | 1, 2, 42 | 8 |
| Audio amplifier: LRCK, BCLK, DOUT | 35, 36, 37 | 7 |
| Debug connector UART0: TXD0, RXD0 | 43, 44 | 1 |
| USB: D-, D+ (the console) | 19, 20 | |
| Flash and PSRAM of the module | 26 to 32 | |

The other connectors carry no free GPIO signal:

- The RS-485 connector (J7) carries the differential lines A and B, the ground
  and +5 V. The transceiver uses GPIO 1, 2 and 42. The datasheet remark of
  RS485-B (table 6) is "SD card data input": GPIO 38 and the RS-485 B line may
  share a pin. See "GPIO 38 and RS-485" in the microSD section: keep the RS-485
  connector unused while a microSD card is in use.
- The debug connector (J3) carries UART0 and BOOT (GPIO 0). Table 1 gives
  "GPIO 14" as module pin of the GND row. The schematic shows GND there, and
  the datasheet gives GPIO 14 as EXT_IO5, so this README treats the entry as a
  datasheet error.
- The speaker connector (J4) carries the amplifier outputs.

### Use

The command-line shell has no command that sends GPIO control commands. Check
the devices with:

```
ls /devices/gpio
```

The output lists `pin10`, `pin11`, `pin12`, `pin13`, `pin14` and `pin21`.
Applications use the `CONFIGURE`, `RELEASE` and `GET_CAPABILITIES` control
commands of kind `b'g'` on `/devices/gpio/pin<number>`, and then read or write
one byte (`0` or `1`). A read or write fails with `DeviceUnavailable` until
`CONFIGURE` configures the line. `RELEASE` returns the pin to high impedance.
Closing the device does not release the pin: the pin keeps its configuration,
and an output keeps its level, until an application sends `RELEASE` or
`CONFIGURE` again. Release on close is tracked in
[core#384](https://github.com/xila-project/core/issues/384).

Hardware validation is open: see the status in platforms#221.
