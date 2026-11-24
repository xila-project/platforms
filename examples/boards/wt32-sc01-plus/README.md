# WT32-SC01 Plus Wi-Fi

The board starts its ESP32-S3 station in 2.4 GHz mode and exposes
`/devices/network/wlan0`. It can scan and associate with open and WPA2 Personal
networks. The Settings Wi-Fi panel can turn the radio on and off, scan, connect
and disconnect when the graphical firmware includes Settings. Core owns
Ethernet frame processing, IP addressing and DNS.

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

## microSD card

The slot takes a microSD card. The firmware reads the card in native SD mode,
through the SDHOST controller, with a 1-bit bus at 20 MHz. The card holds a
LittleFS file system with 512-byte blocks. A card has no other file system
support.

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
- A card that does not hold LittleFS is not mounted and not changed. The boot
  log says so.

### Format

The firmware never formats a card by itself. To format the card with LittleFS,
type these commands in the command-line shell (the console, or the Terminal
application), as a user who can write to `/configuration/system`:

```
echo erase > /configuration/system/format_sd_card
reboot
```

At the next boot, the firmware removes the file and formats the card, whatever
it holds. A file that does not contain the text `erase` is removed without
effect. A format request is for one boot: with no card in the slot, the
firmware discards it.

### Test

To write a 256 KiB file to the mounted card, read it back and compare it, type:

```
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
- No card has been tested on a board yet. The tested cards and their speeds
  will be listed here.
