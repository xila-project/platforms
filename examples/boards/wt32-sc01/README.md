# WT32-SC01

Wireless-Tag's WT32-SC01: an ESP32 on the ESP32-WROVER-B module, with 4 MB of
flash and 8 MB of PSRAM, and a 3.5-inch 480 × 320 touch screen. The build and
flash steps, the display pins and the memory notes are in the
[board README](../README.md#build-and-flash-the-wt32-sc01-firmware).

## GPIO

The firmware mounts a GPIO device for each free pin of the expansion
connectors, at `/devices/gpio/pin<number>`, where `<number>` is the GPIO number
of the ESP32. The pin table is `src/firmware/gpio.rs`. The tracking issue is
[platforms#221](https://github.com/xila-project/platforms/issues/221), and the
control commands are in Core's `devices/src/gpio.rs`
([core#384](https://github.com/xila-project/core/issues/384)).

Source: Wireless-Tag WT32-SC01 datasheet V1.0, page 7, figure 4 "Interface
Definition Diagram". The board has two 2 × 20 expansion connectors (items 8 and
9 of the datasheet). They are wired pin to pin, so a pin has the same signal on
both connectors.

| Device | Connector signal | Note |
|---|---|---|
| `/devices/gpio/pin2` | IO2 | Strapping pin |
| `/devices/gpio/pin4` | IO4 | |
| `/devices/gpio/pin5` | IO5 (I2S_SCLK) | Strapping pin |
| `/devices/gpio/pin12` | IO12 | Strapping pin |
| `/devices/gpio/pin25` | IO25 (I2S_LRCK) | |
| `/devices/gpio/pin26` | IO26 (I2S_DSDIN) | |
| `/devices/gpio/pin27` | IO27 | |
| `/devices/gpio/pin32` | IO32 | |
| `/devices/gpio/pin33` | IO33 | |
| `/devices/gpio/pin34` | IO34 | Input only |
| `/devices/gpio/pin35` | IO35 (I2S_ASDOUT) | Input only |
| `/devices/gpio/pin36` | SENSOR_VP | Input only |

The chip samples the strapping pins at reset. At reset, the firmware does not
run and the pins have their internal pull resistors only, so only an external
circuit can change the sampled level:

- GPIO 12 (MTDI) selects the voltage of the flash and PSRAM supply. The flash
  and PSRAM of the ESP32-WROVER-B use 3.3 V. If an external circuit holds GPIO
  12 high at reset, the chip selects 1.8 V and the board does not boot. Keep
  GPIO 12 low or floating at reset (it has an internal pull-down).
- GPIO 2 must be low or floating when GPIO 0 is low at reset, or the chip
  cannot enter the download mode. It has no effect on a normal boot.
- GPIO 5 sets, with GPIO 15, the timing of the SDIO slave. The firmware does
  not use the SDIO slave.

The input-only pins have no output driver and no pull resistor.

### Withheld pins

The firmware creates a device for no other pin. The `CONFIGURE` command of a
device reconfigures its pin, so the firmware withholds every pin that the board uses, even when the
figure shows it on a connector:

| GPIO | Connector signal | Reason |
|---|---|---|
| 0 | IO0 (I2S_MCLK) | The USB-UART bridge drives it, with EN, to enter the boot loader. |
| 1 | TXD | UART0 transmit: the console. GPIO 3 (receive) is not on the connector. |
| 13, 14, 15 | IO13, IO14, IO15 | Display on HSPI: data, clock, chip select. |
| 21, 22, 23 | IO21, IO22, IO23 | Display command or data, reset and backlight. |
| 18, 19 | I2C_SDA, I2C_SCL | Touch panel I2C. |
| 39 | SENSOR_VN | The touch controller drives its interrupt line. The firmware polls instead. |
| 6 to 11, 16, 17 | | Flash and PSRAM of the module (not on the connectors). |

The display and touch pins come from the [board README](../README.md#build-and-flash-the-wt32-sc01-firmware)
and `src/firmware/screen.rs`. The figure does not give GPIO numbers for
I2C_SDA and I2C_SCL: they are the touch panel pins 18 and 19.

### Use

The command-line shell has no command that sends GPIO control commands. Check
the devices with:

```
ls /devices/gpio
```

The output lists the twelve devices of the table. Applications use the
`CONFIGURE`, `RELEASE` and `GET_CAPABILITIES` control commands of kind `b'g'` on
`/devices/gpio/pin<number>`, and then read or write one byte (`0` or `1`). A read
or write fails with `DeviceUnavailable` until `CONFIGURE` configures the line.
`RELEASE` returns the pin to high impedance. Closing the device does not release
the pin: the pin keeps its configuration, and an output keeps its level, until
an application sends `RELEASE` or `CONFIGURE` again. Release on close is tracked
in [core#384](https://github.com/xila-project/core/issues/384).

Hardware validation is open: see the status in platforms#221.
