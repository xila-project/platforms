//! Native shutdown: host signals, the lifecycle supervisor and the process
//! power device. Halting ends only this Xila process; the host is untouched.
use core::time::Duration;
use xila::lifecycle::{Action, SYSTEM, Supervisor};

/// Quiescence budget for tasks and guests after a request.
const QUIESCE_BUDGET: Duration = Duration::from_secs(5);

#[cfg(unix)]
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
    fn _exit(status: i32) -> !;
}

/// Async-signal-safe: one atomic request, or `_exit` when a shutdown is
/// already in progress (forced exit: data LittleFS has not committed is lost,
/// the last committed state stays on disk).
#[cfg(unix)]
extern "C" fn on_signal(number: i32) {
    let status = 128 + number;
    if SYSTEM.request(Action::PowerOff, status as u8).is_err() {
        unsafe { _exit(status) };
    }
    // Legacy broadcast for native guest runners; the supervisor also signals
    // each guest's task.
    wasm::request_shutdown();
}

/// Route SIGINT and SIGTERM to a lifecycle request (exit status 128 + signal).
pub fn install_signal_handlers() {
    #[cfg(unix)]
    for number in [2, 15] {
        // SIG_ERR is (sighandler_t)-1.
        let previous = unsafe { signal(number, on_signal as *const () as usize) };
        assert_ne!(previous, usize::MAX, "failed to install signal {number}");
    }
}

/// Run the shutdown pipeline, requesting power-off with `status` unless a
/// request (command or signal) already won. On success the power device
/// exits the process. Returns only if shutdown failed: the process then
/// stays alive with every resource retained, and SIGINT/SIGTERM forces exit.
pub async fn finish(
    virtual_file_system: &'static xila::virtual_file_system::VirtualFileSystem,
    time_manager: &'static xila::time::Manager<'static>,
    task: xila::task::TaskIdentifier,
    services: &[xila::task::TaskIdentifier],
    status: u8,
) {
    let _ = SYSTEM.request(Action::PowerOff, status);
    let supervisor = Supervisor {
        virtual_file_system,
        time_manager,
        task,
        services,
        budget: QUIESCE_BUDGET,
    };
    if let Err(error) = supervisor.run(&SYSTEM, &mut ()).await {
        eprintln!(
            "Shutdown failed: {error}. Resources are retained; send SIGINT or SIGTERM to force exit (uncommitted data is lost)."
        );
    }
}

/// Exit status for a shell that returned `status`.
pub fn shell_status(status: isize) -> u8 {
    status.unsigned_abs().min(255) as u8
}
