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
Normal Xila thread allocations retain access to the 2 MiB PSRAM.

The Wi-Fi firmware reserves 216 KiB (221,184 bytes) for the internal heap.
The radio caps frame buffers at six static receive, twelve dynamic receive
and eight dynamic transmit. At about 1.6 KB per frame they can account for
roughly 42 KB, plus scheduler stacks, management frames and WPA state.
Startup checks for 96 KiB of free internal heap. Record the startup heap
counters and the memory report at login, then measure under traffic.

Core's synchronous smoltcp device API offers no receive wake callback. The
Core runner polls at least every 20 ms while idle, and the driver raises the
radio receive queue from five to twelve frames to absorb bursts. An
interrupt-driven receive path needs a wake interface from Core.
