use std::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

const CAPACITY: usize = 2048;

std::thread_local! {
    static STACK_BOUNDS: (AtomicUsize, AtomicUsize) =
        const { (AtomicUsize::new(0), AtomicUsize::new(0)) };
}

pub(super) fn initialize_thread() {
    // SAFETY: pthread_getattr_np writes an initialized attribute object on
    // success. pthread_attr_getstack writes two valid scalar fields.
    unsafe {
        let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        if libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) != 0 {
            return;
        }
        let mut attributes = attributes.assume_init();
        let mut address = ptr::null_mut();
        let mut size = 0;
        let result = libc::pthread_attr_getstack(&attributes, &mut address, &mut size);
        libc::pthread_attr_destroy(&mut attributes);
        if result == 0 {
            let start = address as usize;
            let end = start.saturating_add(size);
            STACK_BOUNDS.with(|bounds| {
                bounds.0.store(start, Ordering::Relaxed);
                bounds.1.store(end, Ordering::Release);
            });
        }
    }
}

pub(super) struct Window {
    storage: [u8; CAPACITY],
    length: usize,
}

impl Window {
    pub(super) fn capture(pointer: usize) -> Self {
        let mut window = Self {
            storage: [0; CAPACITY],
            length: 0,
        };
        let bounds = STACK_BOUNDS.try_with(|bounds| {
            let end = bounds.1.load(Ordering::Acquire);
            bounds.0.load(Ordering::Relaxed)..end
        });
        let Ok(bounds) = bounds else {
            return window;
        };
        if !bounds.contains(&pointer) {
            return window;
        }
        let length = (bounds.end - pointer).min(CAPACITY);
        let local = libc::iovec {
            iov_base: window.storage.as_mut_ptr().cast(),
            iov_len: length,
        };
        let remote = libc::iovec {
            iov_base: pointer as *mut _,
            iov_len: length,
        };
        // SAFETY: This syscall copies into the local buffer. An unreadable
        // page returns an error or a short count without faulting the handler.
        let copied = unsafe {
            libc::syscall(
                libc::SYS_process_vm_readv,
                libc::getpid(),
                &local,
                1,
                &remote,
                1,
                0,
            )
        };
        window.length = (copied.max(0) as usize).min(length);
        window
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.storage[..self.length]
    }
}
