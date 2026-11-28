//! Host file system behaviour through the VFS (moved from Core's VFS tests,
//! which no longer depend on platform drivers).

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
async fn hostfs_symlink_escaping_the_root_is_path_escapes_root() {
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
        Some(io_error::Kind::PathEscapesRoot)
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "linux")]
fn host_test_root(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "xila-{prefix}-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("host")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    root
}

#[cfg(target_os = "linux")]
async fn mount_host(
    vfs: &'static VirtualFileSystem,
    task: TaskIdentifier,
    host_root: &std::path::Path,
) {
    vfs.create_directory(task, &Path::from_str("/host"))
        .await
        .unwrap();
    vfs.mount_file_system(
        HostFileSystem::open(host_root, HostFileSystemOptions::default()).unwrap(),
        Path::from_str("/host"),
        task,
    )
    .await
    .unwrap();
}

/// A WASI preopen on a host mount (`xila-runtime --mount host=...`) reads its
/// input and writes its output through confined lookups.
#[cfg(target_os = "linux")]
#[test]
async fn confined_lookups_enter_a_host_mount() {
    use file_system::{AccessFlags, CreateFlags, Flags};
    use std::fs;

    let root = host_test_root("hostfs-confined");
    let host_root = root.join("host");
    fs::create_dir(host_root.join("data")).unwrap();
    fs::write(host_root.join("data/input"), b"input").unwrap();
    let (task, vfs) = initialize().await;
    mount_host(vfs, task, &host_root).await;

    for granted in ["/host", "/host/data"] {
        let directory_root = vfs.resolve_directory_root(task, granted).await.unwrap();
        let input = if granted == "/host" {
            "data/input"
        } else {
            "input"
        };
        let mut file = vfs
            .open_beneath(
                &Path::from_str(input),
                Flags::from(AccessFlags::Read),
                task,
                &directory_root,
            )
            .await
            .unwrap();
        let mut buffer = [0; 16];
        let read = file.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"input");
        file.close(vfs).await.unwrap();

        let directory = vfs
            .open_directory_beneath(task, &Path::from_str("."), &directory_root)
            .await
            .unwrap();
        directory.close(vfs).await.unwrap();
    }

    let directory_root = vfs
        .resolve_directory_root(task, "/host/data")
        .await
        .unwrap();
    let mut output = vfs
        .open_beneath(
            &Path::from_str("output"),
            Flags::new(AccessFlags::Write, Some(CreateFlags::Create), None),
            task,
            &directory_root,
        )
        .await
        .unwrap();
    output.write(b"output").await.unwrap();
    output.close(vfs).await.unwrap();
    assert_eq!(fs::read(host_root.join("data/output")).unwrap(), b"output");

    vfs.create_directory_beneath(task, &Path::from_str("directory"), &directory_root)
        .await
        .unwrap();
    vfs.create_symlink_beneath(
        task,
        Path::from_str("../input"),
        Path::from_str("directory/link"),
        &directory_root,
    )
    .await
    .unwrap();
    let mut file = vfs
        .open_beneath(
            &Path::from_str("directory/link"),
            Flags::from(AccessFlags::Read),
            task,
            &directory_root,
        )
        .await
        .unwrap();
    let mut buffer = [0; 16];
    let read = file.read(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..read], b"input");
    file.close(vfs).await.unwrap();
    vfs.remove_beneath(task, Path::from_str("directory/link"), &directory_root)
        .await
        .unwrap();
    vfs.remove_beneath(task, Path::from_str("directory"), &directory_root)
        .await
        .unwrap();
    assert!(!host_root.join("data/directory").exists());

    vfs.unmount(Path::from_str("/host"), true).await.unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Host links pointing outside the host directory, or outside the confined
