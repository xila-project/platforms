# WT32-SC01 Plus Wi-Fi

The board enables the ESP32-S3 station driver for 2.4 GHz open and WPA2 Personal
networks. The radio starts only when the firmware finds a valid station profile
on LittleFS. Startup continues to the login prompt if the profile is absent
or if Wi-Fi initialization returns an error.

## Station profile

Create `/configuration/system/wifi` in the board's LittleFS storage partition
with owner root (user ID 0) and mode `0600`. Provision the file privately in
the storage image. Use a local file outside the checkout and keep its contents
out of shell commands, console transcripts, source control and CI artifacts.

The first line is the network name (1–32 bytes). The second is the WPA2
passphrase (8–63 printable ASCII bytes or a 64-digit hexadecimal key). Use an
empty second line for an open network. Finish each line with a newline.
The two-line format enables Core's DHCP client. The pinned Core starts DHCP
and exchanges packets but does not apply the lease to the interface. For an
end-to-end connectivity test with this revision, append three lines in this
order to select static IPv4 instead.

1. An unused IPv4 address with prefix length for the test subnet.
2. The router's IPv4 address.
3. A reachable DNS server's IPv4 address.

With this profile, the board mounts `/devices/network/wlan0` and sends and
receives Ethernet frames through Core's smoltcp stack. Core also owns IP
addresses, routes and DNS. The network device implements Core's
`CONNECT_WIFI_CLIENT`, `DISCONNECT_WIFI_CLIENT`, `IS_LINK_UP` and the Wi-Fi
connection state, information and error queries.

## Hardware smoke test

1. Boot with no profile. Expect `Wi-Fi: no station configuration; radio disabled`
   and `User name:`.
2. Provision a five-line profile using a test access point on a permitted 2.4 GHz
   channel and restart. Expect `Wi-Fi: wlan0 ready; internal heap free before
   startup …, after startup … bytes` and `User name:`. A successful association
   logs `Wi-Fi: station connected` if it completes before the console filters
   informational messages. After login, `ip address` should show `wlan0` as
   `Enabled, Up` with the selected address. `ip route` should show the gateway.
3. Run `ping -c 4 <gateway>`, ping the board from a peer on the same test
   network, and run `dns_resolve <known hostname>`. Expect ICMP replies in both
   directions and an address from DNS.
4. Send `DISCONNECT_WIFI_CLIENT` with unit input on `/devices/network/wlan0`
   from a test client using Core's device controls. Check that
   `GET_WIFI_CONNECTION_STATE` reports `DISCONNECTED` and `IS_LINK_UP` is false.
   With the access point reachable, send `CONNECT_WIFI_CLIENT` with an in-memory
   `WifiClientConfiguration` read privately from the profile. Wait for
   `CONNECTED` and repeat the ping. Turning off the access point also exercises
   the unsolicited link-loss path, which logs `Wi-Fi: link lost (…)`.
5. Repeat boot with an incorrect passphrase and with the test access point
   absent. Expect `Wi-Fi: connection failed (authentication)` or
   `Wi-Fi: connection failed (access point not found)`, according to the
   disconnect reason the access point and radio report. Confirm the link stays
   down and the console reaches login. A timed-out attempt reports
   `Wi-Fi: connection failed (timeout)`. Check captures for credential leakage
   before sharing them.

## Internal SRAM

esp-radio 1.0.0-beta.1 and esp-rtos 0.4.0 use esp-hal 1.2.2. The radio needs a
preemptive scheduler. esp-rtos uses TIMG0 timer 0 and software interrupt 0 on
the first core. Xila's executor stays on the scheduler's main thread and keeps
its SYSTIMER alarm 0. The second core remains parked. Radio C allocations and
radio task stacks use the existing Xila heap's internal DMA-capable region.
The board still registers its 2 MiB of PSRAM for eligible Xila allocations.

The radio-enabled firmware reserves 216 KiB (221,184 bytes) of internal SRAM
for the heap, down from 256 KiB, to leave room for the radio's linked code and
data and the executor's main stack. The driver caps the radio at six static RX,
twelve dynamic RX and eight dynamic TX buffers. At about 1.6 KB per frame,
their combined maximum payload storage is roughly 42 KB, including about 10 KB
of static RX buffers. Scheduler stacks, management frames, WPA state and
allocator overhead are additional. Startup requires 96 KiB of free internal heap
before turning on the radio. The two startup counters show actual heap use at
driver initialization and interface registration. Association and traffic
can allocate additional buffers, so record the heap report at login and measure
under load on hardware before raising buffer counts.
