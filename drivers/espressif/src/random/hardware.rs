use esp_hal::{
    peripherals::{ADC1, RNG},
    rng::{Trng, TrngSource},
};
use file_system::IoErrorKind;
use static_cell::StaticCell;

use super::RandomDevice;

static ENTROPY_SOURCE: StaticCell<TrngSource<'static>> = StaticCell::new();

impl RandomDevice {
    /// Enables the analog entropy source for the lifetime of the firmware.
    ///
    /// Call once at startup. The random number generator and analog-to-digital
    /// converter remain reserved even if the character device is unmounted.
    pub fn new(random: RNG<'static>, converter: ADC1<'static>) -> Self {
        ENTROPY_SOURCE.init_with(|| TrngSource::new(random, converter));

        Self::from_entropy_source(|buffer| {
            let random = Trng::try_new().map_err(|_| IoErrorKind::DeviceUnavailable)?;
            random.read(buffer);
            Ok(())
        })
    }
}
