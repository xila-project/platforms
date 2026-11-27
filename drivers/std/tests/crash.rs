#![cfg(all(feature = "crash", target_os = "linux"))]

testing_support::instantiate_global_allocator!();

use std::{os::unix::process::ExitStatusExt as _, path::Path, process::Command};

use ::crash::{Record, ResetReason, list, read};
use drivers_std::crash::{import_pending, install};
use file_system::MemoryDevice;

const CHILD_MODE: &str = "XILA_CRASH_TEST_MODE";
const CHILD_PENDING_PATH: &str = "XILA_CRASH_TEST_PENDING_PATH";

#[test]
fn capture_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let pending = std::env::var_os(CHILD_PENDING_PATH).unwrap();
    let _registration = install(Path::new(&pending)).unwrap().unwrap();
    if mode == "signal" {
        // SAFETY: SIGFPE is handled by the installed crash handler.
        unsafe { libc::raise(libc::SIGFPE) };
    }
    panic!("Native crash reporting test");
}

#[task::test]
async fn next_process_imports_panic_and_signal() {
    let task_manager = task::test_manager();
    let root = task_manager.get_current_task_identifier().await;
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(
        time::Manager::new(&testing_support::TimeDevice).unwrap(),
    ));
    let disk = MemoryDevice::<512>::new_static(256 * 512);
    let filesystem =
        little_fs::FileSystem::get_or_format(disk, little_fs::Options::new(256)).unwrap();
    let filesystem =
        virtual_file_system::initialize(task_manager, users, time, filesystem).unwrap();
    virtual_file_system::create_default_hierarchy(&filesystem, root)
        .await
        .unwrap();

    let directory = std::env::temp_dir().join(format!("xila-crash-test-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let pending = directory.join("disk.crash.pending");
    for (mode, reason, message) in [
        (
            "panic",
            ResetReason::HostPanic,
            "Native crash reporting test",
        ),
        ("signal", ResetReason::HostSignal, "SIGFPE"),
    ] {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "capture_child"])
            .env(CHILD_MODE, mode)
            .env(CHILD_PENDING_PATH, &pending)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(!child.status.success());
        if mode == "signal" {
            assert_eq!(child.status.signal(), Some(libc::SIGFPE));
        }
        assert!(pending.is_file());
        import_pending(&filesystem, &pending, 128 * 1024)
            .await
            .unwrap();
        assert!(!pending.exists());
        let reports = list(&filesystem, root).await.unwrap();
        let bytes = read(&filesystem, root, *reports.last().unwrap())
            .await
            .unwrap();
        let report = Record::read(&bytes).unwrap();
        assert_eq!(report.header.reset_reason, reason);
        assert!(report.message.contains(message));
        if mode == "signal" {
            assert_ne!(report.header.program_counter, 0);
            assert!(!report.stack_window.is_empty());
        }
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[task::test]
async fn import_pending_removes_a_malformed_file() {
    let task_manager = task::test_manager();
    let root = task_manager.get_current_task_identifier().await;
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(
        time::Manager::new(&testing_support::TimeDevice).unwrap(),
    ));
    let disk = MemoryDevice::<512>::new_static(256 * 512);
    let filesystem =
        little_fs::FileSystem::get_or_format(disk, little_fs::Options::new(256)).unwrap();
    let filesystem =
        virtual_file_system::initialize(task_manager, users, time, filesystem).unwrap();
    virtual_file_system::create_default_hierarchy(&filesystem, root)
        .await
        .unwrap();

    let directory = std::env::temp_dir().join(format!("xila-crash-test-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let pending = directory.join("disk.crash.pending");
    std::fs::write(&pending, b"not a crash record").unwrap();
    let error = import_pending(&filesystem, &pending, 128 * 1024)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!pending.exists());
    std::fs::remove_dir_all(&directory).unwrap();
}
