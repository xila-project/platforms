//! Pure logic for Nordic nRF52840 GPIO.
//!
//! Pin numbering, mode/level/pull decoding, reserved-mask validation,
//! and command ABI validation. Host-testable without hardware or Embassy dependencies.

pub use device::gpio::{
    ABI_VERSION, CONFIGURE, CapabilitiesAbi, ConfigureAbi, GET_CAPABILITIES, RELEASE, VersionAbi,
};
use file_system::Error;

/// Total number of GPIO pins on nRF52840 (Port 0 has 32, Port 1 has 16).
pub const PIN_COUNT: usize = 48;

/// Number of pins on Port 0 (P0.00 .. P0.31).
pub const PORT_0_PINS: u8 = 32;

/// Number of pins on Port 1 (P1.00 .. P1.15).
pub const PORT_1_PINS: u8 = 16;

/// Calculate the flat pin index (0..47) from hardware port (0 or 1) and pin number.
///
/// Port 0: pin 0..31 -> index 0..31
/// Port 1: pin 0..15 -> index 32..47
pub const fn pin_index(port: u8, pin: u8) -> Result<u8, Error> {
    match port {
        0 if pin < PORT_0_PINS => Ok(pin),
        1 if pin < PORT_1_PINS => Ok(PORT_0_PINS + pin),
        _ => Err(Error::InvalidParameter),
    }
}

/// Convert a flat pin index (0..47) to hardware (port, pin).
pub const fn port_and_pin(index: u8) -> Result<(u8, u8), Error> {
    if index < PORT_0_PINS {
        Ok((0, index))
    } else if index < (PORT_0_PINS + PORT_1_PINS) {
        Ok((1, index - PORT_0_PINS))
    } else {
        Err(Error::InvalidParameter)
    }
}

/// Check if a pin index is reserved according to the board mask.
///
/// Pins at index >= 48 are always considered reserved / invalid.
pub const fn is_pin_reserved(index: u8, reserved_mask: u64) -> bool {
    if index >= (PORT_0_PINS + PORT_1_PINS) {
        true
    } else {
        (reserved_mask & (1u64 << index)) != 0
    }
}

/// Validate that a pin index is valid and not reserved.
pub const fn validate_pin(index: u8, reserved_mask: u64) -> Result<(), Error> {
    if index >= (PORT_0_PINS + PORT_1_PINS) {
        return Err(Error::InvalidParameter);
    }
    if is_pin_reserved(index, reserved_mask) {
        return Err(Error::PermissionDenied);
    }
    Ok(())
}

/// GPIO line direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Input = 0,
    Output = 1,
}

/// Decode ABI direction byte.
pub const fn decode_direction(value: u8) -> Result<Direction, Error> {
    match value {
        0 => Ok(Direction::Input),
        1 => Ok(Direction::Output),
        _ => Err(Error::InvalidParameter),
    }
}

/// Internal pull resistor configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    Disabled = 0,
    Up = 1,
    Down = 2,
}

/// Decode ABI pull resistor byte.
pub const fn decode_pull(value: u8) -> Result<Pull, Error> {
    match value {
        0 => Ok(Pull::Disabled),
        1 => Ok(Pull::Up),
        2 => Ok(Pull::Down),
        _ => Err(Error::InvalidParameter),
    }
}

/// Output drive mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveMode {
    PushPull = 0,
    OpenDrain = 1,
}

/// Decode ABI drive mode byte.
pub const fn decode_drive_mode(value: u8) -> Result<DriveMode, Error> {
    match value {
        0 => Ok(DriveMode::PushPull),
        1 => Ok(DriveMode::OpenDrain),
        _ => Err(Error::InvalidParameter),
    }
}

/// Logical pin level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low = 0,
    High = 1,
}

/// Decode ABI level byte (0 low, 1 high).
pub const fn decode_level(value: u8) -> Result<Level, Error> {
    match value {
        0 => Ok(Level::Low),
        1 => Ok(Level::High),
        _ => Err(Error::InvalidParameter),
    }
}

/// Decode level from a plain write buffer byte.
///
/// Accepts binary `0` and `1`, as well as ASCII `b'0'` and `b'1'` for shell convenience.
pub const fn decode_write_level(byte: u8) -> Result<Level, Error> {
    match byte {
        0 | b'0' => Ok(Level::Low),
        1 | b'1' => Ok(Level::High),
        _ => Err(Error::InvalidParameter),
    }
}

/// Decode a plain write buffer: one level byte, optionally followed or preceded by ASCII
/// whitespace (as written by `echo`). `Ok(None)` means whitespace only (nothing to apply).
pub fn decode_write_buffer(buffer: &[u8]) -> Result<Option<Level>, Error> {
    let mut levels = buffer.iter().filter(|byte| !byte.is_ascii_whitespace());
    match (levels.next(), levels.next()) {
        (None, _) => Ok(None),
        (Some(&byte), None) => decode_write_level(byte).map(Some),
        _ => Err(Error::InvalidParameter),
    }
}

