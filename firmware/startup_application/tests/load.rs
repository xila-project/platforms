//! `load` on a LittleFS file system in memory, as the startup task sees it.
#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

extern crate alloc;
extern crate critical_section;

testing::instantiate_global_allocator!();

use startup_application::{Application, CONFIGURATION_PATH, ConfigurationError, Error, load};
use xila::{
    file_system::{AccessFlags, CreateFlags, Flags, MemoryDevice, Permissions},
    task::TaskIdentifier,
    users::{self, GroupIdentifier, UserIdentifier},
    virtual_file_system::{self, File, VirtualFileSystem},
};

const KIOSK: UserIdentifier = UserIdentifier::new(1001);
const KIOSK_GROUP: GroupIdentifier = GroupIdentifier::new(1001);
const RUNTIME: &str = "/applications/executables/runtime";
const CLOCK_BUNDLE: &str = "/applications/bundles/dev.xila.clock";

struct System {
    file_system: &'static VirtualFileSystem,
    users: &'static users::Manager,
    task: TaskIdentifier,
}

/// A fresh system with a `kiosk` account and an executable runtime.
async fn system() -> System {
    let tasks = Box::leak(Box::new(task::Manager::new()));
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(time::Manager::new(&testing::TimeDevice).unwrap()));
    let task = tasks.get_current_task_identifier().await;
    let disk = MemoryDevice::<512>::new_static(512 * 1024);
    little_fs::FileSystem::format(disk, little_fs::Options::new(256)).unwrap();
    let little_file_system =
        little_fs::FileSystem::new(disk, little_fs::Options::new(256)).unwrap();
    let file_system: &'static VirtualFileSystem = Box::leak(Box::new(
        virtual_file_system::initialize(tasks, users, time, little_file_system).unwrap(),
    ));
    virtual_file_system::create_default_hierarchy(file_system, task)
        .await
        .unwrap();
    users.add_group(KIOSK_GROUP, "kiosk", &[]).await.unwrap();
    users
        .add_user(KIOSK, "kiosk", KIOSK_GROUP, "/users/kiosk")
        .await
        .unwrap();
    let system = System {
        file_system,
        users,
        task,
    };
    system.write(RUNTIME, b"runtime").await;
    file_system
        .set_permissions(task, RUNTIME, Permissions::EXECUTABLE)
        .await
        .unwrap();
    system
}

impl System {
    async fn write(&self, path: &str, content: &[u8]) {
        if let Some((parent, _)) = path.rsplit_once('/') {
            let mut directory = String::new();
            for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
                directory.push('/');
                directory.push_str(segment);
                let _ = self
                    .file_system
                    .create_directory(self.task, &directory.as_str())
                    .await;
            }
        }
        let mut file = File::open(
            self.file_system,
            self.task,
            path,
            Flags::new(AccessFlags::Write, Some(CreateFlags::CREATE_TRUNCATE), None),
        )
        .await
        .unwrap();
        file.write(content).await.unwrap();
        file.close(self.file_system).await.unwrap();
    }

    async fn configure(&self, configuration: &str) {
        self.write(CONFIGURATION_PATH, configuration.as_bytes())
            .await;
    }

    async fn install_clock(&self) {
        self.write(
            &format!("{CLOCK_BUNDLE}/shortcut.json"),
            format!(
                r#"{{"name": "Clock", "command": "{RUNTIME}", "arguments": ["@bundle/executables/clock.wasm", "--", "24h"], "terminal": false, "icon_string": "Cl", "icon_color": [0, 0, 0]}}"#
            )
            .as_bytes(),
        )
        .await;
        self.write(&format!("{CLOCK_BUNDLE}/executables/clock.wasm"), b"\0asm")
            .await;
    }

    async fn load(&self) -> Result<Option<Application>, Error> {
        load(self.file_system, self.users, self.task).await
    }
}

#[task::test]
async fn without_a_configuration_the_device_starts_as_before() {
    let system = system().await;
    assert!(matches!(system.load().await, Ok(None)));
}

