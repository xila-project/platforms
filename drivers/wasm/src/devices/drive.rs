//! Main-thread browser disk with optional origin-private file storage.
//!
//! All reads and writes use a disk-sized memory cache. Persistent disks hold
//! a Web Lock and save atomic snapshots through [`DriveDevice::sync`], with at
//! most one extra snapshot in flight. Memory disks have no browser storage.

#[cfg(target_feature = "atomics")]
compile_error!("The browser drive requires a single browser thread");

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectBlockDevice, Error,
    MountOperations, Path, Position, Size,
    block_device::{GET_BLOCK_COUNT, GET_BLOCK_SIZE},
    mount::MutexMountWrapper,
};
use js_sys::Uint8Array;
use shared::AnyByLayout;
use synchronization::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    DomException, File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetFileOptions,
    FileSystemWritableFileStream,
};

const BLOCK_SIZE: u32 = 512;

// A reservation can be released even if its Rust opening future is cancelled
// before the Web Locks callback runs. No JS promise is synchronously blocked.
#[wasm_bindgen(inline_js = "
const releasingDrives = new Map();
export function reserveDrive(name) {
    if (!globalThis.navigator?.storage?.getDirectory || !navigator.locks) {
        throw new DOMException('OPFS and Web Locks require a secure browser context', 'NotSupportedError');
    }
    let released = false;
    let unlock;
    let request;
    const ready = new Promise((resolve, reject) => {
        const acquire = () => {
            request = navigator.locks.request('xila-opfs:' + name, {ifAvailable: true}, lock => {
                if (released) {
                    reject(new DOMException('Opening was cancelled', 'AbortError'));
                    return;
                }
                if (!lock) {
                    reject(new DOMException('Disk is already open', 'NoModificationAllowedError'));
                    return;
                }
                return new Promise(release => {
                    unlock = release;
                    resolve();
                });
            }).catch(reject);
        };
        const previous = releasingDrives.get(name);
        if (previous) previous.then(acquire, acquire);
        else acquire();
    });
    // A cancelled Rust future may no longer observe rejection.
    ready.catch(() => {});
    return {ready, release() {
        released = true;
        if (request) {
            releasingDrives.set(name, request);
            request.then(() => {
                if (releasingDrives.get(name) === request) releasingDrives.delete(name);
            });
        }
        if (unlock) unlock();
    }};
}
")]
extern "C" {
    type Reservation;

    #[wasm_bindgen(catch, js_name = reserveDrive)]
    fn reserve_drive(name: &str) -> Result<Reservation, JsValue>;

    #[wasm_bindgen(method, getter)]
    fn ready(this: &Reservation) -> js_sys::Promise;

    #[wasm_bindgen(method)]
    fn release(this: &Reservation);
}

#[wasm_bindgen(module = "/src/devices/seed.mjs")]
extern "C" {
    #[wasm_bindgen(catch, js_name = importSeed)]
    async fn import_seed(url: &str, data: &Uint8Array) -> Result<(), JsValue>;

    #[wasm_bindgen(catch, js_name = fingerprintSeed)]
    async fn fingerprint_seed(data: &Uint8Array) -> Result<alloc::string::String, JsValue>;
}

struct Lease(Reservation);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn map_error(value: JsValue) -> Error {
    match value
        .dyn_ref::<DomException>()
        .map(DomException::name)
        .as_deref()
    {
        Some("QuotaExceededError") => Error::NoSpaceLeft,
        Some("NotAllowedError" | "SecurityError") => Error::PermissionDenied,
        Some("NotFoundError") => Error::NotFound,
        Some("NoModificationAllowedError" | "InvalidStateError") => Error::RessourceBusy,
        Some("NotSupportedError") => Error::UnsupportedOperation,
        Some("TypeMismatchError") => Error::InvalidFile,
        _ => Error::InputOutput,
    }
}

struct Persistence {
    handle: FileSystemFileHandle,
    generation: u64,
    saved_generation: u64,
    _lease: Lease,
}

struct Inner {
    data: Uint8Array,
    persistence: Option<Persistence>,
}

// SAFETY: this module rejects atomics-enabled WASM; all JS objects and device
// operations stay on the single browser thread. Cache access is also guarded.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

struct State {
    inner: MutexMountWrapper<CriticalSectionRawMutex, Inner>,
    saving: Mutex<CriticalSectionRawMutex, ()>,
    accepting_writes: AtomicBool,
}