/// root, and `..` chains cannot leave a preopen on a host mount.
#[cfg(target_os = "linux")]
#[test]
async fn confined_lookups_cannot_escape_through_host_links() {
    use file_system::{AccessFlags, CreateFlags, Flags};
    use io_error::Kind;
    use std::fs;

    let root = host_test_root("hostfs-escape-confined");
    let host_root = root.join("host");
    let outside = root.join("outside");
    fs::write(outside.join("secret"), b"secret").unwrap();
    fs::create_dir_all(host_root.join("sandbox/sub")).unwrap();
    fs::create_dir(host_root.join("other")).unwrap();
    fs::write(host_root.join("other/file"), b"other").unwrap();
    fs::write(host_root.join("sandbox/file"), b"file").unwrap();
    // Relative link out of the host directory.
    std::os::unix::fs::symlink("../../outside", host_root.join("sandbox/up")).unwrap();
    // Host-absolute link out of the host directory.
    std::os::unix::fs::symlink(&outside, host_root.join("sandbox/absolute")).unwrap();
    // Link inside the host directory but outside the confined root.
    std::os::unix::fs::symlink("../other", host_root.join("sandbox/sibling")).unwrap();
    // Link chain ending outside the confined root.
    std::os::unix::fs::symlink("sibling", host_root.join("sandbox/chain")).unwrap();
    // Link that stays inside the confined root.
    std::os::unix::fs::symlink("sub/../file", host_root.join("sandbox/inside")).unwrap();

    let (task, vfs) = initialize().await;
    mount_host(vfs, task, &host_root).await;
    let directory_root = vfs
        .resolve_directory_root(task, "/host/sandbox")
        .await
        .unwrap();
    let read = Flags::from(AccessFlags::Read);

    let mut file = vfs
        .open_beneath(&Path::from_str("inside"), read, task, &directory_root)
        .await
        .unwrap();
    let mut buffer = [0; 16];
    let length = file.read(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..length], b"file");
    file.close(vfs).await.unwrap();

    for path in [
        "up/secret",
        "sibling/file",
        "chain/file",
        "../other/file",
        "sub/../../other/file",
        "sub/../../../outside/secret",
        "../../../../../../outside/secret",
    ] {
        assert_eq!(
            vfs.open_beneath(&Path::from_str(path), read, task, &directory_root)
                .await
                .err(),
            Some(Kind::PathEscapesRoot),
            "{path}"
        );
    }
    // A host-absolute target is not a Xila path at all.
    assert!(
        vfs.open_beneath(
            &Path::from_str("absolute/secret"),
            read,
            task,
            &directory_root
        )
        .await
        .is_err()
    );
    for path in ["up/created", "absolute/created", "sibling/created"] {
        assert!(
            vfs.open_beneath(
                &Path::from_str(path),
                Flags::new(AccessFlags::Write, Some(CreateFlags::Create), None),
                task,
                &directory_root,
            )
            .await
            .is_err(),
            "{path}"
        );
        assert!(
            vfs.create_directory_beneath(task, &Path::from_str(path), &directory_root)
                .await
                .is_err(),
            "{path}"
        );
    }
    assert!(
        vfs.remove_beneath(task, Path::from_str("sibling/file"), &directory_root)
            .await
            .is_err()
    );

    // Outside a preopen, the relative host link resolves in the Xila namespace
    // (`/outside`, which does not exist there), never in the host's.
    assert_eq!(
        vfs.open(&Path::from_str("/host/sandbox/up/secret"), read, task)
            .await
            .err(),
        Some(Kind::NotFound)
    );

    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"secret");
    assert_eq!(fs::read(host_root.join("other/file")).unwrap(), b"other");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert!(!host_root.join("other/created").exists());
    vfs.unmount(Path::from_str("/host"), true).await.unwrap();
    let _ = fs::remove_dir_all(root);
}

/// A host process keeps replacing a directory under a preopen with a link to
/// an outside directory while the virtual file system creates entries in it.
#[cfg(target_os = "linux")]
#[test]
async fn confined_creates_stay_inside_while_a_host_parent_is_replaced() {
    use file_system::{AccessFlags, CreateFlags, Flags};
    use std::{fs, thread};

    let root = host_test_root("hostfs-rename-race");
    let host_root = root.join("host");
    let outside = root.join("outside");
    fs::create_dir_all(host_root.join("sandbox/parent")).unwrap();
    let (task, vfs) = initialize().await;
    mount_host(vfs, task, &host_root).await;
    let directory_root = vfs
        .resolve_directory_root(task, "/host/sandbox")
        .await
        .unwrap();

    let replacing_sandbox = host_root.join("sandbox");
    let replacing_outside = outside.clone();
    let replacer = thread::spawn(move || {
        for _ in 0..300 {
            let parent = replacing_sandbox.join("parent");
            let moved = replacing_sandbox.join("parent-moved");
            let _ = fs::rename(&parent, &moved);
            let _ = std::os::unix::fs::symlink(&replacing_outside, &parent);
            let _ = fs::remove_file(&parent);
            let _ = fs::rename(&moved, &parent);
        }
    });
    let create = Flags::new(AccessFlags::Write, Some(CreateFlags::Create), None);
    for _ in 0..300 {
        if let Ok(file) = vfs
            .open_beneath(
                &Path::from_str("parent/pwned"),
                create,
                task,
                &directory_root,
            )
            .await
        {
            file.close(vfs).await.unwrap();
        }
        let _ = vfs
            .create_directory_beneath(
                task,
                &Path::from_str("parent/pwned-directory"),
                &directory_root,
            )
            .await;
        let _ = vfs
            .remove_beneath(task, Path::from_str("parent/pwned"), &directory_root)
            .await;
        let _ = vfs
            .remove_beneath(
                task,
                Path::from_str("parent/pwned-directory"),
                &directory_root,
            )
            .await;
    }
    replacer.join().unwrap();
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    vfs.unmount(Path::from_str("/host"), true).await.unwrap();
    let _ = fs::remove_dir_all(root);
}