#[task::test]
async fn a_bundle_shortcut_runs_as_the_configured_user() {
    let system = system().await;
    system.install_clock().await;
    system
        .configure(r#"{"shortcut": "dev.xila.clock", "user": "kiosk"}"#)
        .await;
    assert_eq!(
        system.load().await.unwrap(),
        Some(Application {
            name: "dev.xila.clock".into(),
            command: RUNTIME.into(),
            arguments: vec![
                format!("{CLOCK_BUNDLE}/executables/clock.wasm"),
                "--".into(),
                "24h".into()
            ],
            user_name: "kiosk".into(),
            user: KIOSK,
            group: KIOSK_GROUP,
        })
    );
}

#[task::test]
async fn a_native_shortcut_is_found_by_name() {
    let system = system().await;
    system
        .write(
            "/configuration/shared/applications/shortcuts/settings.json",
            format!(r#"{{"command": "{RUNTIME}", "arguments": ["@bundle/kept"]}}"#).as_bytes(),
        )
        .await;
    system
        .configure(r#"{"shortcut": "settings", "user": "kiosk"}"#)
        .await;
    let application = system.load().await.unwrap().unwrap();
    assert_eq!(application.command, RUNTIME);
    assert_eq!(application.arguments, ["@bundle/kept"]);
}

#[task::test]
async fn an_explicit_command_runs_with_its_arguments() {
    let system = system().await;
    system
        .configure(&format!(
            r#"{{"command": "{RUNTIME}", "arguments": ["a", "b"], "user": "kiosk"}}"#
        ))
        .await;
    let application = system.load().await.unwrap().unwrap();
    assert_eq!(application.name, RUNTIME);
    assert_eq!(application.arguments, ["a", "b"]);
}

#[task::test]
async fn problems_are_reported_instead_of_starting_something_else() {
    let system = system().await;

    system.configure("{").await;
    assert!(matches!(
        system.load().await,
        Err(Error::Configuration(ConfigurationError::Syntax))
    ));

    system
        .configure(r#"{"shortcut": "dev.xila.clock", "user": "kiosk"}"#)
        .await;
    assert!(matches!(
        system.load().await,
        Err(Error::ShortcutNotFound(identifier)) if identifier == "dev.xila.clock"
    ));

    system.install_clock().await;
    system
        .configure(r#"{"shortcut": "dev.xila.clock", "user": "nobody"}"#)
        .await;
    assert!(matches!(system.load().await, Err(Error::UnknownUser)));

    system
        .configure(r#"{"command": "/applications/executables/missing", "user": "kiosk"}"#)
        .await;
    assert!(matches!(system.load().await, Err(Error::MissingCommand(_))));

    system
        .file_system
        .remove(
            system.task,
            &format!("{CLOCK_BUNDLE}/executables/clock.wasm").as_str(),
        )
        .await
        .unwrap();
    system
        .configure(r#"{"shortcut": "dev.xila.clock", "user": "kiosk"}"#)
        .await;
    assert!(matches!(
        system.load().await,
        Err(Error::MissingFile(path)) if path.ends_with("/clock.wasm")
    ));
}

#[task::test]
async fn the_user_must_be_allowed_to_run_the_command() {
    let system = system().await;
    system
        .file_system
        .set_permissions(
            system.task,
            RUNTIME,
            Permissions::from_octal(0o700).unwrap(),
        )
        .await
        .unwrap();
    system
        .configure(&format!(r#"{{"command": "{RUNTIME}", "user": "kiosk"}}"#))
        .await;
    assert!(matches!(
        system.load().await,
        Err(Error::NotExecutable { command, user }) if command == RUNTIME && user == KIOSK
    ));
    // Root may run it, but only when the configuration says so.
    system
        .configure(&format!(r#"{{"command": "{RUNTIME}", "user": "root"}}"#))
        .await;
    assert_eq!(
        system.load().await.unwrap().unwrap().user,
        UserIdentifier::ROOT
    );
}

#[task::test]
async fn a_configuration_that_others_could_write_is_ignored() {
    let system = system().await;
    system
        .configure(&format!(r#"{{"command": "{RUNTIME}", "user": "kiosk"}}"#))
        .await;
    system
        .file_system
        .set_ownership(system.task, CONFIGURATION_PATH, Some(KIOSK), None)
        .await
        .unwrap();
    assert!(matches!(
        system.load().await,
        Err(Error::UnsafeConfiguration)
    ));
    system
        .file_system
        .set_ownership(
            system.task,
            CONFIGURATION_PATH,
            Some(UserIdentifier::ROOT),
            None,
        )
        .await
        .unwrap();
    system
        .file_system
        .set_permissions(
            system.task,
            CONFIGURATION_PATH,
            Permissions::from_octal(0o666).unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        system.load().await,
        Err(Error::UnsafeConfiguration)
    ));
}
