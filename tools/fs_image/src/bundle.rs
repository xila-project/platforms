use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Staging(pub PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Package identifier of an official guest application, e.g. `xila.pixel-painter`.
pub fn package_identifier(name: &str) -> String {
    format!("xila.{}", name.replace('_', "-"))
}

fn native_shortcut(name: &str) -> Option<&'static str> {
    Some(match name {
        "activity" => include_str!("../shortcuts/activity.json"),
        "debug" => include_str!("../shortcuts/debug.json"),
        "file_manager" => include_str!("../shortcuts/file_manager.json"),
        "settings" => include_str!("../shortcuts/settings.json"),
        "terminal" => include_str!("../shortcuts/terminal.json"),
        _ => return None,
    })
}

/// Copy a verified bundle directory, rejecting anything but regular files and
/// directories. Entries are visited in sorted order for reproducible images.
fn copy_bundle(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir(destination)?;
    let mut entries = fs::read_dir(source)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    for path in entries {
        let metadata = fs::symlink_metadata(&path)?;
        let target = destination.join(path.file_name().ok_or("invalid bundle entry")?);
        if metadata.is_dir() {
            copy_bundle(&path, &target)?;
        } else if metadata.is_file() {
            fs::copy(&path, &target)?;
        } else {
            return Err(format!("bundle entry is not a regular file: {}", path.display()).into());
        }
    }
    Ok(())
}

/// Stage a seed profile from verified bundle directories named by package
/// identifier (`<bundles>/xila.clock/.xila/manifest.json`, ...), as produced by
/// `xila-prepare-applications` or `xila-package bundle build`.
pub fn prepare(profile: &str, bundles: &Path) -> Result<Staging, Box<dyn Error>> {
    let manifest = match profile {
        "native" => include_str!("../profiles/native.txt"),
        "wasm" => include_str!("../profiles/browser.txt"),
        _ => return Err("bundle must be native or wasm".into()),
    };
    let mut guests: Vec<&str> = manifest.lines().filter(|line| !line.is_empty()).collect();
    // Locally built demo bundles can be staged before their first release.
    if bundles.join("xila.digits").exists() {
        guests.push("digits");
    }
    let staging = loop {
        let path = std::env::temp_dir().join(format!(
            "xila-bundle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => break Staging(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    for directory in [
        "system/accounts/users",
        "system/accounts/groups",
        "system/executables",
        "system/libraries",
        "system/services",
        "configuration/system/packages",
        "configuration/system/packages/repositories",
        "configuration/system/packages/installed",
        "configuration/applications",
        "configuration/shared/applications/shortcuts",
        "devices/audio",
        "devices/compute",
        "devices/graphics",
        "devices/input",
        "devices/network",
        "devices/random",
        "devices/standard_io",
        "devices/storage",
        "devices/time",
        "applications/executables",
        "applications/libraries",
        "applications/bundles",
        "data/system/logs",
        "data/applications",
        "data/shared",
        "temporary/cache",
        "temporary/sessions",
    ] {
        fs::create_dir_all(staging.0.join(directory))?;
    }
    fs::write(
        staging.0.join(&authentication::SEED_PENDING_PATH[1..]),
        authentication::SEED_PENDING_CONTENT,
    )?;
    for name in &guests {
        let identifier = package_identifier(name);
        let source = bundles.join(&identifier);
        if !fs::symlink_metadata(source.join(".xila/manifest.json"))
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Err(format!("missing bundle {identifier} in {}", bundles.display()).into());
        }
        copy_bundle(
            &source,
            &staging.0.join("applications/bundles").join(&identifier),
        )?;
    }
    // Native applications are linked into the runtime; only their shortcuts are seeded.
    let native_shortcuts = match profile {
        "native" => include_str!("../profiles/native-shortcuts.txt"),
        "wasm" => include_str!("../profiles/browser-shortcuts.txt"),
        _ => unreachable!(),
    };
    for name in native_shortcuts
        .lines()
        .filter(|name| !name.is_empty() && (cfg!(unix) || *name != "debug"))
    {
        fs::write(
            staging.0.join(format!(
                "configuration/shared/applications/shortcuts/{name}.json"
            )),
            native_shortcut(name).ok_or("unknown native application")?,
        )?;
    }
    // The official registry, trusted by its root key (fingerprint
    // f0df 8609 b382 1414 7e39 51c0 b40b c2ea), so `package install` works
    // without a trust prompt. Versions start at 0 and only increase.
    fs::write(
        staging
            .0
            .join("configuration/system/packages/repositories/xila.json"),
        include_str!("../repositories/xila.json"),
    )?;
    println!("Prepared {profile} bundle: {}", guests.join(","));
    Ok(staging)
}
