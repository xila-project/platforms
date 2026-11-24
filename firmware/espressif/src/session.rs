//! Login sessions.
//!
//! Each session is a task under the startup task. It runs its shell on its own
//! terminal, and runs it again whenever it ends, until a lifecycle request.
//! The shell asks for a login itself, so every session has its own user. The
//! startup task then waits for the request, and the lifecycle supervisor stops
//! every session together with the other tasks that are not services.
//!
//! Boards list their sessions in [`crate::Board::sessions`]: the serial
//! console session for now. A graphical session can run beside it as a second
//! task with its own shell and display.

use alloc::{string::String, vec::Vec};

use xila::{
    executable::{self, ExecutableContext, Standard},
    log, task,
};

/// The terminal and the shell of a session.
pub struct Session {
    pub name: &'static str,
    pub input: &'static str,
    pub output: &'static str,
    pub error: &'static str,
    pub shell: &'static str,
    /// The shell's arguments.
    pub arguments: &'static [&'static str],
}

/// The graphical shell on the screen. It reads no terminal.
pub const GRAPHICAL: Session = Session {
    name: "Graphical session",
    input: "/devices/standard_io/null",
    output: "/devices/standard_io/null",
    error: "/devices/standard_io/null",
    shell: crate::GRAPHICAL_SHELL_PATH,
    arguments: &[],
};

/// The graphical shell on a touch screen. The display enables the keyboard.
pub const GRAPHICAL_TOUCH: Session = GRAPHICAL;

/// The command-line shell on the console.
pub const CONSOLE: Session = Session {
    name: "Console session",
    input: "/devices/standard_io/input",
    output: "/devices/standard_io/output",
    error: "/devices/standard_io/error",
    shell: crate::SHELL_PATH,
    arguments: &[],
};

/// Starts `session` as a child task of `parent`.
pub(super) async fn spawn(
    task_manager: &'static task::Manager,
    parent: task::TaskIdentifier,
    executable_context: &'static ExecutableContext,
    session: &'static Session,
) {
    task_manager
        .spawn(parent, session.name, None, async move |task| {
            run(executable_context, task, session).await;
        })
        .await
        .expect("Failed to spawn a session");
}

async fn run(
    executable_context: &'static ExecutableContext,
    task: task::TaskIdentifier,
    session: &Session,
) {
    while xila::lifecycle::SYSTEM.is_running() {
        let standard = match Standard::open(
            &session.input,
            &session.output,
            &session.error,
            task,
            executable_context.virtual_file_system,
            executable_context,
        )
        .await
        {
            Ok(standard) => standard,
            Err(error) => {
                log::error!("{}: cannot open the terminal: {error:?}", session.name);
                return;
            }
        };
        let arguments: Vec<String> = session
            .arguments
            .iter()
            .copied()
            .map(String::from)
            .collect();
        let shell =
            match executable::execute(executable_context, session.shell, arguments, standard, None)
                .await
            {
                Ok(shell) => shell,
                // The supervisor may already be stopping the system.
                Err(_) if !xila::lifecycle::SYSTEM.is_running() => return,
                Err(error) => {
                    log::error!("{}: cannot start the shell: {error:?}", session.name);
                    return;
                }
            };
        let status = shell.join().await;
        log::information!("{}: the shell ended with status {status}", session.name);
    }
}
