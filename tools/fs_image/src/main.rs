//! Platforms' seed image builder: Platforms' profiles, shortcuts and
//! repositories over the image engine of `xila-project/tools`
//! (`xila-tools-image`). Cargo.lock pins that library with the firmware's Core,
//! so the images always match the Core revision that mounts them.
use std::{env, error::Error, path::PathBuf, process};
use tools_image::{Geometry, Layout, Profile, ProfileFile};

tools_image::instantiate_global_allocator!();

/// Default block size of raw flash partition images (Espressif flash erase size).
const DEFAULT_RAW_BLOCK_SIZE: u32 = 4096;

fn main() {
    if let Err(error) = run() {
        eprintln!("fs_image: {error}");
        process::exit(1);
    }
}

fn parse_layout(value: &str) -> Result<Layout, Box<dyn Error>> {
    match value {
        "mbr" => Ok(Layout::Mbr),
        "raw" => Ok(Layout::Raw),
        _ => Err("--layout must be mbr or raw".into()),
    }
}

fn resolve_geometry(
    layout: Option<Layout>,
    block_size: Option<u32>,
) -> Result<Geometry, Box<dyn Error>> {
    let layout = layout.unwrap_or(Layout::Mbr);
    let geometry = match (layout, block_size) {
        (Layout::Mbr, None) => Geometry::MBR,
        (Layout::Raw, None) => Geometry::raw(DEFAULT_RAW_BLOCK_SIZE),
        (Layout::Mbr, Some(block_size)) => Geometry { layout, block_size },
        (Layout::Raw, Some(block_size)) => Geometry::raw(block_size),
    };
    geometry.validate()?;
    Ok(geometry)
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
    let mut native_runtime = None;
    let mut layout = None;
    let mut block_size = None;
    while let Some(flag) = arguments.next() {
        match flag.to_str() {
            Some("--root") => {
                root = Some(PathBuf::from(
                    arguments.next().ok_or("--root requires a directory")?,
                ))
            }
            Some("--bundle") => {
                bundle = Some(
                    arguments
                        .next()
                        .ok_or("--bundle requires native, wasm, wt32-sc01 or wt32-sc01-plus")?,
                )
            }
            Some("--bundles-dir") => {
                bundles = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--bundles-dir requires a directory")?,
                ))
            }
            Some("--native-runtime") => {
                let value = arguments
                    .next()
                    .ok_or("--native-runtime requires unix or windows")?;
                native_runtime = Some(NativeRuntime::parse(
                    value.to_str().ok_or("invalid native runtime")?,
                )?)
            }
            Some("--layout") => {
                let value = arguments.next().ok_or("--layout requires mbr or raw")?;
                layout = Some(parse_layout(value.to_str().ok_or("invalid layout")?)?)
            }
            Some("--block-size") => {
                let value = arguments
                    .next()
                    .ok_or("--block-size requires a block size in bytes")?;
                block_size = Some(
                    value
                        .to_str()
                        .ok_or("invalid block size")?
                        .parse::<u32>()
                        .map_err(|_| "--block-size must be a positive integer")?,
                )
            }
            _ => return Err("unknown argument".into()),
        }
    }
    let geometry = resolve_geometry(layout, block_size)?;
    let output = match (bundle, bundles) {
        (Some(name), Some(directory)) if root.is_none() => {
            let name = name.to_str().ok_or("invalid bundle")?;
            if native_runtime.is_some() && name != "native" {
                return Err("--native-runtime only applies to --bundle native".into());
            }
            let profile = profile(name, native_runtime.unwrap_or_else(NativeRuntime::host))?;
            let staging = tools_image::stage(&profile, &directory)?;
            println!("Prepared {name} bundle: {}", profile.applications.join(","));
            tools_image::build_with(&output, size, Some(staging.path()), geometry)?
        }
        (None, None) if native_runtime.is_none() => {
            tools_image::build_with(&output, size, root.as_deref(), geometry)?
        }
        _ => {
            return Err(
                "use --root or --bundle native|wasm|wt32-sc01|wt32-sc01-plus --bundles-dir <directory> [--native-runtime unix|windows] [--layout mbr|raw] [--block-size <bytes>]"
                    .into(),
            );
        }
    };
    match geometry.layout {
        Layout::Mbr => println!(
            "Verified Xila MBR, LittleFS contents and metadata: {} ({} bytes)",
            output.display(),
            size
        ),
        Layout::Raw => println!(
            "Verified raw LittleFS ({}-byte blocks), contents and metadata: {} ({} bytes)",
            geometry.block_size,
            output.display(),
            size
        ),
    }
    Ok(())
}

/// The host family of the native runtime that will mount a `native` image.
///
/// It is chosen by the caller, not by the machine building the image: an
/// image built on one host can be run by a native runtime on another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeRuntime {
    /// Linux or macOS: the native example mounts the Debug control transport.
    Unix,
    /// Windows: no Debug control transport.
    Windows,
}

