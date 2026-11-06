use task::TaskIdentifier;
use virtual_file_system::VirtualFileSystem;

use crate::pin_line;
use crate::shared::{Gpio, Line, Result};

/// The ESP32-S3 chip features 45 physical GPIO pins:
/// - GPIO0 ~ GPIO21
/// - GPIO26 ~ GPIO48
static LINES: [Line; 45] = [
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
    pin_line!(20),
    pin_line!(21),
    pin_line!(26),
    pin_line!(27),
    pin_line!(28),
    pin_line!(29),
    pin_line!(30),
    pin_line!(31),
    pin_line!(32),
    pin_line!(33),
    pin_line!(34),
    pin_line!(35),
    pin_line!(36),
    pin_line!(37),
    pin_line!(38),
    pin_line!(39),
    pin_line!(40),
    pin_line!(41),
    pin_line!(42),
    pin_line!(43),
    pin_line!(44),
    pin_line!(45),
    pin_line!(46),
    pin_line!(47),
    pin_line!(48),
];

/// GPIO controller for the ESP32-S3, shared by all drivers of the platform.
pub static GPIO: Gpio = Gpio::new(&LINES);

/// Mounts `/devices/Pin<number>` for every GPIO line of the ESP32-S3.
pub async fn mount_pin_devices(
    virtual_file_system: &VirtualFileSystem,
    task: TaskIdentifier,
) -> Result<()> {
    crate::shared::mount_pin_devices(virtual_file_system, task, &GPIO).await
}
