//! The startup application: a device that boots into one application instead
//! of the graphical shell, such as a clock, a thermostat or a dashboard.
//!
//! [`CONFIGURATION_PATH`] (`/configuration/system/startup.json`) names the
//! application and the account it runs as:
//!
//! ```json
//! { "shortcut": "dev.xila.clock", "user": "administrator" }
//! ```
//!
//! `shortcut` is the identifier of an installed shortcut: an application
//! bundle (`/applications/bundles/<identifier>/shortcut.json`, with its
//! `@bundle/` paths resolved) or a native application shortcut
//! (`/configuration/shared/applications/shortcuts/<identifier>.json`). Any
//! installed application can therefore be the startup application. In its
//! place, `command` (an absolute executable path) and optional `arguments`
//! give the program directly.
//!
//! The application runs without a login, as `user`, on the screen, and starts
//! again whenever it ends ([`Restart`]). Without the file, a device starts as
//! before. [`load`] checks everything it can before the sessions start: an
//! invalid file, a file that someone other than root could have written, an
//! unknown account, or a missing or non-executable program is an [`Error`],
//! and the caller then starts the board's usual sessions, so the device stays
//! usable. There is no fallback to root: `"user": "root"` must be written
//! explicitly.

#![no_std]

extern crate alloc;

mod configuration;
mod restart;

use alloc::{format, string::String, vec::Vec};
use core::{fmt, time::Duration};

pub use configuration::{
    BUNDLES_PATH, CONFIGURATION_PATH, Configuration, ConfigurationError,
    MAXIMUM_CONFIGURATION_SIZE, MAXIMUM_SHORTCUT_SIZE, Program, SHORTCUTS_PATH, ShortcutLocation,
    ShortcutProgram, is_valid_identifier, parse_shortcut, program_name, shortcut_locations,
};
pub use restart::Restart;

use xila::{
    executable::{self, ExecutableContext, Standard},
    file_system::{IoErrorKind, Kind, Permission, Statistics},
    log, task,
    users::{self, GroupIdentifier, UserIdentifier},
    virtual_file_system::{File, VirtualFileSystem},
};

/// The terminal of the application: it reads and writes nothing.
pub const NULL_DEVICE: &str = "/devices/standard_io/null";

/// How often a restart delay checks for a lifecycle request.
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The application to start, checked by [`load`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    /// The shortcut identifier or the command, for the log.
    pub name: String,
    pub command: String,
    pub arguments: Vec<String>,
    pub user_name: String,
    pub user: UserIdentifier,
    /// The user's primary group.
    pub group: GroupIdentifier,
}

/// Why the startup application cannot start.
#[derive(Debug)]
pub enum Error {
    /// A file could not be read.
    Read {
        path: String,
        error: IoErrorKind,
    },
    /// The file is not a regular file, or is too large.
    NotAFile(String),
    TooLarge(String),
    InvalidUtf8(String),
    /// Someone other than root could have written the startup configuration.
    UnsafeConfiguration,
    Configuration(ConfigurationError),
    ShortcutNotFound(String),
    /// The configuration names an account that does not exist. The message
    /// names the file and its `user` field, but no account: an unknown account
    /// has no identifier, and the log identifies accounts by their identifier
    /// only.
    UnknownUser,
    /// The command does not exist.
    MissingCommand(String),
    /// The user may not run the command.
    NotExecutable {
        command: String,
        user: UserIdentifier,
    },
    /// A bundle file that the shortcut names does not exist.
    MissingFile(String),
    /// The session task could not take the application's identity.
    Identity(task::Error),
    Home(IoErrorKind),
    Terminal(executable::Error),
    Start(executable::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => write!(formatter, "cannot read {path}: {error}"),
            Self::NotAFile(path) => write!(formatter, "{path} is not a file"),
            Self::TooLarge(path) => write!(formatter, "{path} is too large"),
            Self::InvalidUtf8(path) => write!(formatter, "{path} is not UTF-8 text"),
            Self::UnsafeConfiguration => write!(
                formatter,
                "{CONFIGURATION_PATH} is ignored: it must belong to root and be writable by root only"
            ),
            Self::Configuration(error) => write!(formatter, "{CONFIGURATION_PATH}: {error}"),
            Self::ShortcutNotFound(identifier) => write!(
                formatter,
                "no shortcut \"{identifier}\" in {BUNDLES_PATH}/{identifier}/ or {SHORTCUTS_PATH}/"
            ),
            Self::UnknownUser => write!(
                formatter,
                "{CONFIGURATION_PATH}: the \"user\" field names no account"
            ),
            Self::MissingCommand(command) => write!(formatter, "no program {command}"),
            Self::NotExecutable { command, user } => {
                write!(formatter, "user {} may not run {command}", user.as_u16())
            }
            Self::MissingFile(path) => write!(formatter, "missing application file {path}"),
            Self::Identity(error) => {
                write!(formatter, "cannot take the user's identity: {error:?}")
            }
            Self::Home(error) => write!(formatter, "cannot prepare the user's home: {error}"),
            Self::Terminal(error) => write!(formatter, "cannot open the terminal: {error:?}"),
            Self::Start(error) => write!(formatter, "cannot start the application: {error:?}"),
        }
    }
}

