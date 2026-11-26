//! A native runtime whose disk names a startup application boots into it,
//! without the graphical shell or its login.
#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use xila::file_system::{
    AccessFlags, AttributeFlags, Attributes, BaseOperations, Context, DirectBaseOperations,
    FileSystemOperations, Kind, Permissions, XILA_DISK_SIGNATURE,
    mbr::{Mbr, PartitionKind},
};

drivers_std::instantiate_global_allocator!();

const SIZE: u64 = 4 * 1024 * 1024;

struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Runtime(Child);

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A seed disk holding only `/configuration/system/startup.json`, as root's,
/// mode 0644, as the image builder writes it.
fn seed(path: &Path, configuration: &str) {
    let name = path.to_str().unwrap();
    let drive = drivers_std::drive_file::FileDriveDevice::new_static(&name, SIZE);
    let partition = Mbr::format_disk_with_signature_and_partition(
        drive,
        XILA_DISK_SIGNATURE,
        PartitionKind::Xila,
    )
    .unwrap();
    let file_system = xila::little_fs::FileSystem::new_format(
        Box::leak(Box::new(partition)),
        xila::little_fs::Options::new(256),
    )
    .unwrap();
    let attributes = |kind, permissions| {
        Attributes::new()
            .set_mask(
                AttributeFlags::all().difference(AttributeFlags::Size | AttributeFlags::Links),
            )
            .set_kind(kind)
            .set_permissions(Permissions::from_octal(permissions).unwrap())
    };
    for directory in ["/configuration", "/configuration/system"] {
        file_system
            .create_directory(directory.as_ref(), &Attributes::new())
            .unwrap();
        file_system
            .set_attributes(directory.as_ref(), &attributes(Kind::Directory, 0o755))
            .unwrap();
    }
    let path = startup_application::CONFIGURATION_PATH;
    file_system
        .create_file(path.as_ref(), &Attributes::new())
        .unwrap();
    let mut context = Context::new_empty();
    file_system
        .lookup_file(&mut context, path.as_ref(), AccessFlags::Write.into())
        .unwrap();
    file_system
        .write_sync(&mut context, configuration.as_bytes(), 0)
        .unwrap();
    file_system.close(&mut context).unwrap();
    file_system
        .set_attributes(path.as_ref(), &attributes(Kind::File, 0o644))
        .unwrap();
    drop(file_system);
    DirectBaseOperations::flush_sync(drive).unwrap();
}

/// One Debug request; `None` while the transport is not answering yet.
fn request(endpoint: &Path, request: &str) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(endpoint).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(stream, "{request}").ok()?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).ok()?;
    serde_json::from_str(&response).ok()
}

#[test]
fn the_startup_application_replaces_the_graphical_shell() {
    let directory =
        Directory(std::env::temp_dir().join(format!("xila-native-startup-{}", std::process::id())));
    fs::create_dir(&directory.0).unwrap();
    seed(
        &directory.0.join("seed.img"),
        r#"{"command": "/applications/executables/settings", "user": "root"}"#,
    );
    let log_path = directory.0.join("runtime.log");
    let log = fs::File::create(&log_path).unwrap();
    let mut runtime = Runtime(
        Command::new(env!("CARGO_BIN_EXE_native_example"))
            .args([
                "--display-driver",
                "headless",
                "--resolution",
                "480x320",
                "--no-network",
            ])
            .arg("--disk-size")
            .arg(SIZE.to_string())
            .arg("--disk-path")
            .arg(directory.0.join("drive.img"))
            .arg("--seed-image")
            .arg(directory.0.join("seed.img"))
            .env("TMPDIR", &directory.0)
            .env_remove("XILA_UI_CONTROL_ENDPOINT")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let uid = fs::metadata(&directory.0).unwrap().uid();
    let endpoint = directory.0.join(format!("xila-debug-{uid}/control.sock"));

    // Wait until the Settings window is on the screen.
    let deadline = Instant::now() + Duration::from_secs(30);
    let tasks = loop {
        assert!(
            runtime.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "The startup application did not start: {}",
            fs::read_to_string(&log_path).unwrap(),
        );
        if let Some(response) = request(&endpoint, r#"{"op":"diagnostics"}"#) {
            let tasks: Vec<String> = response["result"]["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|task| task["name"].as_str().map(String::from))
                .collect();
            if tasks.iter().any(|name| name == "settings") {
                break tasks;
            }
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(
        tasks.iter().any(|name| name == "Application session"),
        "{tasks:?}"
    );
    assert!(
        !tasks.iter().any(|name| name == "graphical_shell"),
        "{tasks:?}"
    );

    // No login form: the window on the screen is the application's.
    let tree = request(
        &endpoint,
        r#"{"op":"ui_tree","max_elements":64,"max_depth":16}"#,
    )
    .unwrap();
    let names: Vec<&str> = tree["result"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|element| element["name"].as_str())
        .collect();
    assert!(!names.contains(&"Login"), "{names:?}");
    assert!(
        fs::read_to_string(&log_path)
            .unwrap()
            .contains("Startup application /applications/executables/settings"),
    );
}
