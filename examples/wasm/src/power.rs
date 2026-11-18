//! Stop the browser drive, saving and unlocking persistent disks.
//! Runs after the lifecycle unmounted LittleFS, before the power device acts.
use core::sync::atomic::{AtomicBool, Ordering};
use xila::file_system::MountOperations;
use xila::lifecycle::{Drivers, Error};
use xila::task::JoinHandle;

pub struct BrowserDrivers {
    drive: &'static drivers_wasm::devices::DriveDevice,
    autosave: Option<(&'static AtomicBool, JoinHandle<()>)>,
}

impl BrowserDrivers {
    pub fn new(
        drive: &'static drivers_wasm::devices::DriveDevice,
        autosave: Option<(&'static AtomicBool, JoinHandle<()>)>,
    ) -> Self {
        Self { drive, autosave }
    }
}

impl Drivers for BrowserDrivers {
    async fn stop(&mut self) -> Result<(), Error> {
        self.drive.stop_writes();
        if let Some((running, handle)) = self.autosave.take() {
            running.store(false, Ordering::Release);
            handle.join().await;
        }
        // A failed commit keeps the lock and the page.
        self.drive.sync().await.map_err(|_| Error::Driver)?;
        self.drive.unmount().map_err(|_| Error::Driver)
    }
}