impl State {
    async fn sync(&self) -> file_system::Result<()> {
        let _save = self.saving.lock().await;
        let (handle, snapshot, generation) = {
            let inner = self.inner.try_get()?;
            let Some(persistence) = &inner.persistence else {
                return Ok(());
            };
            if persistence.generation == persistence.saved_generation {
                return Ok(());
            }
            (
                persistence.handle.clone(),
                inner.data.slice(0, inner.data.length()),
                persistence.generation,
            )
        };

        let stream = JsFuture::from(handle.create_writable())
            .await
            .map_err(map_error)?
            .dyn_into::<FileSystemWritableFileStream>()
            .map_err(map_error)?;

        let result = async {
            JsFuture::from(
                stream
                    .write_with_js_u8_array(&snapshot)
                    .map_err(map_error)?,
            )
            .await
            .map_err(map_error)?;
            // Closing, not writing, commits the replacement file atomically.
            JsFuture::from(stream.close()).await.map_err(map_error)?;
            Ok(())
        }
        .await;

        if result.is_err() {
            let _ = JsFuture::from(stream.abort()).await;
            return result;
        }

        // Later writes have a different generation and still need a save.
        if let Some(persistence) = &mut self.inner.try_get()?.persistence {
            persistence.saved_generation = generation;
        }
        Ok(())
    }
}

pub struct DriveDevice(Arc<State>, bool);

impl DriveDevice {
    fn validate_size(size: usize) -> file_system::Result<()> {
        if size == 0 || !size.is_multiple_of(BLOCK_SIZE as usize) || size > isize::MAX as usize {
            return Err(Error::InvalidParameter);
        }
        Ok(())
    }

    fn from_cache(data: Uint8Array, persistence: Option<Persistence>, is_new: bool) -> Self {
        Self(
            Arc::new(State {
                inner: MutexMountWrapper::new_mounted(Inner { data, persistence }),
                saving: Mutex::new(()),
                accepting_writes: AtomicBool::new(true),
            }),
            is_new,
        )
    }

    /// Load a fresh image into memory without opening browser storage or
    /// acquiring a Web Lock. Each drive has its own independent cache.
    pub async fn new_memory_from_seed(size: usize, seed_url: &str) -> file_system::Result<Self> {
        Self::validate_size(size)?;
        let data = Uint8Array::new_with_length(size as u32);
        import_seed(seed_url, &data).await.map_err(map_error)?;
        Ok(Self::from_cache(data, None, true))
    }

    /// Hash the bytes in a fresh memory drive before mounting or modifying it.
    pub async fn seed_fingerprint(&self) -> file_system::Result<alloc::string::String> {
        let data = self.0.inner.try_get()?.data.clone();
        fingerprint_seed(&data).await.map_err(map_error)
    }

    /// Open a persistent disk, using this memory image for a new disk or an
    /// explicitly requested rebuild. A rebuild replaces the committed image
    /// only when the caller successfully saves the replacement with `sync`.
    pub async fn into_persistent(self, name: &Path, rebuild: bool) -> file_system::Result<Self> {
        let data = self.0.inner.try_get()?.data.clone();
        Self::open_persistent(name, data.length() as usize, Some(data), rebuild).await
    }

    /// Open a fixed-size OPFS disk. `name` must be a single filename and `size`
    /// a nonzero multiple of 512. A nonempty image must already have this size.
    /// Opening does not erase or resize an existing disk.
    pub async fn new(name: &Path, size: usize) -> file_system::Result<Self> {
        Self::validate_size(size)?;
        Self::open_persistent(name, size, None, false).await
    }

    async fn open_persistent(
        name: &Path,
        size: usize,
        seed: Option<Uint8Array>,
        rebuild: bool,
    ) -> file_system::Result<Self> {
        let name = name.as_str();
        if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\', '\0']) {
            return Err(Error::InvalidPath);
        }

        let lease = Lease(reserve_drive(name).map_err(map_error)?);
        JsFuture::from(lease.0.ready()).await.map_err(map_error)?;

        let storage = web_sys::window()
            .ok_or(Error::UnsupportedOperation)?
            .navigator()
            .storage();
        let directory = JsFuture::from(storage.get_directory())
            .await
            .map_err(map_error)?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(map_error)?;
        let options = FileSystemGetFileOptions::new();
        options.set_create(true);
        let handle = JsFuture::from(directory.get_file_handle_with_options(name, &options))
            .await
            .map_err(map_error)?
            .dyn_into::<FileSystemFileHandle>()
            .map_err(map_error)?;
        let file = JsFuture::from(handle.get_file())
            .await
            .map_err(map_error)?
            .dyn_into::<File>()
            .map_err(map_error)?;
        let is_new = rebuild || file.size() == 0.0;
        let data = if is_new {
            seed.unwrap_or_else(|| Uint8Array::new_with_length(size as u32))
        } else {
            if file.size() != size as f64 {
                return Err(Error::InvalidParameter);
            }
            Uint8Array::new(
                &JsFuture::from(file.array_buffer())
                    .await
                    .map_err(map_error)?,
            )
        };

