//! Pin compatibility for the peripheral adapters.
//!
//! Board pins are chosen by the application, not by the platform. The
//! application builds each esp-hal driver with its own pins and passes the driver
//! to the adapter, for example:
//!
//! ```text
//! let i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, Config::default())?
//!     .with_sda(peripherals.GPIO4)
//!     .with_scl(peripherals.GPIO5);
//! let i2c = EspI2c::new(i2c);
//! ```
//!
//! esp-hal checks what each peripheral's signal pins can carry through its
//! trait bounds, so an incompatible pin fails to compile. The tables below
//! record what the bounds cannot express: pins that build but would conflict
//! with boot, flash or the board itself.
//!
//! # ADC channels
//!
//! esp-hal only accepts an analog pin for an ADC channel. The channel-to-pin
//! map below is generated from esp-hal's own chip metadata.
//!
//! | Chip      | ADC1 channels                         | ADC2 channels                                 |
//! |-----------|---------------------------------------|-----------------------------------------------|
//! | ESP32     | GPIO36, 37, 38, 39, 32, 33, 34, 35    | GPIO4, 0, 2, 15, 13, 12, 14, 27, 25, 26       |
//! | ESP32-S3  | GPIO1 to GPIO10                       | GPIO11 to GPIO20                              |
//! | ESP32-C6  | GPIO0 to GPIO6                        | none                                          |
//!
//! ADC2 is not usable while Wi-Fi is active; ADC1 is the choice for Wi-Fi
//! applications.
//!
//! # Pins to avoid
//!
//! | Chip      | Strapping (boot mode) | Flash / PSRAM                            | Other constraints                                  |
//! |-----------|-----------------------|------------------------------------------|----------------------------------------------------|
//! | ESP32     | GPIO0, 2, 5, 12, 15   | GPIO6 to 11 (never use)                  | GPIO34 to 39 are input-only, no pulls. GPIO36 and 39 feed the Hall sensor. |
//! | ESP32-S3  | GPIO0, 3, 45, 46      | GPIO26 to 32 (reserved); GPIO33 to 37 reserved in octal mode | |
//! | ESP32-C6  | GPIO4, 5, 8, 9, 15    | check the module datasheet               | GPIO8 drives the RGB LED on DevKitC-1.             |
//!
//! Strapping pins are sampled at reset. Use them only when the external
//! circuit does not hold them at a level that changes the boot mode.
//!
//! # Other peripherals
//!
//! I2C, SPI, UART and LEDC (PWM) outputs use the GPIO matrix, so any pin
//! outside the lists above can carry their signals. Check the board schematic
//! for pins already used by the board, such as USB or the onboard LED.
//!
//! # Sources
//!
//! - esp-hal generated chip metadata (`esp-metadata-generated`, analog function
//!   list per chip), which the ADC table reflects.
//! - Espressif ESP-IDF GPIO and ADC reference, and the ESP32, ESP32-S3 and
//!   ESP32-C6 datasheets. The strapping and flash lists were cross-checked
//!   against third-party pinout guides; confirm against the datasheet of the
//!   exact module before finalising a board.