impl From<ConfigurationError> for Error {
    fn from(error: ConfigurationError) -> Self {
        Self::Configuration(error)
    }
}

/// Reads and checks the startup configuration as `task`, which must be root
/// (the startup task). `Ok(None)` when there is no configuration: the device
/// starts its usual sessions.
pub async fn load(
    virtual_file_system: &VirtualFileSystem,
    users_manager: &users::Manager,
    task: task::TaskIdentifier,
) -> Result<Option<Application>, Error> {
    let statistics = match virtual_file_system
        .get_statistics(&CONFIGURATION_PATH)
        .await
    {
        Ok(statistics) => statistics,
        Err(IoErrorKind::NotFound) => return Ok(None),
        Err(error) => {
            return Err(Error::Read {
                path: CONFIGURATION_PATH.into(),
                error,
            });
        }
    };
    if !is_written_by_root_only(&statistics) {
        return Err(Error::UnsafeConfiguration);
    }
    let text = read_text(
        virtual_file_system,
        task,
        CONFIGURATION_PATH,
        &statistics,
        MAXIMUM_CONFIGURATION_SIZE,
    )
    .await?;
    let configuration = Configuration::parse(&text)?;
    let name = program_name(&configuration.program);

    let (command, arguments, bundle_files) = match configuration.program {
        Program::Shortcut(identifier) => {
            let program = read_shortcut(virtual_file_system, task, &identifier).await?;
            (program.command, program.arguments, program.bundle_files)
        }
        Program::Command { command, arguments } => (command, arguments, Vec::new()),
    };

    let user = users_manager
        .get_user_identifier(&configuration.user)
        .await
        .map_err(|_| Error::UnknownUser)?;
    let group = users_manager
        .get_user_primary_group(user)
        .await
        .map_err(|_| Error::UnknownUser)?;

    let statistics = match virtual_file_system.get_statistics(&command.as_str()).await {
        Ok(statistics) if statistics.kind != Kind::Directory => statistics,
        Ok(_) | Err(IoErrorKind::NotFound) => return Err(Error::MissingCommand(command)),
        Err(error) => {
            return Err(Error::Read {
                path: command,
                error,
            });
        }
    };
    if !executable::is_execute_allowed(users_manager, &statistics, user).await {
        return Err(Error::NotExecutable { command, user });
    }
    for path in bundle_files {
        match virtual_file_system.get_statistics(&path.as_str()).await {
            Ok(statistics) if statistics.kind == Kind::File => {}
            Ok(_) | Err(IoErrorKind::NotFound) => return Err(Error::MissingFile(path)),
            Err(error) => return Err(Error::Read { path, error }),
        }
    }

    Ok(Some(Application {
        name,
        command,
        arguments,
        user_name: configuration.user,
        user,
        group,
    }))
}

/// Whether only root can have written a file: it belongs to root, others may
/// not write it, and its group may only if it is root's group.
fn is_written_by_root_only(statistics: &Statistics) -> bool {
    statistics.user == UserIdentifier::ROOT
        && !statistics
            .permissions
            .get_others()
            .contains(Permission::Write)
        && (statistics.group == GroupIdentifier::ROOT
            || !statistics
                .permissions
                .get_group()
                .contains(Permission::Write))
}

