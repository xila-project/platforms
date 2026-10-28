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

/// A first-party reference image seeds display metadata as DATA, not as an
/// application-name match table in Core or a guest __install side effect.
/// These labels and icons are selected by the seed profile only.
fn shortcut_json(name: &str, guest: bool) -> Result<String, Box<dyn Error>> {
    let row = include_str!("../profiles/shortcuts.tsv")
        .lines()
        .find(|line| line.split('|').next() == Some(name))
        .ok_or("missing shortcut descriptor")?;
    let fields: Vec<&str> = row.split('|').collect();
    if fields.len() != 6 {
        return Err("invalid seed shortcut row".into());
    }
    let [id, label, icon, red, green, blue]: [&str; 6] = fields
        .try_into()
        .map_err(|_| "invalid seed shortcut fields")?;
    // This table is compiled into the image builder. Reject unescaped text
    // before formatting JSON, avoiding a runtime JSON library dependency.
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
        || label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'-'))
        || !icon
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'>'))
    {
        return Err("seed shortcut contains invalid metadata".into());
    }
    let (red, green, blue) = (
        red.parse::<u8>()?,
        green.parse::<u8>()?,
        blue.parse::<u8>()?,
    );
    let command = if guest {
        "/applications/executables/wasm".to_string()
    } else {
        format!("/applications/executables/{id}")
    };
    let arguments = if guest {
        format!(r#"["@bundle/executables/{id}.wasm"]"#)
    } else {
        "[]".to_string()
    };
    Ok(format!(
        r#"{{"name":"{label}","command":"{command}","arguments":{arguments},"terminal":false,"icon_string":"{icon}","icon_color":[{red},{green},{blue}]}}"#
    ))
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
        // First-party applications are self-contained bundles with a
        // Shell-only shortcut.json, not install-created shortcut files.
        "applications/bundles",
        "applications/active",
        "data/system/logs",
        "data/applications",
        "data/shared",
        "temporary/cache",
        "temporary/sessions",
    ] {
        fs::create_dir_all(staging.0.join(directory))?;
    }
    // Authentication seeding is distinct from application metadata.
    fs::write(
        staging.0.join("system/accounts/.seed-pending"),
        b"xila-seed-v1\n",
    )?;
    for name in &guests {
        let source = applications.join(format!("{name}.wasm"));
        if !fs::symlink_metadata(&source)?.is_file() {
            return Err("guest binary must be a regular file".into());
        }
        let bundle = staging.0.join(format!("applications/bundles/xila.{name}"));
        fs::create_dir_all(bundle.join("executables"))?;
        fs::copy(source, bundle.join(format!("executables/{name}.wasm")))?;
        fs::write(bundle.join("shortcut.json"), shortcut_json(name, true)?)?;
    }
    let native_shortcuts = match profile {
        "native" => include_str!("../profiles/native-shortcuts.txt"),
        "wasm" => include_str!("../profiles/browser-shortcuts.txt"),
        _ => unreachable!(),
    };
    for name in native_shortcuts
        .lines()
        .filter(|name| cfg!(unix) || *name != "debug")
    {
        let bundle = staging.0.join(format!("applications/bundles/xila.{name}"));
        fs::create_dir_all(&bundle)?;
        fs::write(bundle.join("shortcut.json"), shortcut_json(name, false)?)?;
    }
    println!("Prepared {profile} bundle: {}", guests.join(","));
    Ok(staging)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_creates_shell_shortcuts_next_to_bundled_executables() {
        let root = std::env::temp_dir().join(format!(
            "xila-bundle-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let apps = [
            "calculator",
            "clock",
            "pixel_painter",
            "synth",
            "text_editor",
        ];
        for app in apps {
            fs::write(root.join(format!("{app}.wasm")), b"wasm fixture").unwrap();
        }
        {
            let bundle = prepare("wasm", &root).unwrap();
            for directory in [
                "applications/bundles",
                "applications/active",
                "applications/executables",
                "applications/libraries",
                "applications/bundles/xila.calculator/executables",
            ] {
                assert!(bundle.0.join(directory).is_dir(), "{directory}");
            }
            let shortcut = fs::read_to_string(
                bundle
                    .0
                    .join("applications/bundles/xila.calculator/shortcut.json"),
            )
            .unwrap();
            assert!(shortcut.contains("@bundle/executables/calculator.wasm"));
            assert!(
                bundle
                    .0
                    .join("applications/bundles/xila.calculator/executables/calculator.wasm")
                    .is_file()
            );
            assert!(
                !bundle
                    .0
                    .join("configuration/shared/applications/shortcuts/calculator.json")
                    .exists()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
