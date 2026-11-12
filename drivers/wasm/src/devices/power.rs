//! Browser power device: Xila never closes or navigates away from the page.

use peripherals::power::{Error, PowerController};

/// Power controller of a browser instance. `power_off` leaves the halted
/// instance on the page; `restart` reloads the page to start a fresh instance.
/// Both run only after the lifecycle committed the disk and released its lock.
pub struct BrowserPower;

impl PowerController for BrowserPower {
    fn capabilities(&self) -> u8 {
        // `device::power::Capabilities::POWER_OFF | RESTART`.
        1 | 2
    }

    fn power_off(&mut self, _status: u8) -> Result<(), Error> {
        Ok(())
    }

    fn restart(&mut self, _status: u8) -> Result<(), Error> {
        web_sys::window()
            .ok_or(Error::Other)?
            .location()
            .reload()
            .map_err(|_| Error::Other)
    }
}