/// Reads the first shortcut that `identifier` names.
async fn read_shortcut(
    virtual_file_system: &VirtualFileSystem,
    task: task::TaskIdentifier,
    identifier: &str,
) -> Result<ShortcutProgram, Error> {
    for location in shortcut_locations(identifier) {
        let statistics = match virtual_file_system
            .get_statistics(&location.path.as_str())
            .await
        {
            Ok(statistics) => statistics,
            Err(IoErrorKind::NotFound) => continue,
            Err(error) => {
                return Err(Error::Read {
                    path: location.path,
                    error,
                });
            }
        };
        let text = read_text(
            virtual_file_system,
            task,
            &location.path,
            &statistics,
            MAXIMUM_SHORTCUT_SIZE,
        )
        .await?;
        return Ok(parse_shortcut(&text, location.bundle.as_deref())?);
    }
    Err(Error::ShortcutNotFound(identifier.into()))
}

async fn read_text(
    virtual_file_system: &VirtualFileSystem,
    task: task::TaskIdentifier,
    path: &str,
    statistics: &Statistics,
    maximum_size: u64,
) -> Result<String, Error> {
    if statistics.kind != Kind::File {
        return Err(Error::NotAFile(path.into()));
    }
    if statistics.size > maximum_size {
        return Err(Error::TooLarge(path.into()));
    }
    let mut buffer = Vec::new();
    File::read_from_path(virtual_file_system, task, path, &mut buffer)
        .await
        .map_err(|error| Error::Read {
            path: path.into(),
            error,
        })?;
    if buffer.len() as u64 > maximum_size {
        return Err(Error::TooLarge(path.into()));
    }
    String::from_utf8(buffer).map_err(|_| Error::InvalidUtf8(path.into()))
}

/// Gives `task` the application's identity, as the shells do after a login:
/// the user's home (created if needed) in `HOME`, its name in `User`, then its
/// primary group and the user itself. `task` must still be root.
pub async fn enter(
    context: &ExecutableContext,
    task: task::TaskIdentifier,
    application: &Application,
) -> Result<(), Error> {
    let home = context
        .virtual_file_system
        .ensure_user_home(task, application.user)
        .await
        .map_err(Error::Home)?;
    let task_manager = context.task_manager;
    task_manager
        .set_environment_variable(task, "HOME", home.as_str())
        .await
        .map_err(Error::Identity)?;
    task_manager
        .set_environment_variable(task, "User", &application.user_name)
        .await
        .map_err(Error::Identity)?;
    task_manager
        .set_group(task, application.group)
        .await
        .map_err(Error::Identity)?;
    task_manager
        .set_user(task, application.user)
        .await
        .map_err(Error::Identity)
}

/// Gives `task` back the root identity that [`enter`] replaced, so that the
/// graphical shell can start in its place and log a user in.
pub async fn leave(context: &ExecutableContext, task: task::TaskIdentifier) -> Result<(), Error> {
    let task_manager = context.task_manager;
    task_manager
        .set_user(task, UserIdentifier::ROOT)
        .await
        .map_err(Error::Identity)?;
    task_manager
        .set_group(task, GroupIdentifier::ROOT)
        .await
        .map_err(Error::Identity)?;
    for name in ["HOME", "User"] {
        // The variable may not exist if `enter` stopped early.
        let _ = task_manager.remove_environment_variable(task, name).await;
    }
    Ok(())
}

/// Runs the application as its user on `task`, a session task of its own,
/// and starts it again whenever it ends ([`Restart`]), until a lifecycle
/// request. An error means it could not start: the caller may then [`leave`]
/// and start the graphical shell instead.
pub async fn run(
    context: &'static ExecutableContext,
    task: task::TaskIdentifier,
    application: &Application,
) -> Result<(), Error> {
    enter(context, task, application).await?;
    let mut restart = Restart::new();
    while xila::lifecycle::SYSTEM.is_running() {
        let standard = Standard::open(
            &NULL_DEVICE,
            &NULL_DEVICE,
            &NULL_DEVICE,
            task,
            context.virtual_file_system,
            context,
        )
        .await
        .map_err(Error::Terminal)?;
        let started = context.time_manager.monotonic_now();
        let program = match executable::execute(
            context,
            application.command.as_str(),
            application.arguments.clone(),
            standard,
            None,
        )
        .await
        {
            Ok(program) => program,
            // The supervisor may already be stopping the system.
            Err(_) if !xila::lifecycle::SYSTEM.is_running() => return Ok(()),
            Err(error) => return Err(Error::Start(error)),
        };
        let status = program.join().await;
        // A lifecycle request stops the application: that is no reason to
        // start it again.
        if !xila::lifecycle::SYSTEM.is_running() {
            break;
        }
        let duration = context.time_manager.monotonic_now().saturating_sub(started);
        let delay = restart.delay_after(duration);
        log::warning!(
            "{}: ended with status {status} after {} s; starting it again in {} s",
            application.name,
            duration.as_secs(),
            delay.as_secs()
        );
        let resume = context.time_manager.monotonic_now() + delay;
        while xila::lifecycle::SYSTEM.is_running() && context.time_manager.monotonic_now() < resume
        {
            task::Manager::sleep(LIFECYCLE_POLL_INTERVAL).await;
        }
    }
    Ok(())
}

