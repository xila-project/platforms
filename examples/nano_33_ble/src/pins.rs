//! Arduino Nano 33 BLE pin mappings and GPIO mounting.
//!
//! Pin mapping references:
//! - ArduinoCore-mbed: `variants/ARDUINO_NANO33BLE/variant.cpp`
//! - ArduinoCore-mbed: `variants/ARDUINO_NANO33BLE/pins_arduino.h`
//! - Arduino Nano 33 BLE official schematic (Rev 1 / Rev 2)
//!
//! On the Arduino Nano 33 BLE:
//! - Header digital pins D0..D13 and analog pins A0..A7 are routed to nRF52840 GPIOs.
//! - D13 is wired to LED_BUILTIN (P0.13, active-high).
//! - On-board RGB LEDs on P0.24 (Red), P0.16 (Green), P0.06 (Blue) are active-low.
//! - Power-enable pins (P0.22 VDD_ENV_ENABLE, P1.00 I2C_PULL, P0.17 PDM PWR, P1.09 LED PWR)
//!   and internal sensor/PDM lines are reserved and cannot be accessed as generic GPIO.

#![allow(dead_code)]

use drivers_nordic::gpio::{Direction, DriveMode, GpioPinDevice, Level, Pull};
use xila::{file_system, task, virtual_file_system};

// Header Digital Pins D0..D13
pub const PIN_D0: u8 = 35; // P1.03 (D0 / TX)
pub const PIN_D1: u8 = 42; // P1.10 (D1 / RX)
pub const PIN_D2: u8 = 43; // P1.11 (D2)
pub const PIN_D3: u8 = 44; // P1.12 (D3)
pub const PIN_D4: u8 = 47; // P1.15 (D4)
pub const PIN_D5: u8 = 45; // P1.13 (D5)
pub const PIN_D6: u8 = 46; // P1.14 (D6)
pub const PIN_D7: u8 = 23; // P0.23 (D7)
pub const PIN_D8: u8 = 21; // P0.21 (D8)
pub const PIN_D9: u8 = 27; // P0.27 (D9)
pub const PIN_D10: u8 = 34; // P1.02 (D10)
pub const PIN_D11: u8 = 33; // P1.01 (D11 / MOSI)
pub const PIN_D12: u8 = 40; // P1.08 (D12 / MISO)
pub const PIN_D13: u8 = 13; // P0.13 (D13 / SCK / LED_BUILTIN)
pub const PIN_LED_BUILTIN: u8 = PIN_D13;

// Header Analog Pins A0..A7
pub const PIN_A0: u8 = 4; // P0.04 (A0)
pub const PIN_A1: u8 = 5; // P0.05 (A1)
pub const PIN_A2: u8 = 30; // P0.30 (A2)
pub const PIN_A3: u8 = 29; // P0.29 (A3)
pub const PIN_A4: u8 = 31; // P0.31 (A4 / SDA)
pub const PIN_A5: u8 = 2; // P0.02 (A5 / SCL)
pub const PIN_A6: u8 = 28; // P0.28 (A6)
pub const PIN_A7: u8 = 3; // P0.03 (A7)

// Onboard RGB LEDs (active-low)
pub const PIN_LED_RED: u8 = 24; // P0.24 (LED R, active-low)
pub const PIN_LED_GREEN: u8 = 16; // P0.16 (LED G, active-low)
pub const PIN_LED_BLUE: u8 = 6; // P0.06 (LED B, active-low)

// Internal and power-enable pins (must remain reserved)
pub const PIN_VDD_ENV_ENABLE: u8 = 22; // P0.22 (powers onboard sensors 3.3V)
pub const PIN_I2C_PULLUP: u8 = 32; // P1.00 (internal I2C pullup)
pub const PIN_PDM_PWR: u8 = 17; // P0.17 (PDM microphone power)
pub const PIN_LED_PWR: u8 = 41; // P1.09 (power LED)
pub const PIN_SDA2: u8 = 14; // P0.14 (internal I2C SDA)
pub const PIN_SCL2: u8 = 15; // P0.15 (internal I2C SCL)
pub const PIN_INT_APDS: u8 = 19; // P0.19 (sensor interrupt)
pub const PIN_PDM_DIN: u8 = 25; // P0.25 (PDM DIN)
pub const PIN_PDM_CLK: u8 = 26; // P0.26 (PDM CLK)

