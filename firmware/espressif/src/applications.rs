//! The native applications (`graphics` feature), the WebAssembly runtime and
//! the package installer (`wasm` feature), and statically linked C executables
//! (`native-linked` feature) mounted under `/applications/executables/`.
//!
//! They are mounted at `/applications/executables/<name>`, where the seed
//! image's shortcuts (`tools/fs_image/shortcuts`) and application bundles
//! expect them; the terminal starts the command-line shell from there too. That
//! directory is on the persistent LittleFS partition, so an entry may already
//! be there:
//!
//! - an executable entry, which an earlier boot mounted: it is replaced;
//! - anything else (a file, a directory or a link): it is user data, so it is
//!   kept, and that application is not mounted, with a warning.
//!
//! A failure never stops the start-up: the application is left out, with a
//! warning, and the sessions start.

use xila::{
    executable::{ExecutableContext, ExecutableTrait, ExecutableWrapper},
    file_system::{IoErrorKind, Kind, Permissions},
    log, task,
    virtual_file_system::VirtualFileSystem,
};

/// Mounts the native applications and WebAssembly runtime.
pub(crate) async fn mount(
    context: &'static ExecutableContext,
    virtual_file_system: &'static VirtualFileSystem,
    task: task::TaskIdentifier,
) {
    #[cfg(feature = "graphics")]
    {
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/command_line_shell",
            command_line_shell::ShellExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/activity",
            activity::ActivityExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/file_manager",
            file_manager::FileManagerExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/settings",
            settings::SettingsExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/terminal",
            terminal::TerminalExecutable,
        )
        .await;
    }
    #[cfg(feature = "wasm")]
    {
        mount_one(
            context,
            virtual_file_system,
            task,
            crate::WASM_PATH,
            wasm::WasmExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/package",
            package::PackageExecutable,
        )
        .await;
    }
    #[cfg(feature = "native-linked")]
    {
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/c_demo",
            c_demo::CDemoExecutable,
        )
        .await;
        mount_one(
            context,
            virtual_file_system,
            task,
            "/applications/executables/hexdump",
            hexdump::HexdumpExecutable,
        )
        .await;
    }
}

async fn mount_one(
    context: &'static ExecutableContext,
    virtual_file_system: &'static VirtualFileSystem,
    task: task::TaskIdentifier,
    path: &'static str,
    executable: impl ExecutableTrait + 'static,
) {
    match virtual_file_system
        .get_statistics_for_task(task, &path)
        .await
    {
        Ok(statistics) if statistics.kind == Kind::CharacterDevice => {
            // An earlier boot's mount: replace it.
            if let Err(error) = virtual_file_system.remove(task, path).await {
                log::warning!("Xila: {path} is not mounted: cannot replace the old entry: {error}");
                return;
            }
        }
        Ok(statistics) => {
            log::warning!(
                "Xila: {path} is not mounted: it holds a {:?}, which is kept",
                statistics.kind
            );
            return;
        }
        Err(IoErrorKind::NotFound) => {}
        Err(error) => {
            log::warning!("Xila: {path} is not mounted: {error}");
            return;
        }
    }
    let wrapper = ExecutableWrapper::new(executable, context);
    if let Err(error) = virtual_file_system
        .mount_character_device(task, path, wrapper)
        .await
    {
        log::warning!("Xila: {path} is not mounted: {error}");
        return;
    }
    if let Err(error) = virtual_file_system
        .set_permissions(task, path, Permissions::EXECUTABLE)
        .await
    {
        log::warning!("Xila: {path} cannot be made executable: {error}");
    }
}
