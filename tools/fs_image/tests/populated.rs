#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "xila-seed-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("root/applications/executables")).unwrap();
        fs::write(
            path.join("root/applications/executables/example"),
            vec![42; 18000],
        )
        .unwrap();
        Self(path)
    }
    fn build(&self, name: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_fs_image"))
            .arg(self.0.join(name))
            .args(["4194304", "--root"])
            .arg(self.0.join("root"))
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn populated_images_are_verified_and_reproducible() {
    let f = Fixture::new();
    for name in ["one.img", "two.img"] {
        let output = f.build(name);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Verified"));
    }
    assert_eq!(
        fs::read(f.0.join("one.img")).unwrap(),
        fs::read(f.0.join("two.img")).unwrap()
    );
    let before = fs::read(f.0.join("one.img")).unwrap();
    assert!(!f.build("one.img").status.success());
    assert_eq!(before, fs::read(f.0.join("one.img")).unwrap());
}
#[cfg(unix)]
#[test]
fn rejects_symlinks_without_leaving_an_image() {
    let f = Fixture::new();
    std::os::unix::fs::symlink("/etc/passwd", f.0.join("root/link")).unwrap();
    assert!(!f.build("bad.img").status.success());
    assert!(!f.0.join("bad.img").exists());
}
#[test]
fn out_of_space_removes_incomplete_output() {
    let f = Fixture::new();
    fs::write(
        f.0.join("root/applications/executables/huge"),
        vec![1; 5 * 1024 * 1024],
    )
    .unwrap();
    assert!(!f.build("full.img").status.success());
    assert!(!f.0.join("full.img").exists());
}

fn write_bundle(root: &std::path::Path, app: &str) {
    let bundle = root.join(format!("xila.{}", app.replace('_', "-")));
    fs::create_dir_all(bundle.join(".xila")).unwrap();
    fs::create_dir_all(bundle.join("executables")).unwrap();
    fs::write(
        bundle.join(".xila/manifest.json"),
        format!("manifest for {app}"),
    )
    .unwrap();
    fs::write(
        bundle.join(format!("executables/{app}.wasm")),
        format!("guest fixture for {app}"),
    )
    .unwrap();
    fs::write(bundle.join("shortcut.json"), format!("shortcut for {app}")).unwrap();
}

#[test]
fn browser_and_native_bundles_contain_their_declared_apps() {
    let f = Fixture::new();
    let bundles = f.0.join("bundles");
    for app in [
        "calculator",
        "weather",
        "clock",
        "digits",
        "pixel_painter",
        "synth",
        "text_editor",
    ] {
        write_bundle(&bundles, app);
    }
    for profile in ["native", "wasm"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fs_image"))
            .arg(f.0.join(format!("{profile}.img")))
            .arg("4194304")
            .args(["--bundle", profile, "--bundles-dir"])
            .arg(&bundles)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("calculator,clock,digits,pixel_painter,synth,text_editor"));
        let image = fs::read(f.0.join(format!("{profile}.img"))).unwrap();
        let contains = |content: &[u8]| image.windows(content.len()).any(|bytes| bytes == content);
        for app in ["digits", "synth", "text_editor"] {
            for content in [
                format!("guest fixture for {app}"),
                format!("shortcut for {app}"),
                format!("manifest for {app}"),
            ] {
                assert!(contains(content.as_bytes()), "{profile}: {content}");
            }
        }
        assert_eq!(text.contains(",weather"), profile == "native");
        assert_eq!(contains(b"guest fixture for weather"), profile == "native");
        assert!(contains(b"/applications/executables/activity"));
        assert!(contains(b"/applications/executables/terminal"));
        assert!(contains(
            br#""name":"xila","url":"https://registry.xila.dev","root_key":"AJEDFQ2MPtUO0vu/BnC7xX5Ag8sBuq6DF5+ShKfCQgU=""#
        ));
    }
}

#[test]
fn missing_bundle_is_an_error_without_output() {
    let f = Fixture::new();
    let bundles = f.0.join("bundles");
    for app in ["calculator", "clock", "digits", "pixel_painter", "synth"] {
        write_bundle(&bundles, app);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_fs_image"))
        .arg(f.0.join("missing.img"))
        .arg("4194304")
        .args(["--bundle", "wasm", "--bundles-dir"])
        .arg(&bundles)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing bundle xila.text-editor"));
    assert!(!f.0.join("missing.img").exists());
}

#[test]
fn optional_capacity_defaults_when_population_flags_follow_output() {
    let f = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_fs_image"))
        .arg(f.0.join("default.img"))
        .arg("--root")
        .arg(f.0.join("root"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::metadata(f.0.join("default.img")).unwrap().len(),
        16 * 1024 * 1024
    );
}

#[cfg(unix)]
#[test]
fn unsupported_entries_and_non_utf8_names_are_rejected() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt, net::UnixListener};
    let f = Fixture::new();
    let socket = f.0.join("root/socket");
    let listener = UnixListener::bind(&socket).unwrap();
    assert!(!f.build("socket.img").status.success());
    assert!(!f.0.join("socket.img").exists());
    drop(listener);
    fs::remove_file(socket).unwrap();
    let invalid =
        f.0.join("root")
            .join(std::ffi::OsString::from_vec(vec![0xff]));
    fs::write(&invalid, b"invalid path").unwrap();
    assert!(!f.build("utf8.img").status.success());
    assert!(!f.0.join("utf8.img").exists());
    fs::remove_file(invalid).unwrap();
    assert!(f.build("before.img").status.success());
    fs::set_permissions(
        f.0.join("root/applications/executables/example"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    fs::File::open(f.0.join("root/applications/executables/example"))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    assert!(f.build("after.img").status.success());
    assert_eq!(
        fs::read(f.0.join("before.img")).unwrap(),
        fs::read(f.0.join("after.img")).unwrap()
    );
}

#[test]
fn both_profiles_require_the_digits_bundle() {
    let f = Fixture::new();
    let bundles = f.0.join("bundles");
    for app in [
        "calculator",
        "clock",
        "pixel_painter",
        "synth",
        "text_editor",
        "weather",
    ] {
        write_bundle(&bundles, app);
    }
    for profile in ["native", "wasm"] {
        let image = f.0.join(format!("missing-digits-{profile}.img"));
        let output = Command::new(env!("CARGO_BIN_EXE_fs_image"))
            .arg(&image)
            .arg("4194304")
            .args(["--bundle", profile, "--bundles-dir"])
            .arg(&bundles)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{profile} silently omitted Digits"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing bundle xila.digits"));
        assert!(!image.exists());
    }
}
