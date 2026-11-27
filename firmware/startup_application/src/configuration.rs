//! The startup configuration file and the shortcuts it can name: parsing and
//! validation, without the file system.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

/// The startup configuration. Only root may write it: it chooses the user the
/// device runs as without a login.
pub const CONFIGURATION_PATH: &str = "/configuration/system/startup.json";

/// The largest startup configuration file read, in bytes.
pub const MAXIMUM_CONFIGURATION_SIZE: u64 = 4096;

/// The largest shortcut file read, in bytes.
pub const MAXIMUM_SHORTCUT_SIZE: u64 = 4096;

/// The installed application bundles: `<identifier>/shortcut.json` in each.
pub const BUNDLES_PATH: &str = "/applications/bundles";

/// The shortcuts of the native applications: `<identifier>.json`.
pub const SHORTCUTS_PATH: &str = "/configuration/shared/applications/shortcuts";

/// The prefix of a shortcut command or argument relative to its bundle.
const BUNDLE_PREFIX: &str = "@bundle/";

/// The longest shortcut identifier accepted.
const MAXIMUM_IDENTIFIER_LENGTH: usize = 128;

/// What the startup application runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// The command and arguments of an installed shortcut, by identifier: a
    /// bundle identifier such as `dev.xila.clock`, or a native application
    /// shortcut such as `settings`.
    Shortcut(String),
    /// An explicit command, an absolute executable path, with its arguments.
    Command {
        command: String,
        arguments: Vec<String>,
    },
}

/// The parsed startup configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub program: Program,
    /// The name of the account the application runs as.
    pub user: String,
}

/// The file as written. Unknown keys, such as `$schema`, are ignored.
#[derive(miniserde::Deserialize)]
struct ConfigurationFile {
    shortcut: Option<String>,
    command: Option<String>,
    arguments: Option<Vec<String>>,
    user: Option<String>,
}

/// Why a startup configuration or a shortcut is invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    /// Not a JSON object with the expected value types.
    Syntax,
    MissingUser,
    /// Neither `shortcut` nor `command`.
    MissingProgram,
    /// Both `shortcut` and `command`.
    AmbiguousProgram,
    /// `arguments` with a shortcut, which brings its own.
    ArgumentsWithShortcut,
    InvalidShortcutIdentifier(String),
    /// A command that is not an absolute path.
    RelativeCommand(String),
    /// A shortcut that is not a JSON object with a command and arguments.
    InvalidShortcut,
    /// A `@bundle/` path that is empty or leaves its bundle.
    InvalidBundlePath(String),
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => write!(
                formatter,
                "not a JSON object with string values for shortcut, command and user"
            ),
            Self::MissingUser => write!(formatter, "it names no user"),
            Self::MissingProgram => write!(formatter, "it names neither a shortcut nor a command"),
            Self::AmbiguousProgram => write!(formatter, "it names both a shortcut and a command"),
            Self::ArgumentsWithShortcut => write!(
                formatter,
                "arguments only go with a command: a shortcut brings its own"
            ),
            Self::InvalidShortcutIdentifier(identifier) => {
                write!(formatter, "invalid shortcut identifier \"{identifier}\"")
            }
            Self::RelativeCommand(command) => {
                write!(
                    formatter,
                    "the command \"{command}\" is not an absolute path"
                )
            }
            Self::InvalidShortcut => write!(
                formatter,
                "the shortcut is not a JSON object with a command and arguments"
            ),
            Self::InvalidBundlePath(path) => {
                write!(formatter, "the shortcut path \"{path}\" leaves its bundle")
            }
        }
    }
}

impl Configuration {
    /// Parses the content of [`CONFIGURATION_PATH`].
    pub fn parse(text: &str) -> Result<Self, ConfigurationError> {
        let file: ConfigurationFile =
            miniserde::json::from_str(text).map_err(|_| ConfigurationError::Syntax)?;
        let user = match file.user {
            Some(user) if !user.is_empty() => user,
            _ => return Err(ConfigurationError::MissingUser),
        };
        let program = match (file.shortcut, file.command) {
            (Some(_), Some(_)) => return Err(ConfigurationError::AmbiguousProgram),
            (None, None) => return Err(ConfigurationError::MissingProgram),
            (Some(identifier), None) => {
                if file.arguments.is_some() {
                    return Err(ConfigurationError::ArgumentsWithShortcut);
                }
                if !is_valid_identifier(&identifier) {
                    return Err(ConfigurationError::InvalidShortcutIdentifier(identifier));
                }
                Program::Shortcut(identifier)
            }
            (None, Some(command)) => {
                if !command.starts_with('/') {
                    return Err(ConfigurationError::RelativeCommand(command));
                }
                Program::Command {
                    command,
                    arguments: file.arguments.unwrap_or_default(),
                }
            }
        };
        Ok(Self { program, user })
    }
}

