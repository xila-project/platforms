//! FocalTech FT6336U capacitive touch controller on I²C.
//!
//! The controller keeps its latest report in registers 0x02 to 0x06 (FT6336U
//! datasheet, register map): the number of touch points, then the first
//! point's event flag and X coordinate, and its touch identifier and Y
//! coordinate, 12 bits each. The driver polls it: one 5-byte read per sample.

use embedded_hal::i2c::I2c;

use super::{TouchSensor, TouchTransform};

/// The controller's 7-bit I²C address.
pub const FT6336_ADDRESS: u8 = 0x38;

/// `TD_STATUS`: the number of touch points; the first point's registers
/// follow it.
const TOUCH_STATUS_REGISTER: u8 = 0x02;
/// `ID_G_FOCALTECH_ID`: FocalTech's panel identifier.
const VENDOR_IDENTIFIER_REGISTER: u8 = 0xA8;
/// `ID_G_CIPHER`: the chip identifier.
const CHIP_IDENTIFIER_REGISTER: u8 = 0xA3;

/// The controller supports two touch points; larger counts, such as 0x0F
/// after power-up, mean that no valid report is available.
const MAXIMUM_POINTS: u8 = 2;

/// Event flag of a touch point (bits 7 and 6 of `P1_XH`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchEvent {
    PressDown,
    LiftUp,
    Contact,
    NoEvent,
}

impl TouchEvent {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b00 => Self::PressDown,
            0b01 => Self::LiftUp,
            0b10 => Self::Contact,
            _ => Self::NoEvent,
        }
    }
}

/// The first touch point of a report, in the controller's native coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub event: TouchEvent,
    pub identifier: u8,
}

/// Parses registers 0x02 to 0x06. Returns the first touch point while the
/// panel is touched, and `None` once the finger is lifted or when the report
/// is not valid.
pub fn parse_report(report: &[u8; 5]) -> Option<TouchPoint> {
    let points = report[0] & 0x0F;
    if points == 0 || points > MAXIMUM_POINTS {
        return None;
    }
    let event = TouchEvent::from_bits(report[1] >> 6);
    if matches!(event, TouchEvent::LiftUp | TouchEvent::NoEvent) {
        return None;
    }
    Some(TouchPoint {
        x: u16::from(report[1] & 0x0F) << 8 | u16::from(report[2]),
        y: u16::from(report[3] & 0x0F) << 8 | u16::from(report[4]),
        event,
        identifier: report[3] >> 4,
    })
}

/// An FT6336U on an I²C bus.
pub struct Ft6336<I2C> {
    i2c: I2C,
    address: u8,
    transform: TouchTransform,
    last_native_point: Option<(u16, u16)>,
}

impl<I2C: I2c> Ft6336<I2C> {
    /// A controller at `address` ([`FT6336_ADDRESS`] by default), whose points
    /// `transform` maps to the screen.
    pub const fn new(i2c: I2C, address: u8, transform: TouchTransform) -> Self {
        Self {
            i2c,
            address,
            transform,
            last_native_point: None,
        }
    }

    fn read_register(&mut self, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0];
        self.i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
    }

    /// The chip identifier (`ID_G_CIPHER`) and FocalTech's panel identifier
    /// (`ID_G_FOCALTECH_ID`), to check that the controller answers.
    pub fn identifiers(&mut self) -> Result<(u8, u8), I2C::Error> {
        Ok((
            self.read_register(CHIP_IDENTIFIER_REGISTER)?,
            self.read_register(VENDOR_IDENTIFIER_REGISTER)?,
        ))
    }

    /// Reads the first touch point, in native coordinates.
    pub fn read_point(&mut self) -> Result<Option<TouchPoint>, I2C::Error> {
        let mut report = [0; 5];
        self.i2c
            .write_read(self.address, &[TOUCH_STATUS_REGISTER], &mut report)?;
        Ok(parse_report(&report))
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}

impl<I2C: I2c> TouchSensor for Ft6336<I2C> {
    type Error = I2C::Error;

    fn read(&mut self) -> Result<Option<(i16, i16)>, Self::Error> {
        let point = self.read_point()?;
        self.last_native_point = point.map(|point| (point.x, point.y));
        Ok(point.map(|point| self.transform.apply(point.x, point.y)))
    }

