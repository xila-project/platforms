use task::TaskIdentifier;
use virtual_file_system::VirtualFileSystem;

use crate::pin_input_only;
use crate::pin_line;
use crate::shared::{Gpio, Line, Result};

/// The ESP32 chip features 34 physical GPIO pins:
/// - GPIO0 ~ GPIO19
/// - GPIO21 ~ GPIO23
/// - GPIO25 ~ GPIO27
/// - GPIO32 ~ GPIO39
///
/// GPIO34 ~ GPIO39 are input-only and have no pull resistors.
static LINES: [Line; 34] = [
    pin_line!(0),
    pin_line!(1),
    pin_line!(2),
    pin_line!(3),
    pin_line!(4),
    pin_line!(5),
    pin_line!(6),
    pin_line!(7),
    pin_line!(8),
    pin_line!(9),
    pin_line!(10),
    pin_line!(11),
    pin_line!(12),
    pin_line!(13),
    pin_line!(14),
    pin_line!(15),
    pin_line!(16),
    pin_line!(17),
    pin_line!(18),
    pin_line!(19),
    pin_line!(21),
    pin_line!(22),
    pin_line!(23),
    pin_line!(25),
    pin_line!(26),
    pin_line!(27),
    pin_line!(32),
    pin_line!(33),
    pin_input_only!(34),
    pin_input_only!(35),
    pin_input_only!(36),
    pin_input_only!(37),
    pin_input_only!(38),
    pin_input_only!(39),
];

/// GPIO controller for the ESP32, shared by all drivers of the platform.
pub static GPIO: Gpio = Gpio::new(&LINES);

/// Mounts `/devices/Pin<number>` for every GPIO line of the ESP32.
pub async fn mount_pin_devices(
    virtual_file_system: &VirtualFileSystem,
    task: TaskIdentifier,
) -> Result<()> {
    crate::shared::mount_pin_devices(virtual_file_system, task, &GPIO).await
}