/// Whether `identifier` can name a shortcut: a file or directory name of ASCII
/// letters, digits, `.`, `-` and `_` that does not start with a dot.
pub fn is_valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAXIMUM_IDENTIFIER_LENGTH
        && !identifier.starts_with('.')
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Where a shortcut identifier may be installed, in the order searched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutLocation {
    /// The shortcut file.
    pub path: String,
    /// The bundle directory that `@bundle/` paths are relative to, for a
    /// bundle shortcut. Native application shortcuts have none, and their
    /// paths are used unchanged, as the graphical shell does.
    pub bundle: Option<String>,
}

/// The shortcut files `identifier` may name: the bundle's own shortcut first,
/// then a native application shortcut. `identifier` must be valid
/// ([`is_valid_identifier`]).
pub fn shortcut_locations(identifier: &str) -> [ShortcutLocation; 2] {
    let bundle = format!("{BUNDLES_PATH}/{identifier}");
    [
        ShortcutLocation {
            path: format!("{bundle}/shortcut.json"),
            bundle: Some(bundle),
        },
        ShortcutLocation {
            path: format!("{SHORTCUTS_PATH}/{identifier}.json"),
            bundle: None,
        },
    ]
}

/// What a shortcut runs, with its `@bundle/` paths resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutProgram {
    pub command: String,
    pub arguments: Vec<String>,
    /// The bundle files that the command or arguments name, which must exist
    /// for the application to start.
    pub bundle_files: Vec<String>,
}

/// The part of a shortcut that says what it runs. Its presentation (name,
/// icon) is the graphical shell's business.
#[derive(miniserde::Deserialize)]
struct ShortcutFile {
    command: String,
    arguments: Vec<String>,
}

/// Parses a shortcut (`schemas/shell-shortcut.schema.json` of
/// `xila-project/applications`). For a bundle shortcut, `@bundle/` paths are
/// resolved against `bundle` and rejected when they could leave it, exactly
/// as the graphical shell does.
pub fn parse_shortcut(
    text: &str,
    bundle: Option<&str>,
) -> Result<ShortcutProgram, ConfigurationError> {
    let file: ShortcutFile =
        miniserde::json::from_str(text).map_err(|_| ConfigurationError::InvalidShortcut)?;
    let mut bundle_files = Vec::new();
    let mut resolve = |value: String| -> Result<String, ConfigurationError> {
        let Some(bundle) = bundle else {
            return Ok(value);
        };
        let Some(relative) = value.strip_prefix(BUNDLE_PREFIX) else {
            return Ok(value);
        };
        if relative.is_empty()
            || relative.contains('\\')
            || relative
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(ConfigurationError::InvalidBundlePath(value));
        }
        let path = format!("{bundle}/{relative}");
        bundle_files.push(path.clone());
        Ok(path)
    };
    let command = resolve(file.command)?;
    let arguments = file
        .arguments
        .into_iter()
        .map(&mut resolve)
        .collect::<Result<Vec<_>, _>>()?;
    if !command.starts_with('/') {
        return Err(ConfigurationError::RelativeCommand(command));
    }
    Ok(ShortcutProgram {
        command,
        arguments,
        bundle_files,
    })
}