/// The sessions to start when a startup application is configured: the ones
/// that keep running, such as the serial console for maintenance, and the
/// first one that the application replaces (the graphical shell), which is
/// started instead if the application cannot start.
pub struct Plan<'a, Session> {
    pub kept: Vec<&'a Session>,
    pub replaced: Option<&'a Session>,
}

/// Splits `sessions` for a startup application: `is_graphical` sessions are
/// replaced by the application, the others are kept.
pub fn plan<Session>(
    sessions: &[Session],
    is_graphical: impl Fn(&Session) -> bool,
) -> Plan<'_, Session> {
    let mut plan = Plan {
        kept: Vec::new(),
        replaced: None,
    };
    for session in sessions {
        if is_graphical(session) {
            plan.replaced.get_or_insert(session);
        } else {
            plan.kept.push(session);
        }
    }
    plan
}

/// A log line for a configured application. It names the account by its
/// identifier: account names stay out of the log.
pub fn describe(application: &Application) -> String {
    format!(
        "{} ({}) as user {}",
        application.name,
        application.command,
        application.user.as_u16()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use xila::file_system::Time;

    #[test]
    fn the_application_replaces_the_graphical_sessions_only() {
        let sessions = ["console", "graphical", "graphical touch"];
        let plan = plan(&sessions, |session| session.starts_with("graphical"));
        assert_eq!(plan.kept, [&"console"]);
        assert_eq!(plan.replaced, Some(&"graphical"));

        let plan = super::plan(&["console"], |session| session.starts_with("graphical"));
        assert_eq!(plan.kept, [&"console"]);
        assert_eq!(plan.replaced, None);
    }

    #[test]
    fn the_log_line_names_the_account_by_identifier() {
        let application = Application {
            name: "dev.xila.clock".into(),
            command: "/applications/executables/runtime".into(),
            arguments: Vec::new(),
            user_name: "kiosk".into(),
            user: UserIdentifier::new(1001),
            group: GroupIdentifier::new(1001),
        };
        let line = describe(&application);
        assert_eq!(
            line,
            "dev.xila.clock (/applications/executables/runtime) as user 1001"
        );
        assert!(!line.contains("kiosk"));
    }

    #[test]
    fn error_messages_name_the_account_by_identifier_only() {
        use alloc::string::ToString;
        let error = Error::NotExecutable {
            command: "/applications/executables/runtime".into(),
            user: UserIdentifier::new(1001),
        };
        assert_eq!(
            error.to_string(),
            "user 1001 may not run /applications/executables/runtime"
        );
        assert_eq!(
            Error::UnknownUser.to_string(),
            format!("{CONFIGURATION_PATH}: the \"user\" field names no account")
        );
    }

    fn statistics(user: u16, group: u16, permissions: u16) -> Statistics {
        let mut statistics = Statistics::new(
            0,
            1,
            0,
            Time::new(0),
            Time::new(0),
            Time::new(0),
            Time::new(0),
            Kind::File,
            xila::file_system::Permissions::from_octal(permissions).unwrap(),
            UserIdentifier::new(user),
            GroupIdentifier::new(group),
        );
        statistics.size = 10;
        statistics
    }

    #[test]
    fn only_root_may_write_the_configuration() {
        assert!(is_written_by_root_only(&statistics(0, 0, 0o644)));
        assert!(is_written_by_root_only(&statistics(0, 0, 0o664)));
        assert!(is_written_by_root_only(&statistics(0, 1000, 0o644)));
        assert!(!is_written_by_root_only(&statistics(0, 1000, 0o664)));
        assert!(!is_written_by_root_only(&statistics(0, 0, 0o646)));
        assert!(!is_written_by_root_only(&statistics(1000, 0, 0o644)));
    }
}
