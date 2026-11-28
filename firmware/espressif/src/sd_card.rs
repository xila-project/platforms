//! The microSD slot of a board: probe at boot, expose, mount.
//!
//! The slot has no card-detect line, so the firmware looks for a card once,
//! at boot. A card inserted or removed later is not noticed until the next
//! boot.
//!
//! - The card appears as the block device `/devices/storage/sd_card`, readable
//!   and writable by its owner only.
//! - A card that holds LittleFS is mounted at `/media/sd_card`. The root of
//!   a new card becomes a shared directory: every user can create entries,
//!   and only the owner of an entry can remove or rename it.
//! - The firmware never formats a card by itself. A card from another device
//!   holds files, so a card without LittleFS stays unmounted. The user formats
//!   it by creating [`FORMAT_REQUEST_PATH`] with the text `erase` and
//!   rebooting. The firmware removes the file before it acts on it, so one
//!   request formats one card, once.
//! - The file [`TEST_REQUEST_PATH`] asks for a write and read-back test of the
//!   mounted card at the next boot. The firmware removes it likewise.

use alloc::{boxed::Box, string::String, vec};
use core::time::Duration;

use xila::io_error::Kind;
use xila::{
    file_system::{AccessFlags, CreateFlags, Flags, Permissions},
    little_fs, log, task, time,
    virtual_file_system::{File, ItemStatic, VirtualFileSystem, ignore_already_exists_error},
};

/// The block device of the card.
pub const DEVICE_PATH: &str = "/devices/storage/sd_card";

/// The parent of the mount point.
const MEDIA_PATH: &str = "/media";

/// Where a card that holds LittleFS is mounted.
pub const MOUNT_PATH: &str = "/media/sd_card";

/// A file that asks the next boot to format the card with LittleFS. It is a
/// workaround until Core can format a device from a shell command: see
/// xila-project/platforms#219.
pub const FORMAT_REQUEST_PATH: &str = "/configuration/system/format_sd_card";

/// The text that [`FORMAT_REQUEST_PATH`] must hold. It makes an empty file,
/// created by `touch` or by mistake, harmless.
const FORMAT_CONFIRMATION: &str = "erase";

/// A file that asks the next boot to test the mounted card.
pub const TEST_REQUEST_PATH: &str = "/configuration/system/test_sd_card";

/// LittleFS cache: one sector, a divisor of the 4096-byte block of the device.
/// Core's LittleFS also uses it as the size of the lookahead bitmap.
const CACHE_SIZE: usize = 512;

/// The size of the file of the test, and of each transfer.
const TEST_FILE_SIZE: usize = 256 * 1024;
const TEST_CHUNK_SIZE: usize = 4096;
const TEST_FILE_PATH: &str = "/media/sd_card/xila_test.bin";

/// The card that a board opened.
pub struct OpenedSdCard {
    /// The card type, capacity and bus mode, for the boot log.
    pub description: String,
    direct: &'static dyn xila::file_system::DirectBlockDevice,
    block: &'static dyn xila::file_system::BlockDevice,
}

impl OpenedSdCard {
    /// Describes `device`, a block device that reports 4096-byte blocks.
    pub fn new<D: xila::file_system::DirectBlockDevice>(
        description: String,
        device: &'static D,
    ) -> Self {
        Self {
            description,
            direct: device,
            block: device,
        }
    }
}

/// How the firmware opens the card of a board.
type Opener = Box<dyn FnOnce() -> Result<OpenedSdCard, String> + Send>;

/// The microSD slot of a board, not yet probed.
pub struct SdCardSlot {
    opener: Opener,
}

impl SdCardSlot {
    /// `open` initializes the card. It runs once, at boot, and returns the
    /// reason as text when no card answers.
    pub fn new(open: impl FnOnce() -> Result<OpenedSdCard, String> + Send + 'static) -> Self {
        Self {
            opener: Box::new(open),
        }
    }
}

/// What the user asked for with a request file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    None,
    Confirmed,
}

