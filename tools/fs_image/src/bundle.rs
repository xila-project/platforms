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

pub fn prepare(profile: &str, applications: &Path) -> Result<Staging, Box<dyn Error>> {
    let manifest = match profile {
        "native" => include_str!("../profiles/native.txt"),
        "wasm" => include_str!("../profiles/browser.txt"),
        _ => return Err("bundle must be native or wasm".into()),
    };
    let guests: Vec<&str> = manifest.lines().filter(|line| !line.is_empty()).collect();
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
        "configuration/system",
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
        "data/system/logs",
        "data/applications",
        "data/shared",
        "temporary/cache",
        "temporary/sessions",
    ] {
        fs::create_dir_all(staging.0.join(directory))?;
    }
    fs::write(
        staging.0.join(&application_metadata::PENDING_PATH[1..]),
        application_metadata::PENDING_CONTENT,
    )?;
    for name in &guests {
        let source = applications.join(format!("{name}.wasm"));
        if !fs::symlink_metadata(&source)?.is_file() {
            return Err("guest binary must be a regular file".into());
        }
        fs::copy(
            source,
            staging.0.join(format!("applications/executables/{name}")),
        )?;
        fs::create_dir_all(staging.0.join(format!("data/applications/{name}")))?;
    }
    let native_shortcuts = match profile {
        "native" => include_str!("../profiles/native-shortcuts.txt"),
        "wasm" => include_str!("../profiles/browser-shortcuts.txt"),
        _ => unreachable!(),
    };
    for name in guests
        .iter()
        .copied()
        .chain(native_shortcuts.lines())
        .filter(|name| cfg!(unix) || *name != "debug")
    {
        fs::write(
            staging.0.join(format!(
                "configuration/shared/applications/shortcuts/{name}.json"
            )),
            application_shortcuts::shortcut(name).ok_or("unknown application")?,
        )?;
    }
    println!("Prepared {profile} bundle: {}", guests.join(","));
    Ok(staging)
}
