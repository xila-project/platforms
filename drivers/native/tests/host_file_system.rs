//! Host file system behaviour through the VFS (moved from Core's VFS tests,
//! which no longer depend on platform drivers).

extern crate abi_definitions;
extern crate alloc;

use drivers_native::host_file_system::{HostFileSystem, HostFileSystemOptions};
use file_system::{Context, FileSystemOperations, MemoryDevice, Path};
use task::{TaskIdentifier, test};
use virtual_file_system::VirtualFileSystem;

drivers_std::memory::instantiate_global_allocator!();

async fn initialize() -> (TaskIdentifier, &'static VirtualFileSystem) {
    let time_manager = Box::leak(Box::new(
        time::Manager::new(&drivers_std::devices::TimeDevice).unwrap(),
    ));
    initialize_with_time(time_manager).await
}

async fn initialize_with_time(
    time_manager: &'static time::Manager<'static>,
) -> (TaskIdentifier, &'static VirtualFileSystem) {
    let task_instance = Box::leak(Box::new(task::Manager::new()));
    let users_manager = Box::leak(Box::new(users::Manager::new()));

    if !log::is_initialized() {
        log::initialize(&drivers_std::log::Logger).unwrap();
    }

    let task = task_instance.get_current_task_identifier().await;

    let device = MemoryDevice::<512>::new_static(1024 * 512);

    let options = little_fs::Options::new(256);

    little_fs::FileSystem::format(device, options).unwrap();
    let file_system = little_fs::FileSystem::new(device, options).unwrap();

    let virtual_file_system = Box::leak(Box::new(
        virtual_file_system::initialize(task_instance, users_manager, time_manager, file_system)
            .unwrap(),
    ));

    (task, virtual_file_system)
}

#[cfg(target_os = "linux")]
#[test]
async fn hostfs_absolute_link_uses_vfs_root_without_escaping_host_capability() {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "xila-vfs-symlink-hostfs-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let host_root = root.join("host");
    fs::create_dir_all(&host_root).unwrap();
    fs::create_dir_all(root.join("target-directory")).unwrap();

    let direct_filesystem =
        HostFileSystem::open(&host_root, HostFileSystemOptions::default()).unwrap();
    let (task, vfs) = initialize().await;
    vfs.create_directory(task, &Path::from_str("/target-directory"))
        .await
        .unwrap();
    vfs.mount_file_system(
        HostFileSystem::open(&host_root, HostFileSystemOptions::default()).unwrap(),
        Path::from_str("/host"),
        task,
    )
    .await
    .unwrap();
    vfs.create_symlink(
        task,
        Path::from_str("/target-directory"),
        Path::from_str("/host/dirlink"),
    )
    .await
    .unwrap();

    let directory = vfs
        .open_directory(task, &Path::from_str("/host/dirlink"))
        .await
        .unwrap();
    directory.close(vfs).await.unwrap();

    let mut direct_context = Context::new_empty();
    assert!(
        direct_filesystem
            .lookup_directory(&mut direct_context, Path::from_str("/dirlink"))
            .is_err()
    );
    vfs.unmount(Path::from_str("/host"), true).await.unwrap();
    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "linux")]
#[test]
async fn hostfs_symlink_escaping_the_root_is_permission_denied() {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "xila-hostfs-escape-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let host_root = root.join("host");
    let outside = root.join("outside");
    fs::create_dir_all(&host_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, host_root.join("escape")).unwrap();

    let file_system = HostFileSystem::open(&host_root, HostFileSystemOptions::default()).unwrap();
    let mut context = Context::new_empty();
    assert_eq!(
        file_system
            .lookup_file(
                &mut context,
                Path::from_str("/escape/secret"),
                file_system::Flags::new(file_system::AccessFlags::Read, None, None),
            )
            .err(),
        Some(file_system::Error::PermissionDenied)
    );

    let _ = fs::remove_dir_all(root);
}