/// Reserved pin mask for Arduino Nano 33 BLE.
///
/// Prevents user firmware/shell access from disabling power rails or corrupting internal buses.
pub const NANO_33_BLE_RESERVED_PINS_MASK: u64 = (1u64 << PIN_VDD_ENV_ENABLE)
    | (1u64 << PIN_I2C_PULLUP)
    | (1u64 << PIN_PDM_PWR)
    | (1u64 << PIN_LED_PWR)
    | (1u64 << PIN_SDA2)
    | (1u64 << PIN_SCL2)
    | (1u64 << PIN_INT_APDS)
    | (1u64 << PIN_PDM_DIN)
    | (1u64 << PIN_PDM_CLK);

pub struct PinEntry {
    pub path: &'static str,
    pub pin_index: u8,
    pub initial_output: Option<Level>,
}

pub const BOARD_PINS: &[PinEntry] = &[
    PinEntry {
        path: "/devices/gpio/d0",
        pin_index: PIN_D0,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d1",
        pin_index: PIN_D1,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d2",
        pin_index: PIN_D2,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d3",
        pin_index: PIN_D3,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d4",
        pin_index: PIN_D4,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d5",
        pin_index: PIN_D5,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d6",
        pin_index: PIN_D6,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d7",
        pin_index: PIN_D7,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d8",
        pin_index: PIN_D8,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d9",
        pin_index: PIN_D9,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d10",
        pin_index: PIN_D10,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d11",
        pin_index: PIN_D11,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/d12",
        pin_index: PIN_D12,
        initial_output: None,
    },
    // D13 (LED_BUILTIN): pre-configured as push-pull output, initially low (off)
    PinEntry {
        path: "/devices/gpio/d13",
        pin_index: PIN_D13,
        initial_output: Some(Level::Low),
    },
    PinEntry {
        path: "/devices/gpio/a0",
        pin_index: PIN_A0,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a1",
        pin_index: PIN_A1,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a2",
        pin_index: PIN_A2,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a3",
        pin_index: PIN_A3,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a4",
        pin_index: PIN_A4,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a5",
        pin_index: PIN_A5,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a6",
        pin_index: PIN_A6,
        initial_output: None,
    },
    PinEntry {
        path: "/devices/gpio/a7",
        pin_index: PIN_A7,
        initial_output: None,
    },
    // RGB LEDs: pre-configured as push-pull outputs, initially high (off, active-low)
    PinEntry {
        path: "/devices/gpio/led_red",
        pin_index: PIN_LED_RED,
        initial_output: Some(Level::High),
    },
    PinEntry {
        path: "/devices/gpio/led_green",
        pin_index: PIN_LED_GREEN,
        initial_output: Some(Level::High),
    },
    PinEntry {
        path: "/devices/gpio/led_blue",
        pin_index: PIN_LED_BLUE,
        initial_output: Some(Level::High),
    },
];

/// Mount all board GPIO devices under `/devices/gpio/...`.
pub async fn mount_gpio_pins(
    virtual_file_system: &virtual_file_system::VirtualFileSystem,
    task: task::TaskIdentifier,
) -> virtual_file_system::Result<()> {
    virtual_file_system::ignore_already_exists_error(
        virtual_file_system
            .create_directory(task, &file_system::Path::DEVICES)
            .await,
    )?;
    virtual_file_system::ignore_already_exists_error(
        virtual_file_system
            .create_directory(task, &file_system::Path::GPIO_DEVICES)
            .await,
    )?;

    for entry in BOARD_PINS {
        let device = GpioPinDevice::new(entry.pin_index, NANO_33_BLE_RESERVED_PINS_MASK)
            .map_err(virtual_file_system::Error::FileSystem)?;

        if let Some(initial_level) = entry.initial_output {
            device
                .configure_pin(
                    Direction::Output,
                    Pull::Disabled,
                    Some((DriveMode::PushPull, initial_level)),
                )
                .map_err(virtual_file_system::Error::FileSystem)?;
        }

        let _ = virtual_file_system.remove(task, entry.path).await;
        virtual_file_system
            .mount_character_device(task, entry.path, device)
            .await?;
    }

    Ok(())
}
