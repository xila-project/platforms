use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};

use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectBlockDevice,
    MountOperations, Path, Position, Size,
    block_device::{GET_BLOCK_COUNT, GET_BLOCK_SIZE},
    mount::MutexMountWrapper,
};
use io_error::Kind;
use shared::AnyByLayout;
use synchronization::blocking_mutex::raw::CriticalSectionRawMutex;

use crate::io::map_error;

pub struct FileDriveDevice(MutexMountWrapper<CriticalSectionRawMutex, File>);

impl FileDriveDevice {
    /// Open a committed disk without creating or resizing it.
    pub fn open_existing(path: &std::path::Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self(MutexMountWrapper::new_mounted(file)))
    }

    pub fn new(path: &impl AsRef<Path>, size: Size) -> Self {
        let path = path.as_ref().as_str();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("Error opening file");

        file.set_len(size).expect("Error setting file size");

        Self(MutexMountWrapper::new_mounted(file))
    }

    pub fn new_static(path: &impl AsRef<Path>, size: Size) -> &'static Self {
        Box::leak(Box::new(Self::new(path, size)))
    }
}

impl DirectBaseOperations for FileDriveDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        position: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            let mut inner = self.0.try_get()?;

            inner.seek(SeekFrom::Start(position)).map_err(map_error)?;
            inner.read(buffer).map_err(map_error)
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        position: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            let mut inner = self.0.try_get()?;

            inner.seek(SeekFrom::Start(position)).map_err(map_error)?;
            inner.write(buffer).map_err(map_error)
        })
    }

    fn write_pattern(
        &self,
        _cx: &mut core::task::Context<'_>,
        pattern: &[u8],
        count: usize,
        absolute_position: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            let mut inner = self.0.try_get()?;

            inner
                .seek(SeekFrom::Start(absolute_position))
                .map_err(map_error)?;

            for _ in 0..count {
                inner.write_all(pattern).map_err(map_error)?;
            }

            Ok(pattern.len() * count)
        })
    }

    fn set_position(&self, _: Size, position: &Position) -> io_error::Result<Size> {
        let position = match position {
            Position::Start(position) => SeekFrom::Start(*position),
            Position::End(position) => SeekFrom::End(*position),
            Position::Current(position) => SeekFrom::Current(*position),
        };

        self.0
            .try_get()
            .map_err(|_| Kind::WouldBlock)?
            .seek(position)
            .map_err(map_error)
    }

    fn flush(&self, _cx: &mut core::task::Context<'_>) -> core::task::Poll<io_error::Result<()>> {
        file_system::ready(|| {
            self.0
                .try_get()
                .map_err(|_| Kind::WouldBlock)?
                .sync_all()
                .map_err(map_error)
        })
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        _: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<io_error::Result<()>> {
        file_system::ready(|| {
            match command {
                GET_BLOCK_SIZE::IDENTIFIER => {
                    let block_size = GET_BLOCK_SIZE::cast_output(output)?;
                    *block_size = 512; // Fixed block size for file drive device

                    Ok(())
                }
                GET_BLOCK_COUNT::IDENTIFIER => {
                    let block_count = GET_BLOCK_COUNT::cast_output(output)?;

                    let file_size = self
                        .0
                        .try_get()
                        .map_err(|_| Kind::WouldBlock)?
                        .metadata()
                        .map_err(map_error)?
                        .len();

                    *block_count = (file_size / 512) as _; // Fixed block size for file drive device

                    Ok(())
                }
                _ => Err(Kind::Unsupported),
            }
        })
    }
}

impl MountOperations for FileDriveDevice {
    fn unmount(&self) -> io_error::Result<()> {
        self.0.unmount()
    }
}

impl DirectBlockDevice for FileDriveDevice {}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::implement_block_device_tests;

    fn create_test_device() -> FileDriveDevice {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        // A per-process file in the system temporary directory, so parallel
        // runs and sandboxes without a writable `/tmp` do not collide.
        let path = std::env::temp_dir().join(format!(
            "xila-file-drive-device-test-{}-{id}.img",
            std::process::id()
        ));
        let path = path.to_str().expect("temporary directory is not UTF-8");

        let device = FileDriveDevice::new(&Path::from_str(path), 16 * 1024 * 1024);
        // The device keeps its file open; unlinking it now leaves nothing
        // behind on Unix. Windows refuses to remove an open file, so it may stay.
        let _ = std::fs::remove_file(path);
        device
    }

    implement_block_device_tests!(create_test_device());
}
