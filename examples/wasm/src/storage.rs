use drivers_wasm::devices::DriveDevice;
use xila::{authentication, little_fs};

/// Seeded and existing disks are already formatted. Errors never authorize erasure.
pub fn mount(drive: &'static DriveDevice) -> xila::io_error::Result<little_fs::FileSystem> {
    little_fs::mount_disk(drive, little_fs::Options::new(256))
}

pub async fn initialize_users(
    context: &authentication::Context<'_>,
    _new_disk: bool,
) -> authentication::Result<()> {
    authentication::initialize_seeded(context).await
}