/// Reads and removes the request file `path`. A file whose text is not
/// `confirmation` is removed without effect.
async fn take_request(
    virtual_file_system: &VirtualFileSystem,
    task: task::TaskIdentifier,
    path: &str,
    confirmation: Option<&str>,
) -> Request {
    let mut buffer = [0u8; 32];
    let length = match File::open(
        virtual_file_system,
        task,
        path,
        Flags::from(AccessFlags::Read),
    )
    .await
    {
        Ok(mut file) => {
            let result = file.read(&mut buffer).await;
            let _ = file.close(virtual_file_system).await;
            match result {
                Ok(length) => length,
                Err(error) => {
                    log::warning!("Xila: SD card: cannot read {path}: {error}");
                    0
                }
            }
        }
        Err(Kind::NotFound) => return Request::None,
        Err(error) => {
            log::warning!("Xila: SD card: cannot open {path}: {error}");
            return Request::None;
        }
    };
    // The request is one use only: remove it first. If that fails, do not act
    // on it, or every boot would repeat it.
    if let Err(error) = virtual_file_system.remove(task, path).await {
        log::warning!("Xila: SD card: cannot remove {path}: {error}; the request is ignored");
        return Request::None;
    }
    let text = core::str::from_utf8(&buffer[..length]).unwrap_or("").trim();
    match confirmation {
        Some(expected) if text != expected => {
            log::warning!("Xila: SD card: {path} must hold the text \"{expected}\"; ignored");
            Request::None
        }
        _ => Request::Confirmed,
    }
}

/// Probes the slot and, with a card, exposes and mounts it. Logs one line when
/// there is no card.
pub(super) async fn start(
    slot: SdCardSlot,
    virtual_file_system: &'static VirtualFileSystem,
    task: task::TaskIdentifier,
    time_manager: &'static time::Manager<'static>,
) {
    let format = take_request(
        virtual_file_system,
        task,
        FORMAT_REQUEST_PATH,
        Some(FORMAT_CONFIRMATION),
    )
    .await;
    let test = take_request(virtual_file_system, task, TEST_REQUEST_PATH, None).await;

    let card = match (slot.opener)() {
        Ok(card) => card,
        Err(reason) => {
            log::information!("Xila: SD card: none ({reason})");
            if format == Request::Confirmed {
                log::warning!("Xila: SD card: the format request is discarded: no card");
            }
            return;
        }
    };
    log::information!("Xila: SD card: {}", card.description);

    if let Err(error) = virtual_file_system
        .mount_static_with_permissions(
            task,
            DEVICE_PATH,
            ItemStatic::BlockDevice(card.block),
            Permissions::USER_READ_WRITE,
        )
        .await
    {
        log::error!("Xila: SD card: cannot expose {DEVICE_PATH}: {error}");
        return;
    }

    let options = little_fs::Options::new(CACHE_SIZE);
    let file_system = if format == Request::Confirmed {
        log::warning!("Xila: SD card: formatting with LittleFS, as requested");
        little_fs::FileSystem::new_format(card.direct, options)
    } else {
        little_fs::FileSystem::new(card.direct, options)
    };
    let file_system = match file_system {
        Ok(file_system) => file_system,
        Err(error) if format == Request::Confirmed => {
            log::error!("Xila: SD card: format failed: {error}");
            return;
        }
        Err(_) => {
            log::warning!(
                "Xila: SD card: not formatted for Xila, so not mounted; to format it, write \
                 \"{FORMAT_CONFIRMATION}\" to {FORMAT_REQUEST_PATH} and reboot (this erases the card)"
            );
            return;
        }
    };

    if let Err(error) = ignore_already_exists_error(
        virtual_file_system
            .create_directory(task, &MEDIA_PATH)
            .await,
    ) {
        log::error!("Xila: SD card: cannot create {MEDIA_PATH}: {error}");
        return;
    }
    if let Err(error) = ignore_already_exists_error(
        virtual_file_system
            .create_directory(task, &MOUNT_PATH)
            .await,
    ) {
        log::error!("Xila: SD card: cannot create {MOUNT_PATH}: {error}");
        return;
    }
    if let Err(error) = virtual_file_system
        .mount_file_system(file_system, MOUNT_PATH, task)
        .await
    {
        log::error!("Xila: SD card: cannot mount {MOUNT_PATH}: {error}");
        return;
    }
    log::information!("Xila: SD card: LittleFS mounted at {MOUNT_PATH}");

    share_new_root(virtual_file_system, task).await;

    if test == Request::Confirmed {
        run_test(virtual_file_system, task, time_manager).await;
    }
}