        Ok(Self::from_cache(
            data,
            Some(Persistence {
                handle,
                generation: u64::from(is_new),
                saved_generation: 0,
                _lease: lease,
            }),
            is_new,
        ))
    }

    /// Import a reference image into an uncommitted cache only for an absent
    /// or zero-byte disk. The caller must validate/mount it before `sync`.
    /// Existing disks never fetch a reference image.
    pub async fn new_seeded(name: &Path, size: usize, seed_url: &str) -> file_system::Result<Self> {
        let drive = Self::new(name, size).await?;
        if drive.is_new() {
            let data = drive.0.inner.try_get()?.data.clone();
            import_seed(seed_url, &data).await.map_err(map_error)?;
        }
        Ok(drive)
    }

    /// Abandon the cache without committing it and release the disk lease.
    /// Intended for startup failures before the filesystem becomes usable.
    /// Unlike `unmount`, this explicitly discards unsaved changes. A running
    /// save must finish first; this never races an OPFS commit.
    pub fn discard_unsaved(&self) -> file_system::Result<()> {
        let _save = self.0.saving.try_lock().map_err(|_| Error::RessourceBusy)?;
        self.0.inner.unmount()
    }

    /// Whether the cache started fresh, from a seed or an empty disk,
    /// rather than from an existing installation. Saves do not change this.
    pub const fn is_new(&self) -> bool {
        self.1
    }

    /// Commit a consistent cache snapshot to OPFS, or return a storage error.
    /// Writes made during this save remain dirty for the next call. Saves keep
    /// running if the waiting future is cancelled, retaining the lease and save
    /// mutex until completion. Dropping the device does not save dirty data.
    /// On a memory drive this does nothing.
    pub async fn sync(&self) -> file_system::Result<()> {
        if self.0.inner.try_get()?.persistence.is_none() {
            return Ok(());
        }
        let completion =
            Arc::new(Signal::<CriticalSectionRawMutex, file_system::Result<()>>::new());
        let state = self.0.clone();
        let result = completion.clone();
        spawn_local(async move {
            result.signal(state.sync().await);
        });
        completion.wait().await
    }

    /// Temporarily reject writes so the browser host can safely save and
    /// release this disk before switching to another image.
    pub fn stop_writes(&self) {
        self.0.accepting_writes.store(false, Ordering::Release);
    }

    /// Resume writes when a requested disk switch could not be saved.
    pub fn resume_writes(&self) {
        self.0.accepting_writes.store(true, Ordering::Release);
    }
}

impl DirectBaseOperations for DriveDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        position: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| {
            let inner = self.0.inner.try_get()?;
            let position = u32::try_from(position).map_err(|_| Error::InvalidParameter)?;
            let size = inner.data.length();
            if position > size {
                return Err(Error::InvalidParameter);
            }
            let count = buffer.len().min((size - position) as usize);
            inner
                .data
                .subarray(position, position + count as u32)
                .copy_to(&mut buffer[..count]);
            Ok(count)
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        position: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| {
            if !self.0.accepting_writes.load(Ordering::Acquire) {
                return Err(Error::RessourceBusy);
            }
            let mut inner = self.0.inner.try_get()?;
            let position = u32::try_from(position).map_err(|_| Error::InvalidParameter)?;
            let size = inner.data.length();
            if position > size {
                return Err(Error::InvalidParameter);
            }
            let count = buffer.len().min((size - position) as usize);
            if count != 0 {
                inner
                    .data
                    .subarray(position, position + count as u32)
                    .copy_from(&buffer[..count]);
                if let Some(persistence) = &mut inner.persistence {
                    persistence.generation = persistence.generation.wrapping_add(1);
                }
            }
            Ok(count)
        })
    }

    fn set_position(&self, current: Size, position: &Position) -> file_system::Result<Size> {
        let inner = self.0.inner.try_get()?;
        file_system::block_device::set_position(current, position, inner.data.length() as Size)
    }

    /// Cache-only flush. OPFS durability requires awaiting `sync()`.
    fn flush(
        &self,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| {
            let _inner = self.0.inner.try_get()?;
            Ok(())
        })
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        _: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| {
            let inner = self.0.inner.try_get()?;
            match command {
                GET_BLOCK_SIZE::IDENTIFIER => *GET_BLOCK_SIZE::cast_output(output)? = BLOCK_SIZE,
                GET_BLOCK_COUNT::IDENTIFIER => {
                    *GET_BLOCK_COUNT::cast_output(output)? = inner.data.length() / BLOCK_SIZE;
                }
                _ => return Err(Error::UnsupportedOperation),
            }
            Ok(())
        })
    }
}

impl MountOperations for DriveDevice {
    /// Unmount only a clean cache when no save is running. Await `sync()` first.
    fn unmount(&self) -> file_system::Result<()> {
        let _save = self.0.saving.try_lock().map_err(|_| Error::RessourceBusy)?;
        {
            let inner = self.0.inner.try_get()?;
            if inner
                .persistence
                .as_ref()
                .is_some_and(|persistence| persistence.generation != persistence.saved_generation)
            {
                return Err(Error::RessourceBusy);
            }
        }
        self.0.inner.unmount()
    }
}

impl DirectBlockDevice for DriveDevice {}
