# Arduino Nano 33 BLE example

Xila on the Arduino Nano 33 BLE (nRF52840): a headless system behind the USB
serial port, with its root file system in the internal flash. No display, no
network, no external storage.

```sh
mise run flash:nano-33-ble
```

builds the firmware and writes it through the Arduino bootloader, which stays
in place. Then open the serial port the board presents (any speed), for example
`picocom /dev/ttyACM0`, and log in as `administrator` with an empty password.

The first start formats the file system and creates that account. Flashing
again keeps the file system; to start over, erase its first pages from the
bootloader.

## Layout

| Region | Flash | Use |
|---|---|---|
| Bootloader | `0x00000`–`0x10000` (64 KiB) | Arduino's, never written |
| Firmware | `0x10000`–`0x80000` (448 KiB) | Xila |
| File system | `0x80000`–`0x100000` (512 KiB) | LittleFS, 4 KiB blocks |

`memory.x` holds these addresses. The RAM is 256 KiB: static data, then the
heap, then 32 KiB of stack; the last 256 bytes carry the message of a crash
across the reset.

## Recovery

- Opening the port at 1200 baud and closing it reboots into the bootloader
  (`stty -F /dev/ttyACM0 1200 hupcl`); `flash.sh` does it.
- A panic or fault stores its message, which the next start logs, and resets.
  After three in a row the board stays in its bootloader.
- Double-tapping the reset button always enters the bootloader.

Build with `--features bring-up` when the console itself is in doubt: the start
then waits for a terminal before doing anything that can fail, and the board
returns to its bootloader if none opened the port within a minute.

## GPIO Devices

Header pins and onboard LEDs are exposed as character devices under `/devices/gpio/`:

- Header digital pins: `/devices/gpio/d0` .. `/devices/gpio/d13`
- Header analog pins: `/devices/gpio/a0` .. `/devices/gpio/a7`
- Onboard LEDs:
  - `/devices/gpio/d13` (yellow `LED_BUILTIN`, P0.13, active-high, initially off)
  - `/devices/gpio/led_red` (P0.24, active-low, initially off)
  - `/devices/gpio/led_green` (P0.16, active-low, initially off)
  - `/devices/gpio/led_blue` (P0.06, active-low, initially off)

### Shell Usage

Read pin level (returns the text `0` or `1` followed by a newline):
```sh
cat /devices/gpio/d13
```

Write output level:
```sh
# Turn on yellow LED_BUILTIN (P0.13, active-high):
echo 1 > /devices/gpio/d13
# Turn off yellow LED_BUILTIN:
echo 0 > /devices/gpio/d13

# Turn on RGB red LED (P0.24, active-low):
echo 0 > /devices/gpio/led_red
# Turn off RGB red LED:
echo 1 > /devices/gpio/led_red
```

A write holds a single `0` or `1` (ASCII or raw byte), optionally surrounded by whitespace; anything else is rejected with `Invalid parameter`.
Pins can also be dynamically configured via VFS control commands (kind `b'g'`: `CONFIGURE` `0x00`, `RELEASE` `0x01`, `GET_CAPABILITIES` `0x02`).
Power-enable lines (P0.22 sensor rail, P1.00 I2C pullup, P0.17 PDM power, P1.09 power LED) and internal sensor buses remain reserved.

## Restart

`reboot` (administrator or root) stops the shell, commits and unmounts the
LittleFS partition, then resets the board through the power device
`/devices/power`. The board cannot power itself off or sleep yet: `poweroff`
reports that the action is unsupported.

## Limits

- The date starts from the build date at every power-up: the board has no clock
  that keeps running without power.
- The USB identifiers are the test pair of pid.codes (`1209:0001`).
- Input goes through a line discipline (echo, backspace, line buffering).