/// Makes the root of a new card a shared directory, as `/data/shared`: every
/// user can create entries, and only the owner of an entry can remove or
/// rename it.
///
/// Core's LittleFS gives the root of a new file system to root, with no
/// permissions, so only root could use the card. The firmware changes the
/// root only while it still has no permissions: on the first mount after a
/// format, including a format by an earlier firmware. A later change by the
/// root user stays, except a change back to no permissions: the next boot
/// shares the root directory again. This is the only write to the card at boot without a request.
async fn share_new_root(virtual_file_system: &VirtualFileSystem, task: task::TaskIdentifier) {
    let permissions = match virtual_file_system
        .get_statistics_for_task(task, &MOUNT_PATH)
        .await
    {
        Ok(statistics) => statistics.permissions,
        Err(error) => {
            log::warning!("Xila: SD card: cannot read the permissions of {MOUNT_PATH}: {error}");
            return;
        }
    };
    if permissions != Permissions::NONE {
        return;
    }
    if let Err(error) = virtual_file_system
        .set_permissions(task, &MOUNT_PATH, Permissions::SHARED_DIRECTORY)
        .await
    {
        log::warning!("Xila: SD card: cannot set the permissions of {MOUNT_PATH}: {error}");
    }
}

/// Writes a file of [`TEST_FILE_SIZE`] bytes to the card, reads it back,
/// compares it and removes it. Logs the result and the speeds.
async fn run_test(
    virtual_file_system: &'static VirtualFileSystem,
    task: task::TaskIdentifier,
    time_manager: &'static time::Manager<'static>,
) {
    match write_and_verify(virtual_file_system, task, time_manager).await {
        Ok((written, read)) => log::information!(
            "Xila: SD card test: passed; wrote {} KiB in {} ms, read it back in {} ms",
            TEST_FILE_SIZE / 1024,
            written.as_millis(),
            read.as_millis(),
        ),
        Err(failure) => log::error!("Xila: SD card test: failed; {failure}"),
    }
    let _ = virtual_file_system.remove(task, TEST_FILE_PATH).await;
}

/// The chunk `index` of the test file: a pattern that differs from chunk to
/// chunk, so that a sector written to the wrong place is found.
fn test_chunk(index: usize, chunk: &mut [u8]) {
    for (position, byte) in chunk.iter_mut().enumerate() {
        *byte = (position as u8)
            .wrapping_mul(7)
            .wrapping_add((index as u8).wrapping_mul(13))
            .wrapping_add((index >> 8) as u8);
    }
}

async fn write_and_verify(
    virtual_file_system: &'static VirtualFileSystem,
    task: task::TaskIdentifier,
    time_manager: &'static time::Manager<'static>,
) -> Result<(Duration, Duration), String> {
    use alloc::format;

    let chunks = TEST_FILE_SIZE / TEST_CHUNK_SIZE;
    let mut buffer = vec![0u8; TEST_CHUNK_SIZE];

    let started = time_manager.monotonic_now();
    let mut file = File::open(
        virtual_file_system,
        task,
        TEST_FILE_PATH,
        Flags::new(AccessFlags::Write, Some(CreateFlags::CREATE_TRUNCATE), None),
    )
    .await
    .map_err(|error| format!("cannot create the file: {error}"))?;
    for index in 0..chunks {
        test_chunk(index, &mut buffer);
        let count = file
            .write(&buffer)
            .await
            .map_err(|error| format!("write of chunk {index}: {error}"))?;
        if count != buffer.len() {
            return Err(format!("short write of chunk {index}: {count} bytes"));
        }
    }
    file.close(virtual_file_system)
        .await
        .map_err(|error| format!("cannot close the file: {error}"))?;
    let written = time_manager.monotonic_now().saturating_sub(started);

    let started = time_manager.monotonic_now();
    let mut file = File::open(
        virtual_file_system,
        task,
        TEST_FILE_PATH,
        Flags::from(AccessFlags::Read),
    )
    .await
    .map_err(|error| format!("cannot reopen the file: {error}"))?;
    let mut expected = vec![0u8; TEST_CHUNK_SIZE];
    for index in 0..chunks {
        let mut done = 0;
        while done < buffer.len() {
            let count = file
                .read(&mut buffer[done..])
                .await
                .map_err(|error| format!("read of chunk {index}: {error}"))?;
            if count == 0 {
                return Err(format!("the file ends in chunk {index}"));
            }
            done += count;
        }
        test_chunk(index, &mut expected);
        if buffer != expected {
            return Err(format!("chunk {index} differs from what was written"));
        }
    }
    let _ = file.close(virtual_file_system).await;
    let read = time_manager.monotonic_now().saturating_sub(started);

    Ok((written, read))
}
