//! Capture a native crash in a host file. Import it when LittleFS is ready.

use std::{
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use ::crash::{
    Architecture, BuildIdentifier, Header, Limits, NATIVE_RECORD_CAPACITY, Record, RecordKind,
    ResetReason, Writer,
};

#[cfg(unix)]
mod unix;

static STATE: OnceLock<State> = OnceLock::new();
static CAPTURED: AtomicBool = AtomicBool::new(false);

struct State {
    architecture: Architecture,
    started: Instant,
    build_identifier: BuildIdentifier,
    load_offset: u64,
    image_range: Option<std::ops::Range<u64>>,
    #[cfg(unix)]
    paths: unix::Paths,
    #[cfg(not(unix))]
    destination: PathBuf,
    #[cfg(not(unix))]
    temporary: PathBuf,
    #[cfg(unix)]
    monotonic_start: u64,
}

impl State {
    fn header(&self, kind: RecordKind, reset_reason: ResetReason) -> Header<'static> {
        Header {
            reset_reason,
            target_name: std::env::consts::OS,
            firmware_version: env!("CARGO_PKG_VERSION"),
            build_identifier: self.build_identifier,
            code_load_offset: self.load_offset,
            ..Header::new(kind, self.architecture)
        }
    }
}

/// The pending file is separate from the mounted disk image.
pub fn pending_path(disk: &Path, alternate: Option<&Path>) -> PathBuf {
    if let Some(path) = alternate {
        return path.to_owned();
    }
    let mut path = disk.as_os_str().to_owned();
    path.push(".crash.pending");
    path.into()
}

#[cfg(unix)]
pub(crate) fn install_executor_stack() -> Option<Registration> {
    STATE.get()?;
    match unix::AlternateStack::install() {
        Ok(stack) => Some(Registration {
            _alternate_stack: stack,
        }),
        Err(error) => {
            log::warning!("Failed to install an alternate crash stack: {error}");
            None
        }
    }
}

pub struct Registration {
    #[cfg(unix)]
    _alternate_stack: unix::AlternateStack,
}

/// Install the Rust panic hook and host signal handlers. Keep the returned
/// registration on this thread to retain its alternate signal stack.
pub fn install(destination: &Path) -> io::Result<Option<Registration>> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => Architecture::NativeX86_64,
        "aarch64" => Architecture::NativeAarch64,
        _ => return Ok(None),
    };
    let destination = std::path::absolute(destination)?;
    let mut temporary = destination.as_os_str().to_owned();
    temporary.push(format!(".writing.{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let (build_identifier, load_offset, image_range) = executable_identity();
    #[cfg(unix)]
    let alternate_stack = unix::AlternateStack::install()?;

    STATE
        .set(State {
            architecture,
            started: Instant::now(),
            build_identifier,
            load_offset,
            image_range,
            #[cfg(unix)]
            paths: unix::Paths::new(&destination, &temporary)?,
            #[cfg(not(unix))]
            destination,
            #[cfg(not(unix))]
            temporary,
            #[cfg(unix)]
            monotonic_start: unix::monotonic_microseconds(),
        })
        .map_err(|_| {
            io::Error::new(io::ErrorKind::AlreadyExists, "Crash reporting is installed")
        })?;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |information| {
        if let Some(state) = claim()
            && let Err(error) = capture_panic(state, information)
        {
            eprintln!("Failed to save the crash report: {error}");
        }
        previous_hook(information);
    }));
    #[cfg(unix)]
    unix::install_handlers()?;
    Ok(Some(Registration {
        #[cfg(unix)]
        _alternate_stack: alternate_stack,
    }))
}

fn claim() -> Option<&'static State> {
    let state = STATE.get()?;
    CAPTURED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .ok()?;
    Some(state)
}

