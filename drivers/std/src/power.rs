//! Hosted power device: powering off ends only this Xila process.

use peripherals::power::{Error, PowerController};
use std::io::Write;

/// Power controller of a hosted runtime. `power_off` exits this process with
/// the requested status; restart and sleep are unsupported (a host supervisor
/// may start a fresh process). The host machine is never powered off.
pub struct ProcessPower;

impl PowerController for ProcessPower {
    fn capabilities(&self) -> u8 {
        // `device::power::Capabilities::POWER_OFF`.
        1
    }

    fn power_off(&mut self, status: u8) -> Result<(), Error> {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(i32::from(status));
    }
}