/// A short name for the program in the log: the shortcut identifier, or the
/// command.
pub fn program_name(program: &Program) -> String {
    match program {
        Program::Shortcut(identifier) => identifier.clone(),
        Program::Command { command, .. } => command.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_shortcut_and_a_user() {
        let configuration = Configuration::parse(
            r#"{
                "$schema": "https://example.invalid/startup.schema.json",
                "shortcut": "dev.xila.clock",
                "user": "administrator"
            }"#,
        )
        .unwrap();
        assert_eq!(
            configuration,
            Configuration {
                program: Program::Shortcut("dev.xila.clock".into()),
                user: "administrator".into(),
            }
        );
    }

    #[test]
    fn a_command_with_or_without_arguments() {
        let configuration = Configuration::parse(
            r#"{"command": "/applications/executables/wasm", "arguments": ["/a.wasm", "--", "x"], "user": "kiosk"}"#,
        )
        .unwrap();
        assert_eq!(
            configuration.program,
            Program::Command {
                command: "/applications/executables/wasm".into(),
                arguments: vec!["/a.wasm".into(), "--".into(), "x".into()],
            }
        );
        let configuration = Configuration::parse(
            r#"{"command": "/applications/executables/settings", "user": "kiosk"}"#,
        )
        .unwrap();
        assert_eq!(
            configuration.program,
            Program::Command {
                command: "/applications/executables/settings".into(),
                arguments: vec![],
            }
        );
    }

    #[test]
    fn invalid_configurations_say_why() {
        for (text, error) in [
            ("", ConfigurationError::Syntax),
            ("[]", ConfigurationError::Syntax),
            (
                r#"{"shortcut": 3, "user": "a"}"#,
                ConfigurationError::Syntax,
            ),
            (
                r#"{"shortcut": "dev.xila.clock"}"#,
                ConfigurationError::MissingUser,
            ),
            (
                r#"{"shortcut": "dev.xila.clock", "user": ""}"#,
                ConfigurationError::MissingUser,
            ),
            (r#"{"user": "a"}"#, ConfigurationError::MissingProgram),
            (
                r#"{"shortcut": "dev.xila.clock", "command": "/x", "user": "a"}"#,
                ConfigurationError::AmbiguousProgram,
            ),
            (
                r#"{"shortcut": "dev.xila.clock", "arguments": [], "user": "a"}"#,
                ConfigurationError::ArgumentsWithShortcut,
            ),
            (
                r#"{"command": "wasm", "user": "a"}"#,
                ConfigurationError::RelativeCommand("wasm".into()),
            ),
            (
                r#"{"shortcut": "../system", "user": "a"}"#,
                ConfigurationError::InvalidShortcutIdentifier("../system".into()),
            ),
        ] {
            assert_eq!(Configuration::parse(text), Err(error), "{text}");
        }
    }

    #[test]
    fn identifiers_are_single_safe_names() {
        for identifier in [
            "dev.xila.clock",
            "file_manager",
            "dev.xila.pixel-painter",
            "a",
        ] {
            assert!(is_valid_identifier(identifier), "{identifier}");
        }
        for identifier in ["", ".", "..", ".hidden", "a/b", "a\\b", "a b", "é"] {
            assert!(!is_valid_identifier(identifier), "{identifier}");
        }
        assert!(!is_valid_identifier(&"a".repeat(129)));
    }

    #[test]
    fn bundle_shortcuts_come_before_native_shortcuts() {
        assert_eq!(
            shortcut_locations("dev.xila.clock"),
            [
                ShortcutLocation {
                    path: "/applications/bundles/dev.xila.clock/shortcut.json".into(),
                    bundle: Some("/applications/bundles/dev.xila.clock".into()),
                },
                ShortcutLocation {
                    path: "/configuration/shared/applications/shortcuts/dev.xila.clock.json".into(),
                    bundle: None,
                },
            ]
        );
    }

    const CLOCK: &str = r#"{
        "$schema": "https://raw.githubusercontent.com/xila-project/applications/main/schemas/shell-shortcut.schema.json",
        "name": "Clock",
        "command": "/applications/executables/wasm",
        "arguments": ["@bundle/executables/clock.wasm"],
        "terminal": false,
        "icon_string": "Cl",
        "icon_color": [98, 160, 234]
    }"#;

    #[test]
    fn bundle_shortcuts_resolve_their_paths() {
        let program = parse_shortcut(CLOCK, Some("/applications/bundles/dev.xila.clock")).unwrap();
        assert_eq!(
            program,
            ShortcutProgram {
                command: "/applications/executables/wasm".into(),
                arguments: vec![
                    "/applications/bundles/dev.xila.clock/executables/clock.wasm".into()
                ],
                bundle_files: vec![
                    "/applications/bundles/dev.xila.clock/executables/clock.wasm".into()
                ],
            }
        );
    }

    #[test]
    fn native_shortcuts_keep_their_paths() {
        let program = parse_shortcut(
            include_str!("../../../tools/fs_image/shortcuts/settings.json"),
            None,
        )
        .unwrap();
        assert_eq!(program.command, "/applications/executables/settings");
        assert!(program.bundle_files.is_empty());
    }

    #[test]
    fn bundle_paths_cannot_leave_their_bundle() {
        for path in [
            "@bundle/../escape",
            "@bundle/a/../../escape",
            "@bundle/",
            "@bundle//x",
            "@bundle/a\\b",
        ] {
            let text = format!(
                r#"{{"command": "/x", "arguments": ["{}"]}}"#,
                path.replace('\\', "\\\\")
            );
            assert_eq!(
                parse_shortcut(&text, Some("/applications/bundles/b")),
                Err(ConfigurationError::InvalidBundlePath(path.into())),
                "{path}"
            );
        }
    }

    #[test]
    fn a_shortcut_needs_an_absolute_command_and_arguments() {
        assert_eq!(
            parse_shortcut(r#"{"command": "/x"}"#, None),
            Err(ConfigurationError::InvalidShortcut)
        );
        assert_eq!(
            parse_shortcut(r#"{"command": "x", "arguments": []}"#, None),
            Err(ConfigurationError::RelativeCommand("x".into()))
        );
        assert_eq!(
            parse_shortcut(
                r#"{"command": "@bundle/executables/x", "arguments": []}"#,
                Some("/applications/bundles/b")
            )
            .unwrap()
            .command,
            "/applications/bundles/b/executables/x"
        );
    }
}
