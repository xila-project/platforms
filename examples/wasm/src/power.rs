//! Browser driver stop: end autosave, commit OPFS and release the disk lock.
//! Runs after the lifecycle unmounted LittleFS, before the power device acts.
use core::sync::atomic::{AtomicBool, Ordering};
use xila::file_system::MountOperations;
use xila::lifecycle::{Drivers, Error};
use xila::task::JoinHandle;

pub struct BrowserDrivers {
    drive: &'static drivers_wasm::devices::DriveDevice,
    autosave_running: &'static AtomicBool,
    autosave: Option<JoinHandle<()>>,
}

impl BrowserDrivers {
    pub fn new(
        drive: &'static drivers_wasm::devices::DriveDevice,
        autosave_running: &'static AtomicBool,
        autosave: JoinHandle<()>,
    ) -> Self {
        Self {
            drive,
            autosave_running,
            autosave: Some(autosave),
        }
    }
}

impl Drivers for BrowserDrivers {
    async fn stop(&mut self) -> Result<(), Error> {
        self.autosave_running.store(false, Ordering::Release);
        if let Some(autosave) = self.autosave.take() {
            autosave.join().await;
        }
        self.drive.stop_writes();
        // A failed commit keeps the lock and the page: nothing is reloaded.
        self.drive.sync().await.map_err(|_| Error::Driver)?;
        self.drive.unmount().map_err(|_| Error::Driver)
    }
}