/// Text representation of a level returned by plain reads.
pub const fn encode_read_level(high: bool) -> &'static [u8; 2] {
    if high { b"1\n" } else { b"0\n" }
}

/// Configuration parameters decoded from a `ConfigureAbi` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedConfiguration {
    pub direction: Direction,
    pub pull: Pull,
    pub output: Option<(DriveMode, Level)>,
}

/// Validate a `ConfigureAbi` request.
pub fn validate_configure(request: &ConfigureAbi) -> Result<DecodedConfiguration, Error> {
    if request.version != ABI_VERSION || request.reserved != 0 {
        return Err(Error::InvalidParameter);
    }

    let pull = decode_pull(request.pull)?;
    let direction = decode_direction(request.direction)?;

    match direction {
        Direction::Input => Ok(DecodedConfiguration {
            direction: Direction::Input,
            pull,
            output: None,
        }),
        Direction::Output => {
            let drive_mode = decode_drive_mode(request.drive_mode)?;
            let initial_level = decode_level(request.initial_level)?;
            Ok(DecodedConfiguration {
                direction: Direction::Output,
                pull,
                output: Some((drive_mode, initial_level)),
            })
        }
    }
}

/// Validate a `VersionAbi` request.
pub const fn validate_version(request: &VersionAbi) -> Result<(), Error> {
    if request.version == ABI_VERSION && request.reserved == 0 {
        Ok(())
    } else {
        Err(Error::InvalidParameter)
    }
}

/// Returns standard `CapabilitiesAbi` for Nordic nRF52840 GPIO pins.
///
/// Supports input, output, pull-up, pull-down, and open-drain drive.
pub const fn get_capabilities() -> CapabilitiesAbi {
    CapabilitiesAbi {
        version: ABI_VERSION,
        input: 1,
        output: 1,
        pull_up: 1,
        pull_down: 1,
        open_drain: 1,
        reserved: 0,
    }
}

