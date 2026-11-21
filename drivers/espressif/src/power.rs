use peripherals::power::{Error, PowerController};

/// Board power controller supporting software restart.
pub struct BoardPower;

impl PowerController for BoardPower {
    fn capabilities(&self) -> u8 {
        // `device::power::Capabilities::RESTART`.
        2
    }

    fn restart(&mut self, _status: u8) -> Result<(), Error> {
        esp_hal::system::software_reset()
    }
}