impl NativeRuntime {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "unix" => Ok(Self::Unix),
            "windows" => Ok(Self::Windows),
            _ => Err("--native-runtime must be unix or windows".into()),
        }
    }

    /// The default when the caller does not say: the build host's family.
    fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }

    /// Whether the runtime provides `/devices/graphics/debug/transport`, which
    /// the Debug application needs (`examples/native/src/main.rs`).
    fn provides_debug_transport(self) -> bool {
        self == Self::Unix
    }
}

/// Platforms' native, browser or WT32 board seed profile.
/// `native_runtime` only affects the `native` profile.
fn profile(name: &str, native_runtime: NativeRuntime) -> Result<Profile, Box<dyn Error>> {
    let wt32_shortcuts = include_str!("../profiles/wt32-native-shortcuts.txt");
    let (applications, shortcuts) = match name {
        "native" => (
            include_str!("../profiles/native.txt"),
            include_str!("../profiles/native-shortcuts.txt"),
        ),
        "wasm" => (
            include_str!("../profiles/browser.txt"),
            include_str!("../profiles/browser-shortcuts.txt"),
        ),
        "wt32-sc01" => ("", wt32_shortcuts),
        "wt32-sc01-plus" => (
            include_str!("../profiles/wt32-sc01-plus.txt"),
            wt32_shortcuts,
        ),
        _ => return Err("bundle must be native, wasm, wt32-sc01 or wt32-sc01-plus".into()),
    };
    // Native applications are linked into the runtime; only their shortcuts
    // are seeded. The Debug application needs the control transport of the
    // runtime that will mount the image, not of the host building it.
    let shortcuts = Profile::parse_list(shortcuts)?
        .into_iter()
        .filter(|name| native_runtime.provides_debug_transport() || name != "debug")
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
            let profile = profile(name, NativeRuntime::host()).unwrap();
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
            profile("native", NativeRuntime::host())
                .unwrap()
                .applications
                .contains(&"weather".to_owned())
        );
        assert!(profile("other", NativeRuntime::host()).is_err());
    }

    #[test]
    fn wt32_sc01_profile_seeds_the_native_application_shortcuts() {
        let profile = profile("wt32-sc01", NativeRuntime::host()).unwrap();
        assert!(profile.applications.is_empty());
        let shortcut_names: Vec<&str> = profile
            .shortcuts
            .iter()
            .map(|shortcut| shortcut.name.as_str())
            .collect();
        assert_eq!(
            shortcut_names,
            [
                "activity.json",
                "file_manager.json",
                "settings.json",
                "terminal.json",
            ]
        );
        assert_eq!(profile.repositories[0].name, "xila.json");
    }

    fn has_debug_shortcut(name: &str, runtime: NativeRuntime) -> bool {
        profile(name, runtime)
            .unwrap()
            .shortcuts
            .iter()
            .any(|shortcut| shortcut.name == "debug.json")
    }

    #[test]
    fn debug_shortcut_follows_the_native_runtime_not_the_build_host() {
        assert!(has_debug_shortcut("native", NativeRuntime::Unix));
        assert!(!has_debug_shortcut("native", NativeRuntime::Windows));
        for runtime in [NativeRuntime::Unix, NativeRuntime::Windows] {
            assert!(!has_debug_shortcut("wasm", runtime));
        }
    }

    #[test]
    fn native_runtime_names_are_explicit() {
        assert_eq!(NativeRuntime::parse("unix").unwrap(), NativeRuntime::Unix);
        assert_eq!(
            NativeRuntime::parse("windows").unwrap(),
            NativeRuntime::Windows
        );
        assert!(NativeRuntime::parse("linux").is_err());
        assert_eq!(NativeRuntime::host() == NativeRuntime::Unix, cfg!(unix));
    }

    #[test]
    fn geometry_resolution_matches_layout_defaults_and_rejects_invalid_values() {
        assert_eq!(resolve_geometry(None, None).unwrap(), Geometry::MBR);
        assert_eq!(
            resolve_geometry(Some(Layout::Raw), None).unwrap(),
            Geometry::raw(4096)
        );
        assert_eq!(
            resolve_geometry(Some(Layout::Raw), Some(512)).unwrap(),
            Geometry::raw(512)
        );
        assert!(resolve_geometry(Some(Layout::Mbr), Some(4096)).is_err());
        assert!(resolve_geometry(Some(Layout::Raw), Some(3000)).is_err());
        assert!(parse_layout("other").is_err());
    }

    #[test]
    fn wt32_sc01_plus_profile_and_raw_image_carry_bundles_shortcuts_registry_and_seed_marker() {
        use file_system::{
            AccessFlags, AttributeFlags, Attributes, BaseOperations, Context, FileSystemOperations,
            Permissions,
        };
        use std::fs;

        let board_profile = profile("wt32-sc01-plus", NativeRuntime::Unix).unwrap();
        let browser_profile = profile("wasm", NativeRuntime::Unix).unwrap();
        assert_eq!(board_profile.applications, browser_profile.applications);
        let shortcut_names: Vec<&str> = board_profile
            .shortcuts
            .iter()
            .map(|file| file.name.as_str())
            .collect();
        assert_eq!(
            shortcut_names,
            [
                "activity.json",
                "file_manager.json",
                "settings.json",
                "terminal.json",
            ]
        );
        assert_eq!(board_profile.repositories[0].name, "xila.json");

        let root = env::temp_dir().join(format!("xila-fs-image-wt32-test-{}", process::id()));
        let _ = fs::remove_dir_all(&root);
        let bundles = root.join("bundles");
        for application in &board_profile.applications {
            let bundle = bundles.join(format!("xila.{}", application.replace('_', "-")));
            let executable = format!("{application}.wasm");
            fs::create_dir_all(bundle.join(".xila")).unwrap();
            fs::create_dir_all(bundle.join("executables")).unwrap();
            fs::write(bundle.join(".xila/manifest.json"), b"{}").unwrap();
            fs::write(bundle.join("shortcut.json"), b"{}").unwrap();
            fs::write(bundle.join("executables").join(executable), b"\0asm").unwrap();
        }

        let image_path = root.join("storage.img");
        let partition_table =
            include_str!("../../../examples/boards/wt32-sc01-plus/partitions.csv");
        let partition_size = partition_table
            .lines()
            .find_map(|line| {
                let mut fields = line.split(',').map(str::trim);
                (fields.next() == Some("storage")).then(|| fields.nth(3).unwrap())
            })
            .unwrap();
        let partition_size =
            u64::from_str_radix(partition_size.strip_prefix("0x").unwrap(), 16).unwrap();
        let geometry = Geometry::raw(4096);
        let staging = tools_image::stage(&board_profile, &bundles).unwrap();
        tools_image::build_with(&image_path, partition_size, Some(staging.path()), geometry)
            .unwrap();
        drop(staging);

        let image_bytes = fs::read(&image_path).unwrap();
        assert_eq!(image_bytes.len() as u64, partition_size);
        assert_ne!(image_bytes[510..512], [0x55, 0xAA]);

        let drive: &'static tools_image::FileDrive = Box::leak(Box::new(
            tools_image::FileDrive::open_with_block_size(&image_path, 4096)
                .unwrap()
                .with_flash_erase(),
        ));
        let file_system =
            little_fs::FileSystem::new(drive, little_fs::Options::new(256).set_block_cycles(500))
                .unwrap();

        let read_file = |path: &str| -> Vec<u8> {
            let mut context = Context::new_empty();
            file_system
                .lookup_file(&mut context, path.as_ref(), AccessFlags::Read.into())
                .unwrap();
            let mut output = Vec::new();
            let mut chunk = [0u8; 256];
            loop {
                let count = file_system
                    .read_sync(&mut context, &mut chunk, output.len() as u64)
                    .unwrap();
                if count == 0 {
                    break;
                }
                output.extend_from_slice(&chunk[..count]);
            }
            file_system.close(&mut context).unwrap();
            output
        };

        assert_eq!(
            read_file("/system/accounts/.seed-pending"),
            b"xila-seed-v1\n"
        );
        assert!(
            !read_file("/configuration/shared/applications/shortcuts/activity.json").is_empty()
        );
        assert!(
            !read_file("/configuration/shared/applications/shortcuts/file_manager.json").is_empty()
        );
        assert!(
            !read_file("/configuration/shared/applications/shortcuts/settings.json").is_empty()
        );
        assert!(
            !read_file("/configuration/shared/applications/shortcuts/terminal.json").is_empty()
        );
        assert!(!read_file("/configuration/system/packages/repositories/xila.json").is_empty());
        for application in &board_profile.applications {
            let package = application.replace('_', "-");
            assert_eq!(
                read_file(&format!(
                    "/applications/bundles/xila.{package}/executables/{application}.wasm"
                )),
                b"\0asm"
            );
        }
        for (path, mode) in [
            ("/system/accounts", 0o700),
            ("/system/accounts/.seed-pending", 0o644),
            (
                "/configuration/shared/applications/shortcuts/activity.json",
                0o644,
            ),
            (
                "/applications/bundles/xila.calculator/executables/calculator.wasm",
                0o644,
            ),
        ] {
            let mut attributes = Attributes::new().set_mask(AttributeFlags::all());
            file_system
                .get_attributes(path.as_ref(), &mut attributes)
                .unwrap();
            assert_eq!(
                attributes.get_permissions(),
                Permissions::from_octal(mode).as_ref(),
                "{path}"
            );
            assert_eq!(
                attributes.get_user(),
                Some(&users::UserIdentifier::ROOT),
                "{path}"
            );
            assert_eq!(
                attributes.get_group(),
                Some(&users::GroupIdentifier::ROOT),
                "{path}"
            );
        }

        let mut missing_debug = Context::new_empty();
        assert!(
            file_system
                .lookup_file(
                    &mut missing_debug,
                    "/configuration/shared/applications/shortcuts/debug.json".as_ref(),
                    AccessFlags::Read.into()
                )
                .is_err()
        );

        drop(file_system);
        drive.close();
        let _ = fs::remove_dir_all(&root);
    }
}