/// Returns a default `VersionAbi` response.
pub const fn version_abi() -> VersionAbi {
    VersionAbi {
        version: ABI_VERSION,
        reserved: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_indexing() {
        // Port 0 pins
        assert_eq!(pin_index(0, 0), Ok(0));
        assert_eq!(pin_index(0, 13), Ok(13));
        assert_eq!(pin_index(0, 31), Ok(31));

        // Port 1 pins
        assert_eq!(pin_index(1, 0), Ok(32));
        assert_eq!(pin_index(1, 3), Ok(35));
        assert_eq!(pin_index(1, 15), Ok(47));

        // Out of range
        assert_eq!(pin_index(0, 32), Err(Error::InvalidParameter));
        assert_eq!(pin_index(1, 16), Err(Error::InvalidParameter));
        assert_eq!(pin_index(2, 0), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_port_and_pin_conversion() {
        assert_eq!(port_and_pin(0), Ok((0, 0)));
        assert_eq!(port_and_pin(13), Ok((0, 13)));
        assert_eq!(port_and_pin(31), Ok((0, 31)));
        assert_eq!(port_and_pin(32), Ok((1, 0)));
        assert_eq!(port_and_pin(47), Ok((1, 15)));

        // Out of range
        assert_eq!(port_and_pin(48), Err(Error::InvalidParameter));
        assert_eq!(port_and_pin(100), Err(Error::InvalidParameter));
        assert_eq!(port_and_pin(255), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_pin_index_round_trip() {
        for idx in 0..48u8 {
            let (port, pin) = port_and_pin(idx).unwrap();
            assert_eq!(pin_index(port, pin), Ok(idx));
        }
    }

    #[test]
    fn test_reserved_pins() {
        // Reserve pin 13 and pin 22
        let mask = (1u64 << 13) | (1u64 << 22);

        assert!(!is_pin_reserved(0, mask));
        assert!(is_pin_reserved(13, mask));
        assert!(is_pin_reserved(22, mask));
        assert!(!is_pin_reserved(35, mask));
        assert!(is_pin_reserved(48, mask)); // out of bounds is reserved

        assert_eq!(validate_pin(0, mask), Ok(()));
        assert_eq!(validate_pin(13, mask), Err(Error::PermissionDenied));
        assert_eq!(validate_pin(22, mask), Err(Error::PermissionDenied));
        assert_eq!(validate_pin(48, mask), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_decode_enums() {
        assert_eq!(decode_direction(0), Ok(Direction::Input));
        assert_eq!(decode_direction(1), Ok(Direction::Output));
        assert_eq!(decode_direction(2), Err(Error::InvalidParameter));

        assert_eq!(decode_pull(0), Ok(Pull::Disabled));
        assert_eq!(decode_pull(1), Ok(Pull::Up));
        assert_eq!(decode_pull(2), Ok(Pull::Down));
        assert_eq!(decode_pull(3), Err(Error::InvalidParameter));

        assert_eq!(decode_drive_mode(0), Ok(DriveMode::PushPull));
        assert_eq!(decode_drive_mode(1), Ok(DriveMode::OpenDrain));
        assert_eq!(decode_drive_mode(2), Err(Error::InvalidParameter));

        assert_eq!(decode_level(0), Ok(Level::Low));
        assert_eq!(decode_level(1), Ok(Level::High));
        assert_eq!(decode_level(2), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_decode_write_level() {
        assert_eq!(decode_write_level(0), Ok(Level::Low));
        assert_eq!(decode_write_level(1), Ok(Level::High));
        assert_eq!(decode_write_level(b'0'), Ok(Level::Low));
        assert_eq!(decode_write_level(b'1'), Ok(Level::High));

        assert_eq!(decode_write_level(2), Err(Error::InvalidParameter));
        assert_eq!(decode_write_level(0xFF), Err(Error::InvalidParameter));
        assert_eq!(decode_write_level(b'x'), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_decode_write_buffer() {
        assert_eq!(decode_write_buffer(b"1\n"), Ok(Some(Level::High)));
        assert_eq!(decode_write_buffer(b"0"), Ok(Some(Level::Low)));
        assert_eq!(decode_write_buffer(&[1]), Ok(Some(Level::High)));
        assert_eq!(decode_write_buffer(b" 1\r\n"), Ok(Some(Level::High)));
        assert_eq!(decode_write_buffer(b"\n"), Ok(None));
        assert_eq!(decode_write_buffer(b""), Ok(None));
        assert_eq!(decode_write_buffer(b"10"), Err(Error::InvalidParameter));
        assert_eq!(decode_write_buffer(b"-n 1"), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_encode_read_level() {
        assert_eq!(encode_read_level(true), b"1\n");
        assert_eq!(encode_read_level(false), b"0\n");
    }

    #[test]
    fn test_validate_configure() {
        // Valid input configuration
        let input_req = ConfigureAbi {
            version: ABI_VERSION,
            direction: 0,
            pull: 1, // Pull Up
            drive_mode: 0,
            initial_level: 0,
            reserved: 0,
        };
        assert_eq!(
            validate_configure(&input_req),
            Ok(DecodedConfiguration {
                direction: Direction::Input,
                pull: Pull::Up,
                output: None,
            })
        );

        // Valid output configuration (push-pull)
        let output_req = ConfigureAbi {
            version: ABI_VERSION,
            direction: 1,
            pull: 0,
            drive_mode: 0,
            initial_level: 1,
            reserved: 0,
        };
        assert_eq!(
            validate_configure(&output_req),
            Ok(DecodedConfiguration {
                direction: Direction::Output,
                pull: Pull::Disabled,
                output: Some((DriveMode::PushPull, Level::High)),
            })
        );

        // Valid output configuration (open-drain)
        let od_req = ConfigureAbi {
            version: ABI_VERSION,
            direction: 1,
            pull: 1, // with pull-up
            drive_mode: 1,
            initial_level: 0,
            reserved: 0,
        };
        assert_eq!(
            validate_configure(&od_req),
            Ok(DecodedConfiguration {
                direction: Direction::Output,
                pull: Pull::Up,
                output: Some((DriveMode::OpenDrain, Level::Low)),
            })
        );

        // Invalid version
        let bad_ver = ConfigureAbi {
            version: ABI_VERSION + 1,
            ..input_req
        };
        assert_eq!(validate_configure(&bad_ver), Err(Error::InvalidParameter));

        // Non-zero reserved
        let bad_res = ConfigureAbi {
            reserved: 1,
            ..input_req
        };
        assert_eq!(validate_configure(&bad_res), Err(Error::InvalidParameter));

        // Invalid pull
        let bad_pull = ConfigureAbi {
            pull: 3,
            ..input_req
        };
        assert_eq!(validate_configure(&bad_pull), Err(Error::InvalidParameter));

        // Invalid drive mode on output
        let bad_drive = ConfigureAbi {
            drive_mode: 2,
            ..output_req
        };
        assert_eq!(validate_configure(&bad_drive), Err(Error::InvalidParameter));

        // Invalid initial level on output
        let bad_level = ConfigureAbi {
            initial_level: 2,
            ..output_req
        };
        assert_eq!(validate_configure(&bad_level), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_validate_version() {
        let ok = VersionAbi {
            version: ABI_VERSION,
            reserved: 0,
        };
        assert_eq!(validate_version(&ok), Ok(()));

        let bad_ver = VersionAbi {
            version: 0,
            reserved: 0,
        };
        assert_eq!(validate_version(&bad_ver), Err(Error::InvalidParameter));

        let bad_res = VersionAbi {
            version: ABI_VERSION,
            reserved: 1,
        };
        assert_eq!(validate_version(&bad_res), Err(Error::InvalidParameter));
    }

    #[test]
    fn test_capabilities() {
        let caps = get_capabilities();
        assert_eq!(caps.version, ABI_VERSION);
        assert_eq!(caps.input, 1);
        assert_eq!(caps.output, 1);
        assert_eq!(caps.pull_up, 1);
        assert_eq!(caps.pull_down, 1);
        assert_eq!(caps.open_drain, 1);
        assert_eq!(caps.reserved, 0);
    }
}
