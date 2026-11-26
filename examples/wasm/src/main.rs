#![no_std]

extern crate alloc;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
let diskManager;
let diskStartup;
export async function selectedXilaDisk() {
    diskManager = await import(new URL('./disk_manager.mjs', document.baseURI));
    diskStartup = diskManager.takeDiskStartup();
    return diskStartup.name;
}
export function rebuildSelectedXilaDisk() {
    return diskStartup.rebuild;
}
export function setXilaSeedVersion(version) {
    diskManager.setSeedVersion(version);
}
export function recordXilaDiskSeedVersion(name, version) {
    diskManager.writeSeedVersion(name, version);
}
export function xilaDiskManagerReady(name) {
    diskManager.diskManagerReady(name);
}
export function xilaDiskManagerStartupFailed(name, message, canRebuild) {
    diskManager.diskManagerStartupFailed(name, message, canRebuild);
}
export function xilaDiskManagerError(message) {
    diskManager.diskManagerError(message);
}
"#)]
#[cfg(target_arch = "wasm32")]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = selectedXilaDisk)]
    async fn selected_xila_disk() -> alloc::string::String;

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = rebuildSelectedXilaDisk)]
    fn rebuild_selected_xila_disk() -> bool;

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = setXilaSeedVersion)]
    fn set_xila_seed_version(version: &str);

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = recordXilaDiskSeedVersion)]
    fn record_xila_disk_seed_version(name: &str, version: &str);

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = xilaDiskManagerReady)]
    fn disk_manager_ready(name: &str);

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = xilaDiskManagerStartupFailed)]
    fn disk_manager_startup_failed(name: &str, message: &str, can_rebuild: bool);

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = xilaDiskManagerError)]
    fn disk_manager_error(message: &str);
}

#[cfg(target_arch = "wasm32")]
mod power;
#[cfg(target_arch = "wasm32")]
mod storage;

#[cfg(target_arch = "wasm32")]
static TASK_MANAGER: xila::task::Manager = xila::task::Manager::new();

