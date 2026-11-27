use std::{ffi::CString, io, os::unix::ffi::OsStrExt as _, path::Path, ptr};

use ::crash::{Header, Limits, NATIVE_RECORD_CAPACITY, RecordKind, ResetReason, Writer};

#[cfg(target_os = "linux")]
mod stack;

pub(super) struct Paths {
    destination: CString,
    temporary: CString,
}

impl Paths {
    pub(super) fn new(destination: &Path, temporary: &Path) -> io::Result<Self> {
        Ok(Self {
            destination: CString::new(destination.as_os_str().as_bytes())?,
            temporary: CString::new(temporary.as_os_str().as_bytes())?,
        })
    }

    pub(super) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if self.write_from_handler(bytes) {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn write_from_handler(&self, mut bytes: &[u8]) -> bool {
        // SAFETY: Both path buffers are NUL-terminated and immutable. This
        // path uses only async-signal-safe system calls.
        unsafe {
            let descriptor = libc::open(
                self.temporary.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC,
                0o600 as libc::mode_t,
            );
            if descriptor < 0 {
                return false;
            }
            let mut success = true;
            while !bytes.is_empty() {
                let count = libc::write(descriptor, bytes.as_ptr().cast(), bytes.len());
                if count <= 0 {
                    success = false;
                    break;
                }
                bytes = &bytes[count as usize..];
            }
            success = success && libc::fsync(descriptor) == 0;
            success = (libc::close(descriptor) == 0) && success;
            success =
                success && libc::rename(self.temporary.as_ptr(), self.destination.as_ptr()) == 0;
            if !success {
                libc::unlink(self.temporary.as_ptr());
            }
            success
        }
    }
}

pub(super) struct AlternateStack {
    _storage: Box<[u8]>,
    previous: libc::stack_t,
}

impl AlternateStack {
    pub(super) fn install() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        stack::initialize_thread();
        let mut storage = vec![0; 64 * 1024].into_boxed_slice();
        // SAFETY: The requested stack has stable storage until the guard
        // restores the old one. The previous stack is written on success.
        unsafe {
            let mut stack: libc::stack_t = std::mem::zeroed();
            stack.ss_sp = storage.as_mut_ptr().cast();
            stack.ss_size = storage.len();
            let mut previous = std::mem::MaybeUninit::uninit();
            if libc::sigaltstack(&stack, previous.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                _storage: storage,
                previous: previous.assume_init(),
            })
        }
    }
}

impl Drop for AlternateStack {
    fn drop(&mut self) {
        // SAFETY: The previous descriptor belongs to this thread. The buffer
        // is freed only after it stops being the thread's alternate stack.
        unsafe {
            libc::sigaltstack(&self.previous, ptr::null_mut());
        }
    }
}

pub(super) fn install_handlers() -> io::Result<()> {
    // SAFETY: The handler has the sa_sigaction signature and uses the
    // alternate stack installed on this thread.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = capture_signal as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
        libc::sigfillset(&mut action.sa_mask);
        for signal in [
            libc::SIGSEGV,
            libc::SIGABRT,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGFPE,
        ] {
            if libc::sigaction(signal, &action, ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

pub(super) fn monotonic_microseconds() -> u64 {
    // SAFETY: The output points to a writable timespec and the clock is
    // async-signal-safe.
    unsafe {
        let mut time: libc::timespec = std::mem::zeroed();
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) != 0 {
            return 0;
        }
        time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1000
    }
}

extern "C" fn capture_signal(
    signal: libc::c_int,
    information: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    if let Some(state) = super::claim() {
        let mut slot = [0; NATIVE_RECORD_CAPACITY];
        let (program_counter, stack_pointer, link_register) = registers(context);
        // SAFETY: SA_SIGINFO supplies valid signal metadata for this call.
        let information = unsafe { &*information };
        let fault_address = if information.si_code > 0 && signal != libc::SIGABRT {
            // SAFETY: This is a kernel fault with an address field.
            unsafe { information.si_addr() as u64 }
        } else {
            0
        };
        #[cfg(target_os = "linux")]
        let captured_stack = stack::Window::capture(stack_pointer as usize);
        #[cfg(target_os = "linux")]
        let active = task::active_task::snapshot();
        let header = Header {
            #[cfg(target_os = "linux")]
            stack_base: if captured_stack.bytes().is_empty() {
                0
            } else {
                stack_pointer
            },
            #[cfg(target_os = "linux")]
            task_identifier: active.map_or(0, |task| task.identifier),
            #[cfg(target_os = "linux")]
            task_name: active
                .as_ref()
                .map_or("", task::active_task::ActiveTask::name),
            raw_reset_reason: signal as u32,
            exception_code: information.si_code as u32,
            uptime_microseconds: monotonic_microseconds().saturating_sub(state.monotonic_start),
            program_counter,
            stack_pointer,
            link_register,
            fault_address,
            ..state.header(RecordKind::KernelException, ResetReason::HostSignal)
        };
        let message = match signal {
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGILL => "SIGILL",
            _ => "SIGFPE",
        };
        if let Some(writer) = Writer::new(&mut slot, &header, Limits::NATIVE) {
            #[cfg(target_os = "linux")]
            let stack = captured_stack.bytes();
            #[cfg(not(target_os = "linux"))]
            let stack = &[];
            let bytes = writer.finish(format_args!("{message}"), stack, |_| Some(0));
            state.paths.write_from_handler(bytes);
        }
    }
    // SAFETY: SA_RESETHAND already restored the default action. Queue the
    // same signal for this thread so the parent observes its original cause.
    unsafe { libc::raise(signal) };
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn registers(context: *mut libc::c_void) -> (u64, u64, u64) {
    // SAFETY: The context is supplied by sigaction for the duration of the handler.
    let registers = unsafe { &(*context.cast::<libc::ucontext_t>()).uc_mcontext.gregs };
    (
        registers[libc::REG_RIP as usize] as u64,
        registers[libc::REG_RSP as usize] as u64,
        0,
    )
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn registers(context: *mut libc::c_void) -> (u64, u64, u64) {
    // SAFETY: The context is supplied by sigaction for the duration of the handler.
    let registers = unsafe { &(*context.cast::<libc::ucontext_t>()).uc_mcontext };
    (registers.pc, registers.sp, registers.regs[30])
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn registers(_context: *mut libc::c_void) -> (u64, u64, u64) {
    (0, 0, 0)
}