fn capture_panic(state: &State, information: &std::panic::PanicHookInfo<'_>) -> io::Result<()> {
    let mut slot = [0; NATIVE_RECORD_CAPACITY];
    let active = task::active_task::snapshot();
    let header = Header {
        uptime_microseconds: state
            .started
            .elapsed()
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX),
        task_identifier: active.map_or(0, |task| task.identifier),
        task_name: active
            .as_ref()
            .map_or("", task::active_task::ActiveTask::name),
        ..state.header(RecordKind::KernelPanic, ResetReason::HostPanic)
    };
    let mut writer = Writer::new(&mut slot, &header, Limits::NATIVE).unwrap();

    let mut frames = [std::ptr::null_mut(); 64];
    let count = capture_frames(&mut frames);
    for frame in frames.iter().take(count) {
        // The collected addresses follow calls. Resolve the call site.
        let address = (*frame as u64).saturating_sub(1);
        if state
            .image_range
            .as_ref()
            .is_none_or(|range| range.contains(&address))
            && !writer.push_frame(address)
        {
            break;
        }
    }
    let bytes = writer.finish(format_args!("{information}"), &[], |output| {
        Some(log::snapshot_tail(output))
    });
    #[cfg(unix)]
    return state.paths.write(bytes);
    #[cfg(not(unix))]
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&state.temporary)?;
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        result.and_then(|()| std::fs::rename(&state.temporary, &state.destination))
    }
}

#[cfg(unix)]
fn capture_frames(frames: &mut [*mut std::ffi::c_void]) -> usize {
    // SAFETY: The writable array holds every pointer libc can return.
    unsafe { libc::backtrace(frames.as_mut_ptr(), frames.len() as i32) }.max(0) as usize
}

#[cfg(windows)]
fn capture_frames(frames: &mut [*mut std::ffi::c_void]) -> usize {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn RtlCaptureStackBackTrace(
            frames_to_skip: u32,
            frames_to_capture: u32,
            backtrace: *mut *mut std::ffi::c_void,
            backtrace_hash: *mut u32,
        ) -> u16;
    }
    // SAFETY: The writable array holds every pointer the system can return.
    usize::from(unsafe {
        RtlCaptureStackBackTrace(
            0,
            frames.len() as u32,
            frames.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    })
}

#[cfg(target_os = "linux")]
fn executable_identity() -> (BuildIdentifier, u64, Option<std::ops::Range<u64>>) {
    use object::{Object as _, ObjectKind, ObjectSegment as _};

    let Ok(path) = std::env::current_exe() else {
        return (BuildIdentifier::None, 0, None);
    };
    let Ok(bytes) = std::fs::read(path) else {
        return (BuildIdentifier::None, 0, None);
    };
    let Ok(executable) = object::File::parse(bytes.as_slice()) else {
        return (BuildIdentifier::None, 0, None);
    };
    let identifier = executable
        .build_id()
        .ok()
        .flatten()
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(BuildIdentifier::None, BuildIdentifier::GnuBuildIdentifier);
    let mut information = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    let offset = if executable.kind() == ObjectKind::Dynamic {
        // SAFETY: dladdr writes the Dl_info for the address in this executable.
        let found = unsafe {
            libc::dladdr(
                executable_identity as *const libc::c_void,
                information.as_mut_ptr(),
            )
        };
        if found != 0 {
            // SAFETY: dladdr initialized the structure on success.
            unsafe { information.assume_init().dli_fbase as u64 }
        } else {
            0
        }
    } else {
        0
    };
    let range = executable
        .segments()
        .fold(None, |range: Option<std::ops::Range<u64>>, segment| {
            let start = segment.address().saturating_add(offset);
            let end = start.saturating_add(segment.size());
            Some(range.map_or(start..end, |range| {
                range.start.min(start)..range.end.max(end)
            }))
        });
    (identifier, offset, range)
}

#[cfg(not(target_os = "linux"))]
fn executable_identity() -> (BuildIdentifier, u64, Option<std::ops::Range<u64>>) {
    (BuildIdentifier::None, 0, None)
}

/// Import a file left by the previous process. Preserve it when storage fails
/// so the next start can retry. Return an error for an invalid file.
pub async fn import_pending(
    file_system: &virtual_file_system::VirtualFileSystem,
    pending: &Path,
    storage_capacity: u64,
) -> io::Result<()> {
    let file = match std::fs::File::open(pending) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(NATIVE_RECORD_CAPACITY as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() <= NATIVE_RECORD_CAPACITY
        && Record::read(&bytes).is_ok_and(|record| record.bytes().len() == bytes.len())
    {
        ::crash::import(file_system, &bytes, storage_capacity)
            .await
            .map_err(io::Error::other)?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid pending crash report",
        ));
    }
    std::fs::remove_file(pending)
}