/// Maximum model size accepted by the dynamic CPU neural network loader (2 MiB).
#[cfg(all(target_arch = "wasm32", feature = "neural-network"))]
const CPU_NN_LOADER_MAXIMUM_MODEL_SIZE: usize = 2 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
#[xila::task::run(
    task_path = xila::task,
    executor = drivers_wasm::executor::instantiate_static_executor!(),
    manager = &TASK_MANAGER
)]
async fn main(task_manager: &'static xila::task::Manager) {
    // AudioWorklet runs this module in a worker-like context without `window`.
    // The worklet only needs the exported audio factory, not the Xila runtime.
    if web_sys::window().is_none() {
        return;
    }

    drivers_wasm::memory::instantiate_global_allocator!();

    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicBool, Ordering};
    use drivers_wasm::devices::{DriveDevice, graphics::GraphicsDevices};
    use wasm_bindgen::{JsCast, closure::Closure};
    use web_sys::{HtmlElement, MouseEvent};
    use xila::bootsplash::Bootsplash;
    use xila::executable::{self, Standard, mount_executables};
    use xila::file_system::mbr::Mbr;
    use xila::file_system::{IoErrorKind, Path};
    use xila::log;
    use xila::task;
    use xila::time::{self, Duration};
    use xila::virtual_file_system::mount_static;
    use xila::{authentication, graphics, users, virtual_file_system};

    console_error_panic_hook::set_once();

    // - Initialize the system
    log::initialize(&drivers_wasm::log::Logger).unwrap();

    // Initialize the task manager
    let task = task_manager.get_current_task_identifier().await;
    let users_manager: &'static users::Manager = Box::leak(Box::new(users::Manager::new()));
    let time_manager: &'static time::Manager<'static> = Box::leak(Box::new(
        time::Manager::new(&drivers_wasm::devices::TimeDevice).unwrap(),
    ));

    // Initialize storage before starting runtime services so startup failures
    // can return control to the disk panel.
    const DISK_SIZE: usize = 16 * 1024 * 1024;
    let selected_disk = selected_xila_disk().await;
    let persistent = selected_disk != "memory";
    let disk_label = if persistent { &selected_disk } else { "Memory" };
    let rebuild = rebuild_selected_xila_disk();
    let seed = DriveDevice::new_memory_from_seed(DISK_SIZE, "./seed.img").await;
    let seed_version = match &seed {
        Ok(seed) => seed.seed_fingerprint().await.unwrap_or_default(),
        Err(_) => alloc::string::String::new(),
    };
    set_xila_seed_version(&seed_version);

    let path = Path::from_str(&selected_disk);
    let opened = match seed {
        Ok(seed) if persistent => seed.into_persistent(path, rebuild).await,
        Ok(seed) => Ok(seed),
        Err(_) if persistent && !rebuild => DriveDevice::new(path, DISK_SIZE).await,
        Err(error) => Err(error),
    };
    let drive: &'static DriveDevice = match opened {
        Ok(drive) => Box::leak(Box::new(drive)),
        Err(error) => {
            disk_manager_startup_failed(
                &selected_disk,
                &alloc::format!("Could not open {disk_label}: {error}"),
                persistent
                    && matches!(
                        error,
                        IoErrorKind::InvalidArgument | IoErrorKind::CorruptData
                    ),
            );
            return;
        }
    };

    let file_system = match storage::mount(drive) {
        Ok(file_system) => file_system,
        Err(error) => {
            let _ = drive.discard_unsaved();
            disk_manager_startup_failed(
                &selected_disk,
                &alloc::format!("Could not mount {disk_label}: {error}"),
                persistent,
            );
            return;
        }
    };

    let mbr = Mbr::read_from_device(drive).unwrap();
    log::information!("MBR Information: {mbr}");

    let virtual_file_system = match virtual_file_system::initialize(
        task_manager,
        users_manager,
        time_manager,
        file_system,
    ) {
        Ok(virtual_file_system) => Box::leak(Box::new(virtual_file_system)),
        Err(error) => {
            let _ = drive.discard_unsaved();
            disk_manager_startup_failed(
                &selected_disk,
                &alloc::format!("Could not initialize {disk_label}: {error}"),
                persistent,
            );
            return;
        }
    };
    let virtual_file_system: &'static virtual_file_system::VirtualFileSystem = virtual_file_system;

    // - Initialize the graphics manager
    // - - Initialize the graphics driver
    let GraphicsDevices {
        screen_device,
        mouse_device,
        keyboard_device,
        canvas: _canvas,
    } = drivers_wasm::devices::graphics::new().await.unwrap();

    let resolution = drivers_wasm::devices::graphics::get_resolution().unwrap();

    // - - Initialize the graphics manager
    // CanvasScreenDevice flushes synchronously via putImageData, so one
    // row-bounded partial tile avoids allocating two full-resolution buffers.
    let graphics_manager = graphics::initialize(
        time_manager,
        Box::leak(Box::new(screen_device)),
        Box::leak(Box::new(mouse_device)),
        graphics::InputKind::Pointer,
        drivers_wasm::devices::graphics::draw_buffer_pixel_count(&resolution),
        false,
    )
    .await;
    let graphics_manager: &'static graphics::Manager = Box::leak(Box::new(graphics_manager));
    graphics::set_ffi_manager(graphics_manager);

    graphics_manager
        .add_input_device(
            Box::leak(Box::new(keyboard_device)),
            graphics::InputKind::Keypad,
        )
        .await
        .unwrap();

    // Touch screens get the system on-screen keyboard; Core opens it when a
    // text area is tapped.
    if drivers_wasm::devices::graphics::has_touch_screen().unwrap_or(false) {
        log::information!("Touch screen detected.");
        graphics_manager.enable_on_screen_keyboard().await;
    }

    let bootsplash = Bootsplash::new(graphics_manager).await.unwrap();

    task_manager
        .spawn(task, "Graphics", None, move |_| {
            graphics_manager.r#loop(task::Manager::sleep)
        })
        .await
        .unwrap();

    // - - Mount the devices
    virtual_file_system
        .mount_file_system(
            xila::memory_file_system::FileSystem::new(),
            Path::DEVICES,
            task,
        )
        .await
        .expect("Failed to mount the in-memory device file system");

    // - - Create the default system hierarchy
    virtual_file_system::create_default_hierarchy(virtual_file_system, task)
        .await
        .expect("Failed to create the default filesystem hierarchy");
    virtual_file_system::clear_temporary(virtual_file_system, task)
        .await
        .expect("Failed to clear temporary storage at boot");
    graphics::appearance::restore_appearance(graphics_manager, virtual_file_system, task).await;

    #[cfg(feature = "neural-network")]
    {
        virtual_file_system::ignore_already_exists_error(
            virtual_file_system
                .create_directory(task, &"/devices/nn")
                .await,
        )
        .expect("Failed to create NN device directory");
        drivers_shared::devices::neural_network::mount_loader(
            virtual_file_system,
            task,
            "/devices/nn/cpu",
            drivers_shared::devices::neural_network::Target::Cpu,
            CPU_NN_LOADER_MAXIMUM_MODEL_SIZE,
        )
        .await
        .expect("Failed to mount CPU neural network loader");
        drivers_shared::devices::neural_network::mount_backend_information(
            virtual_file_system,
            task,
            "/devices/nn/backend",
        )
        .await
        .expect("Failed to mount neural network backend information");
    }

    let document = web_sys::window()
        .and_then(|window| window.document())
        .expect("Failed to find the browser document");
    let audio_button = document
        .get_element_by_id("audio-enable")
        .and_then(|button| button.dyn_into::<HtmlElement>().ok())
        .expect("Failed to find the browser audio button in the help panel");
    let audio_status = document
        .get_element_by_id("audio-status")
        .and_then(|status| status.dyn_into::<HtmlElement>().ok())
        .expect("Failed to find the browser audio status in the help panel");

    let audio_button_for_callback = audio_button.clone();
    let audio_button_callback = Closure::wrap(Box::new(move |_event: MouseEvent| {
        let button = audio_button_for_callback.clone();
        let status = audio_status.clone();
        // A disabled button means a start is running or audio is running: ignore the click.
        if button.has_attribute("disabled") {
            return;
        }
        button.set_attribute("disabled", "true").unwrap();
        button.set_text_content(Some("Starting browser audio…"));
        status.set_hidden(true);
        wasm_bindgen_futures::spawn_local(async move {
            let device =
                match drivers_wasm::devices::WebAudioDevice::start_from_user_gesture().await {
                    Ok(device) => device,
                    Err(error) => {
                        log::error!("Failed to start browser audio: {error}");
                        button.set_text_content(Some("Audio start failed; click to retry"));
                        status.set_text_content(Some(&error.summary()));
                        status.set_hidden(false);
                        button.remove_attribute("disabled").unwrap();
                        return;
                    }
                };

            match virtual_file_system
                .mount_character_device(task, "/devices/audio/playback", device)
                .await
            {
                Ok(()) => {
                    log::information!("Browser audio: mounted at /devices/audio/playback");
                    button.set_text_content(Some("Audio enabled — launch Synth"));
                }
                Err(error) => {
                    log::error!("Failed to mount browser audio: {error}");
                    button.set_text_content(Some("Audio mount failed"));
                    status.set_text_content(Some(&format!(
                        "Browser audio started but could not be mounted: {error}"
                    )));
                    status.set_hidden(false);
                    button.remove_attribute("disabled").unwrap();
                }
            }
        });
    }) as Box<dyn FnMut(MouseEvent)>);
    audio_button
        .add_event_listener_with_callback("click", audio_button_callback.as_ref().unchecked_ref())
        .unwrap();
    audio_button_callback.forget();

    mount_static!(
        virtual_file_system,
        task,
        &[
            (
                &"/devices/standard_io/input",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (
                &"/devices/standard_io/output",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (
                &"/devices/standard_io/error",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (
                &"/devices/time/clock",
                CharacterDevice,
                drivers_wasm::devices::TimeDevice
            ),
            (
                &"/devices/compute/informations",
                CharacterDevice,
                drivers_wasm::devices::CpuInformationsDevice
            ),
            (
                &"/devices/random/source",
                CharacterDevice,
                drivers_shared::devices::RandomDevice
            ),
            (
                &"/devices/standard_io/null",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (
                &"/devices/network/http_client",
                CharacterDevice,
                drivers_wasm::devices::HttpClientDevice::HTTP
            ),
            (
                &"/devices/network/https_client",
                CharacterDevice,
                drivers_wasm::devices::HttpClientDevice::HTTPS
            ),
            (
                &"/devices/compute/hasher",
                CharacterDevice,
                drivers_shared::devices::HashDevice
            ),
        ]
    )
    .await
    .unwrap();

    // Mount static executables

    let executable_context: &'static executable::ExecutableContext =
        Box::leak(Box::new(executable::ExecutableContext {
            task_manager,
            users_manager,
            virtual_file_system,
            graphics_manager: Some(graphics_manager),
            network_manager: None,
            formats: &[],
            time_manager,
        }));

    mount_executables!(
        executable_context,
        virtual_file_system,
        task,
        &[
            (&"/applications/executables/wasm", wasm::WasmExecutable),
            (
                &"/applications/executables/package",
                package::PackageExecutable
            ),
            (
                &"/applications/executables/graphical_shell",
                graphical_shell::ShellExecutable
            ),
            (
                &"/applications/executables/command_line_shell",
                command_line_shell::ShellExecutable
            ),
            (
                &"/applications/executables/settings",
                settings::SettingsExecutable
            ),
            (
                &"/applications/executables/file_manager",
                file_manager::FileManagerExecutable
            ),
            (
                &"/applications/executables/terminal",
                terminal::TerminalExecutable
            ),
            (
                &"/applications/executables/activity",
                activity::ActivityExecutable
            ),
        ]
    )
    .await
    .unwrap();

    // - Execute the shell
    // - - Open the standard input, output and error
    let standard = Standard::open(
        &"/devices/standard_io/input",
        &"/devices/standard_io/output",
        &"/devices/standard_io/error",
        task,
        virtual_file_system,
        executable_context,
    )
    .await
    .unwrap();

    // - - Load saved accounts, or create defaults only on a fresh disk.
    let authentication_context = authentication::Context {
        virtual_file_system,
        task_manager,
        users_manager,
        task,
    };
    if let Err(error) = storage::initialize_users(&authentication_context, drive.is_new()).await {
        let _ = drive.discard_unsaved();
        disk_manager_startup_failed(
            &selected_disk,
            &alloc::format!("Could not load accounts on {disk_label}: {error}"),
            persistent,
        );
        return;
    }

    // System power device, the lifecycle supervisor's final step. Root and
    // the administrator group may use it, and therefore request a shutdown.
    drivers_peripherals::mount_peripheral(
        virtual_file_system,
        task,
        xila::lifecycle::POWER_DEVICE_PATH,
        drivers_peripherals::power::PowerHandle::new(drivers_wasm::devices::BrowserPower),
    )
    .await
    .expect("Failed to mount the power device");
    virtual_file_system
        .set_ownership(
            task,
            xila::lifecycle::POWER_DEVICE_PATH,
            None,
            Some(users::GroupIdentifier::new(1000)),
        )
        .await
        .expect("Failed to delegate the power device");
    virtual_file_system
        .set_permissions(
            task,
            xila::lifecycle::POWER_DEVICE_PATH,
            xila::file_system::Permissions::from_octal(0o660).unwrap(),
        )
        .await
        .expect("Failed to restrict the power device");

    let autosave = if persistent {
        if let Err(error) = drive.sync().await {
            let _ = drive.discard_unsaved();
            disk_manager_startup_failed(
                &selected_disk,
                &alloc::format!("Could not save {disk_label}: {error}"),
                false,
            );
            return;
        }
        if drive.is_new() {
            record_xila_disk_seed_version(&selected_disk, &seed_version);
        }

        // Keep one periodic save running and join it before the final save.
        let running: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(true)));
        let (handle, _) = task_manager
            .spawn(task, "Disk autosave", None, move |_| async move {
                loop {
                    task::Manager::sleep(Duration::from_secs(1)).await;
                    if !running.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) = drive.sync().await {
                        log::error!("Failed to save browser disk: {error}");
                    }
                }
            })
            .await
            .unwrap();
        Some((running, handle))
    } else {
        None
    };

    // A disk switch is an explicit restart of this instance: the lifecycle
    // stops the guests, commits the disk, releases its lock and reloads.
    let switch_requested: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let disk_name_for_switch = selected_disk.clone();
    let switch_callback: Closure<dyn FnMut()> = Closure::wrap(Box::new(move || {
        switch_requested.store(true, Ordering::Release);
    }) as Box<dyn FnMut()>);
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback(
            "xila-disk-switch",
            switch_callback.as_ref().unchecked_ref(),
        )
        .unwrap();
    switch_callback.forget();

    task_manager
        .spawn(task, "Disk manager", None, move |_| async move {
            while xila::lifecycle::SYSTEM.is_running() {
                if switch_requested.swap(false, Ordering::AcqRel) {
                    // Save once before committing to the restart: if this
                    // fails, nothing was stopped and the user can retry.
                    if let Err(error) = drive.sync().await {
                        let message =
                            alloc::format!("Could not save {disk_name_for_switch}: {error}");
                        disk_manager_error(&message);
                        continue;
                    }
                    let _ = xila::lifecycle::SYSTEM.request(xila::lifecycle::Action::Restart, 0);
                    break;
                }
                task::Manager::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();

    // - - Set the environment variables
    task_manager
        .set_environment_variable(task, "Paths", "/applications/executables")
        .await
        .unwrap();

    task_manager
        .set_environment_variable(task, "Host", "xila")
        .await
        .unwrap();

    // - - Time zone: the stored setting becomes `TZ` in this task and in the
    // services already started, so every task started from here inherits it.
    // A missing or invalid setting gives UTC with one warning.
    xila::local_time::initialize(task_manager, virtual_file_system, task).await;

    // wait some time to show the bootsplash
    task::Manager::sleep(Duration::from_secs(2)).await;

    bootsplash.stop(graphics_manager).await.unwrap();

    let arguments: Vec<alloc::string::String> = Vec::new();

    // Everything already running is a runtime service; the shell (or the
    // startup application) and what it launches are stopped by the lifecycle.
    let services = task_manager.get_children(task).await.unwrap();

    // - - A startup application (`/configuration/system/startup.json`)
    // replaces the graphical shell. If it is invalid or cannot start, the
    // graphical shell starts instead.
    let mut ready = false;
    let mut start_shell = true;
    let startup_application =
        match startup_application::load(virtual_file_system, users_manager, task).await {
            Ok(application) => application,
            Err(error) => {
                log::error!("Startup application: {error}; starting the shell instead");
                None
            }
        };
    if let Some(application) = startup_application {
        log::information!(
            "Startup application {}",
            startup_application::describe(&application)
        );
        let application: &'static startup_application::Application =
            Box::leak(Box::new(application));
        let failed: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        task_manager
            .spawn(task, "Application session", None, async move |session| {
                if let Err(error) =
                    startup_application::run(executable_context, session, application).await
                {
                    log::error!("{}: {error}; starting the shell instead", application.name);
                    failed.store(true, Ordering::Release);
                }
            })
            .await
            .unwrap();
        disk_manager_ready(&selected_disk);
        ready = true;
        // The application session runs until a lifecycle request.
        while !failed.load(Ordering::Acquire) {
            if !xila::lifecycle::SYSTEM.is_running() {
                start_shell = false;
                break;
            }
            task::Manager::sleep(Duration::from_millis(100)).await;
        }
    }

    // - - Execute the shell.
    if start_shell {
        let shell = executable::execute(
            executable_context,
            "/applications/executables/graphical_shell",
            arguments,
            standard,
            None,
        )
        .await
        .unwrap();
        if !ready {
            disk_manager_ready(&selected_disk);
        }
        let mut join = core::pin::pin!(shell.join());
        loop {
            use core::{
                future::{Future, poll_fn},
                task::Poll,
            };
            if poll_fn(|context| Poll::Ready(join.as_mut().poll(context)))
                .await
                .is_ready()
                || !xila::lifecycle::SYSTEM.is_running()
            {
                break;
            }
            task::Manager::sleep(Duration::from_millis(100)).await;
        }
    }

    // Leaving the shell halts this instance; the page stays open.
    let _ = xila::lifecycle::SYSTEM.request(xila::lifecycle::Action::PowerOff, 0);
    let supervisor = xila::lifecycle::Supervisor {
        virtual_file_system,
        time_manager,
        task,
        services: &services,
        budget: Duration::from_secs(5),
    };
    let mut drivers = power::BrowserDrivers::new(drive, autosave);
    if let Err(error) = supervisor.run(&xila::lifecycle::SYSTEM, &mut drivers).await {
        disk_manager_error(&alloc::format!("Xila could not shut down: {error}"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    extern crate std;
    panic!("This executable is only for the WebAssembly target.");
}
