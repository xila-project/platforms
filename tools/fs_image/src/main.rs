//! Platforms' seed image builder: Platforms' profiles, shortcuts and
//! repositories over the image engine of `xila-project/tools`
//! (`xila-tools-image`). Cargo.lock pins that library with the firmware's Core,
//! so the images always match the Core revision that mounts them.
use std::{env, error::Error, path::PathBuf, process};
use tools_image::{Profile, ProfileFile};

tools_image::instantiate_global_allocator!();

fn main() {
    if let Err(error) = run() {
        eprintln!("fs_image: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1).peekable();
    let output = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: fs_image <output-image> [size-bytes] (size defaults to 16777216)")?;
    let size = if arguments
        .peek()
        .is_some_and(|value| !value.to_string_lossy().starts_with("--"))
    {
        arguments.next().unwrap().to_string_lossy().parse::<u64>()?
    } else {
        tools_image::DEFAULT_SIZE
    };
    let mut root = None;
    let mut bundle = None;
    let mut bundles = None;
    while let Some(flag) = arguments.next() {
        match flag.to_str() {
            Some("--root") => {
                root = Some(PathBuf::from(
                    arguments.next().ok_or("--root requires a directory")?,
                ))
            }
            Some("--bundle") => {
                bundle = Some(arguments.next().ok_or("--bundle requires native or wasm")?)
            }
            Some("--bundles-dir") => {
                bundles = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--bundles-dir requires a directory")?,
                ))
            }
            _ => return Err("unknown argument".into()),
        }
    }
    let output = match (bundle, bundles) {
        (Some(name), Some(directory)) if root.is_none() => {
            let name = name.to_str().ok_or("invalid bundle")?;
            let profile = profile(name)?;
            let staging = tools_image::stage(&profile, &directory)?;
            println!("Prepared {name} bundle: {}", profile.applications.join(","));
            tools_image::build(&output, size, Some(staging.path()))?
        }
        (None, None) => tools_image::build(&output, size, root.as_deref())?,
        _ => {
            return Err("use --root or --bundle native|wasm --bundles-dir <directory>".into());
        }
    };
    println!(
        "Verified Xila MBR, LittleFS contents and metadata: {} ({} bytes)",
        output.display(),
        size
    );
    Ok(())
}

/// Platforms' seed profile `native` or `wasm` (the browser profile).
fn profile(name: &str) -> Result<Profile, Box<dyn Error>> {
    let (applications, shortcuts) = match name {
        "native" => (
            include_str!("../profiles/native.txt"),
            include_str!("../profiles/native-shortcuts.txt"),
        ),
        "wasm" => (
            include_str!("../profiles/browser.txt"),
            include_str!("../profiles/browser-shortcuts.txt"),
        ),
        _ => return Err("bundle must be native or wasm".into()),
    };
    // Native applications are linked into the runtime; only their shortcuts
    // are seeded. The Debug application needs the Unix control transport.
    let shortcuts = Profile::parse_list(shortcuts)?
        .into_iter()
        .filter(|name| cfg!(unix) || name != "debug")
        .map(|name| {
            Ok(ProfileFile {
                content: native_shortcut(&name)
                    .ok_or("unknown native application")?
                    .as_bytes()
                    .to_vec(),
                name: format!("{name}.json"),
            })
        })
        .collect::<Result<_, Box<dyn Error>>>()?;
    Ok(Profile {
        name: name.into(),
        applications: Profile::parse_list(applications)?,
        shortcuts,
        // The official registry, trusted by its root key (fingerprint
        // f0df 8609 b382 1414 7e39 51c0 b40b c2ea), so `package install` works
        // without a trust prompt. Versions start at 0 and only increase.
        repositories: vec![ProfileFile {
            name: "xila.json".into(),
            content: include_bytes!("../repositories/xila.json").to_vec(),
        }],
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_seed_their_shortcuts_and_the_official_registry() {
        for name in ["native", "wasm"] {
            let profile = profile(name).unwrap();
            assert!(profile.applications.contains(&"digits".to_owned()));
            assert!(
                profile
                    .shortcuts
                    .iter()
                    .any(|shortcut| shortcut.name == "terminal.json")
            );
            assert_eq!(profile.repositories[0].name, "xila.json");
        }
        assert!(
            profile("native")
                .unwrap()
                .applications
                .contains(&"weather".to_owned())
        );
        assert!(profile("other").is_err());
    }
}