    fn last_native_point(&self) -> Option<(u16, u16)> {
        self.last_native_point
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{vec, vec::Vec};

    use embedded_hal::i2c::{ErrorType, Operation};

    use super::*;

    /// Answers reads from a register map and records the transactions.
    struct RegisterMap {
        registers: [u8; 256],
        transactions: Vec<(u8, u8, usize)>,
    }

    impl RegisterMap {
        fn new(values: &[(u8, u8)]) -> Self {
            let mut registers = [0; 256];
            for &(register, value) in values {
                registers[usize::from(register)] = value;
            }
            Self {
                registers,
                transactions: vec![],
            }
        }
    }

    impl ErrorType for RegisterMap {
        type Error = core::convert::Infallible;
    }

    impl I2c for RegisterMap {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0usize;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => register = usize::from(bytes[0]),
                    Operation::Read(buffer) => {
                        self.transactions
                            .push((address, register as u8, buffer.len()));
                        for (offset, byte) in buffer.iter_mut().enumerate() {
                            *byte = self.registers[register + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    const IDENTITY: TouchTransform = TouchTransform::identity(320, 480);

    #[test]
    fn no_touch_points_is_released() {
        assert_eq!(parse_report(&[0x00, 0x81, 0x23, 0x01, 0x45]), None);
    }

    #[test]
    fn power_up_status_is_not_a_touch() {
        assert_eq!(parse_report(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), None);
    }

    #[test]
    fn contact_reports_twelve_bit_coordinates() {
        assert_eq!(
            parse_report(&[0x01, 0x81, 0x23, 0x11, 0xDF]),
            Some(TouchPoint {
                x: 0x123,
                y: 0x1DF,
                event: TouchEvent::Contact,
                identifier: 1,
            })
        );
    }

    #[test]
    fn press_down_is_a_touch() {
        let point = parse_report(&[0x01, 0x00, 0x05, 0x00, 0x07]).unwrap();
        assert_eq!(
            (point.x, point.y, point.event),
            (5, 7, TouchEvent::PressDown)
        );
    }

    #[test]
    fn lift_up_is_released() {
        assert_eq!(parse_report(&[0x01, 0x40, 0x10, 0x00, 0x20]), None);
    }

    #[test]
    fn upper_status_bits_are_ignored() {
        // Bits 7 to 4 of TD_STATUS are reserved.
        assert!(parse_report(&[0x31, 0x80, 0x10, 0x00, 0x20]).is_some());
    }

    #[test]
    fn reads_the_report_in_one_transaction() {
        let mut controller = Ft6336::new(
            RegisterMap::new(&[
                (0x02, 1),
                (0x03, 0x80),
                (0x04, 10),
                (0x05, 0x00),
                (0x06, 20),
            ]),
            FT6336_ADDRESS,
            IDENTITY,
        );
        assert_eq!(controller.read().unwrap(), Some((10, 20)));
        assert_eq!(controller.last_native_point(), Some((10, 20)));
        assert_eq!(controller.release().transactions, vec![(0x38, 0x02, 5)]);
    }

    #[test]
    fn reads_the_identifiers() {
        let mut controller = Ft6336::new(
            RegisterMap::new(&[(0xA3, 0x64), (0xA8, 0x11)]),
            FT6336_ADDRESS,
            IDENTITY,
        );
        assert_eq!(controller.identifiers().unwrap(), (0x64, 0x11));
    }

    #[test]
    fn landscape_transform_swaps_then_mirrors() {
        // A 320 × 480 portrait panel under a 480 × 320 landscape screen.
        let transform = TouchTransform {
            width: 480,
            height: 320,
            swap_axes: true,
            mirror_x: false,
            mirror_y: true,
        };
        assert_eq!(transform.apply(0, 0), (0, 319));
        assert_eq!(transform.apply(319, 479), (479, 0));
        assert_eq!(transform.apply(100, 200), (200, 219));
    }

    #[test]
    fn transform_clamps_to_the_screen() {
        assert_eq!(IDENTITY.apply(4000, 4000), (319, 479));
        let mirrored = TouchTransform {
            mirror_x: true,
            ..IDENTITY
        };
        assert_eq!(mirrored.apply(4000, 0), (0, 0));
    }
}
