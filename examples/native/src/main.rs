#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod cli;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod native_executables;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod power;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod storage;

static TASK_MANAGER: xila::task::Manager = xila::task::Manager::new();

/// Maximum model size accepted by the dynamic CPU neural network loader (4 MiB).
#[cfg(feature = "neural-network")]
const CPU_NN_LOADER_MAXIMUM_MODEL_SIZE: usize = 4 * 1024 * 1024;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[xila::task::run(
    task_path = xila::task,
    executor = drivers_std::executor::instantiate_static_executor!(),
    manager = &TASK_MANAGER
)]
async fn main(task_manager: &'static xila::task::Manager) {
    drivers_std::memory::instantiate_global_allocator!();
    #[cfg(all(feature = "profiling", target_os = "linux"))]
    drivers_std::profiling::start_from_environment().expect("Failed to start profiling snapshots");

    extern crate alloc;

    use alloc::vec;
    use core::time::Duration;
    use xila::authentication;
    use xila::bootsplash::Bootsplash;
    use xila::executable;
    use xila::executable::Standard;
    use xila::executable::mount_executables;
    use xila::file_system::AccessFlags;
    use xila::graphics;
    use xila::little_fs;
    use xila::log;
    use xila::network::{self, ADD_DNS_SERVER, ADD_IP_ADDRESS, ADD_ROUTE};
    use xila::task;
    use xila::time;
    use xila::users;
    use xila::virtual_file_system;
    use xila::virtual_file_system::File;
    use xila::virtual_file_system::mount_static;

    power::install_signal_handlers();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let config = match cli::parse(arguments.iter().map(String::as_str)) {
        Ok(config) => config,
        Err(error) if error.is_help() || error.is_version() => {
            println!("{error}");
            // Returning from this task does not stop the native executor.
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Invalid command-line arguments: {error}\nTry --help for usage.");
            std::process::exit(2);
        }
    };
    if config.interface == cli::InterfaceMode::Graphical
        && config.display_driver == cli::DisplayDriver::Headless
        && (config.resolution.0 as usize)
            .checked_mul(config.resolution.1 as usize)
            .is_none_or(|area| area > xila::graphics::SCREEN_SNAPSHOT_MAX_PIXELS)
    {
        eprintln!(
            "Invalid headless display resolution: area exceeds the {} pixel limit.",
            xila::graphics::SCREEN_SNAPSHOT_MAX_PIXELS
        );
        std::process::exit(2);
    }

    let pending_crash = drivers_std::crash::pending_path(
        std::path::Path::new(&config.disk_path),
        config.crash_file.as_deref().map(std::path::Path::new),
    );
    let paths_match = |path: &str| {
        let path = std::path::Path::new(path);
        match (pending_crash.canonicalize(), path.canonicalize()) {
            (Ok(pending), Ok(path)) => pending == path,
            _ => std::path::absolute(&pending_crash).ok() == std::path::absolute(path).ok(),
        }
    };
    if paths_match(&config.disk_path) || paths_match(&config.seed_image) {
        eprintln!("The crash file must differ from the disk and seed image paths.");
        std::process::exit(2);
    }
    let _crash_registration = drivers_std::crash::install(&pending_crash).unwrap_or_else(|error| {
        eprintln!("Failed to install crash reporting: {error}");
        None
    });

    // - Initialize the system
    log::initialize(&drivers_std::log::Logger).unwrap();

    // Initialize the task manager

    let users_manager: &'static users::Manager = Box::leak(Box::new(users::Manager::new()));
    let time_manager: &'static time::Manager<'static> = Box::leak(Box::new(
        time::Manager::new(&drivers_std::devices::TimeDevice).unwrap(),
    ));

    let task = task_manager.get_current_task_identifier().await;
    // - Initialize graphics only when using the graphical interface.
    let (graphics_manager, bootsplash): (Option<&'static graphics::Manager>, Option<Bootsplash>) =
        if config.interface == cli::InterfaceMode::Graphical {
            let resolution = graphics::Point::new(config.resolution.0, config.resolution.1);
            let (graphics_manager, event_runner) = match config.display_driver {
                cli::DisplayDriver::Native => {
                    let (screen_device, pointer_device, keyboard_device, runner) =
                        drivers_native::window_screen::new(resolution)
                            .await
                            .unwrap();
                    let (screen_device, pointer_device, keyboard_device) = (
                        Box::leak(Box::new(screen_device)),
                        Box::leak(Box::new(pointer_device)),
                        Box::leak(Box::new(keyboard_device)),
                    );
                    let pointer_device: &'static dyn xila::file_system::DirectCharacterDevice =
                        pointer_device;
                    let keyboard_device: &'static dyn xila::file_system::DirectCharacterDevice =
                        keyboard_device;
                    let input_devices = [
                        (pointer_device, graphics::InputKind::Pointer),
                        (keyboard_device, graphics::InputKind::Keypad),
                    ];
                    let manager = graphics::initialize_with_input_devices(
                        time_manager,
                        screen_device,
                        &input_devices,
                        graphics::get_minimal_buffer_size(&resolution),
                        true,
                    )
                    .await;
                    (manager, Some(runner))
                }
                cli::DisplayDriver::Headless => {
                    let screen_device = Box::leak(Box::new(
                        drivers_headless::HeadlessScreenDevice::new(resolution).unwrap_or_else(
                            |error| {
                                eprintln!("Failed to create headless display: {error}");
                                std::process::exit(2);
                            },
                        ),
                    ));
                    let manager = graphics::initialize_with_input_devices(
                        time_manager,
                        screen_device,
                        &[],
                        graphics::get_minimal_buffer_size(&resolution),
                        true,
                    )
                    .await;
                    (manager, None)
                }
            };
            let graphics_manager: &'static graphics::Manager =
                Box::leak(Box::new(graphics_manager));
            graphics::set_ffi_manager(graphics_manager);

            task_manager
                .spawn(task, "Graphics", None, |_| {
                    graphics_manager.r#loop(task::Manager::sleep)
                })
                .await
                .unwrap();

            if let Some(mut runner) = event_runner {
                task_manager
                    .spawn(task, "Event Loop", None, async move |_| {
                        runner.run().await;
                    })
                    .await
                    .unwrap();
            }

            let bootsplash = Bootsplash::new(graphics_manager).await.unwrap();
            (Some(graphics_manager), Some(bootsplash))
        } else {
            (None, None)
        };

    storage::prepare_disk(
        std::path::Path::new(&config.disk_path),
        std::path::Path::new(&config.seed_image),
        config.disk_size,
    )
    .expect("Failed to import reference disk; existing disks are never replaced");
    let drive = Box::leak(Box::new(
        drivers_std::drive_file::FileDriveDevice::open_existing(std::path::Path::new(
            &config.disk_path,
        ))
        .expect("Failed to open disk"),
    ));
    let file_system = little_fs::mount_disk(drive, little_fs::Options::new(256))
        .expect("Failed to mount existing disk; it was not reformatted");
    use xila::file_system::mbr::{Mbr, PartitionKind};
    let storage_capacity = Mbr::read_from_device(drive)
        .ok()
        .and_then(|table| {
            table
                .partitions
                .into_iter()
                .find(|entry| entry.is_valid() && entry.kind == PartitionKind::Xila)
        })
        .map_or(config.disk_size, |entry| {
            u64::from(entry.block_count) * Mbr::SIZE as u64
        });

    // Initialize the virtual file system
    let virtual_file_system: &'static virtual_file_system::VirtualFileSystem = Box::leak(Box::new(
        virtual_file_system::initialize(task_manager, users_manager, time_manager, file_system)
            .unwrap(),
    ));

    log::information!("Virtual file system initialized.");

    // - - Mount the devices
    virtual_file_system
        .mount_file_system(
            xila::memory_file_system::FileSystem::new(),
            xila::file_system::Path::DEVICES,
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

    #[cfg(unix)]
    if graphics_manager.is_some() {
        virtual_file_system::ignore_already_exists_error(
            virtual_file_system
                .create_directory(task, &"/devices/graphics/debug")
                .await,
        )
        .expect("Failed to create debug device directory");
        virtual_file_system
            .mount_character_device(
                task,
                "/devices/graphics/debug/transport",
                drivers_native::window_screen::DebugTransportDevice,
            )
            .await
            .expect("Failed to mount debug transport");
    }

    #[cfg(feature = "audio")]
    let mut audio_runner = {
        // Keep the mixer pinned for the lifetime of every mounted VFS stream.
        let audio_system: &'static drivers_native::audio::System =
            Box::leak(Box::new(drivers_native::audio::System::new().unwrap()));
        // SAFETY: the leaked service can never move or be freed.
        let audio_system = unsafe { core::pin::Pin::new_unchecked(audio_system) };
        match drivers_native::audio::new(audio_system, 1) {
            Ok(runner) => {
                virtual_file_system
                    .mount_character_device(
                        task,
                        "/devices/audio/playback",
                        audio::AudioStreamDevice::new(audio_system),
                    )
                    .await
                    .expect("Failed to mount audio playback");
                log::information!("Native audio ready");
                Some(runner)
            }
            Err(error) => {
                log::warning!("Audio unavailable; applications can run silently: {error}");
                None
            }
        }
    };

    // System power device: the lifecycle supervisor's final step.
    drivers_peripherals::mount_peripheral(
        virtual_file_system,
        task,
        xila::lifecycle::POWER_DEVICE_PATH,
        drivers_peripherals::power::PowerHandle::new(drivers_std::power::ProcessPower),
    )
    .await
    .expect("Failed to mount the power device");

    // Human interface emulation: on Linux, a uinput controller mounted through the upper
    // half lets applications act as a keyboard, media remote and mouse toward this
    // computer. Without write access to /dev/uinput the device is not mounted and the
    // system still starts. The uinput ioctl request numbers use the generic ioctl layout
    // of the Linux kernel, available on these architectures only.
    #[cfg(all(
        feature = "human-interface",
        target_os = "linux",
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "arm",
            target_arch = "aarch64",
            target_arch = "riscv32",
            target_arch = "riscv64"
        )
    ))]
    match drivers_std::human_interface::UinputController::open() {
        Ok(controller) => {
            drivers_peripherals::human_interface::mount_human_interface(
                virtual_file_system,
                task,
                "emulation",
                controller,
            )
            .await
            .expect("Failed to mount the human interface emulation device");
            log::information!("Human interface emulation ready");
        }
        Err(error) => {
            log::warning!(
                "Human interface emulation unavailable, cannot create the uinput device ({error}); write access to /dev/uinput is required"
            );
        }
    }

    virtual_file_system
        .set_permissions(
            task,
            xila::lifecycle::POWER_DEVICE_PATH,
            xila::file_system::Permissions::from_octal(0o600).unwrap(),
        )
        .await
        .expect("Failed to restrict the power device");

    log::information!("Default hierarchy created.");

    if let Some(graphics) = graphics_manager {
        graphics::appearance::restore_appearance(graphics, virtual_file_system, task).await;
    }

    let network_manager: &'static network::Manager = Box::leak(Box::new(network::initialize(
        task_manager,
        virtual_file_system,
        time_manager,
        &drivers_shared::devices::RandomDevice,
    )));
    let http_client = Box::leak(Box::new(drivers_shared::devices::HttpClientDevice::new(
        network_manager,
        task_manager,
    )));
    let https_client = Box::leak(Box::new(drivers_shared::devices::HttpsClientDevice::new(
        &drivers_shared::devices::RandomDevice,
        network_manager,
        task_manager,
    )));

    mount_static!(
        virtual_file_system,
        task,
        &[
            (
                &"/devices/standard_io/input",
                CharacterDevice,
                drivers_std::console::StandardInDevice
            ),
            (
                &"/devices/standard_io/output",
                CharacterDevice,
                drivers_std::console::StandardOutDevice
            ),
            (
                &"/devices/standard_io/error",
                CharacterDevice,
                drivers_std::console::StandardErrorDevice
            ),
            (
                &"/devices/time/clock",
                CharacterDevice,
                drivers_std::devices::TimeDevice
            ),
            (
                &"/devices/compute/informations",
                CharacterDevice,
                drivers_std::devices::CpuInformationsDevice
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
                *http_client
            ),
            (
                &"/devices/network/https_client",
                CharacterDevice,
                *https_client
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

    if config.networking {
        let (interface_device, controller_device) =
            drivers_std::tuntap::new(&config.interface_name, false, true)
                .expect("Failed to create network interface.");

        network_manager
            .mount_interface(task, "tunnel0", interface_device, controller_device, None)
            .await
            .expect("Failed to mount network interface.");

        let mut file = File::open(
            virtual_file_system,
            task,
            "/devices/network/tunnel0",
            AccessFlags::READ_WRITE.into(),
        )
        .await
        .expect("Failed to open network interface file.");

        for ip_cidr in drivers_std::tuntap::IP_ADDRESSES {
            file.control(ADD_IP_ADDRESS, &network::IpCidrAbi::from(ip_cidr.clone()))
                .await
                .ok();
        }

        for route in drivers_std::tuntap::ROUTES {
            file.control(ADD_ROUTE, &network::RouteAbi::from(route.clone()))
                .await
                .ok();
        }

        for dns_server in drivers_std::tuntap::DEFAULT_DNS_SERVERS {
            file.control(
                ADD_DNS_SERVER,
                &network::IpAddressAbi::from(dns_server.clone()),
            )
            .await
            .ok();
        }

        file.close(virtual_file_system).await.unwrap();
    }

    if config.mock_wifi {
        let (controller, runner) = drivers_native::wifi::MockWifiController::new(
            drivers_native::wifi::MockWifiConfiguration::demo(),
        )
        .expect("Invalid mock Wi-Fi fixture");
        let (packets, _) = network::create_loopback_device();
        network_manager
            .mount_interface(task, "wlan0", packets, controller, None)
            .await
            .expect("Failed to mount mock Wi-Fi");
        task_manager
            .spawn(task, "Mock Wi-Fi", None, move |_| async move {
                runner.run().await
            })
            .await
            .expect("Failed to spawn mock Wi-Fi runner");
    }

    // Mount static executables

    let executable_context: &'static executable::ExecutableContext =
        Box::leak(Box::new(executable::ExecutableContext {
            task_manager,
            users_manager,
            virtual_file_system,
            graphics_manager,
            network_manager: (config.networking || config.mock_wifi).then_some(network_manager),
            formats: native_executables::FORMATS,
            time_manager,
        }));

    mount_executables!(
        executable_context,
        virtual_file_system,
        task,
        &[
            (
                &"/applications/executables/graphical_shell",
                graphical_shell::ShellExecutable
            ),
            (
                &"/applications/executables/file_manager",
                file_manager::FileManagerExecutable
            ),
            (
                &"/applications/executables/command_line_shell",
                command_line_shell::ShellExecutable
            ),
            (
                &"/applications/executables/terminal",
                terminal::TerminalExecutable
            ),
            (
                &"/applications/executables/settings",
                settings::SettingsExecutable
            ),
            (
                &"/applications/executables/activity",
                activity::ActivityExecutable
            ),
            (
                &"/applications/executables/package",
                package::PackageExecutable
            ),
            (&"/applications/executables/c_demo", c_demo::CDemoExecutable),
            #[cfg(unix)]
            (
                &"/applications/executables/debug",
                debug_application::DebugExecutable
            ),
            (&"/applications/executables/wasm", wasm::WasmExecutable),
        ]
    )
    .await
    .unwrap();

    let authentication_context = authentication::Context {
        virtual_file_system,
        task_manager,
        users_manager,
        task,
    };
    authentication::initialize_seeded(&authentication_context)
        .await
        .expect("Failed to initialize saved accounts");
    if let Err(error) =
        drivers_std::crash::import_pending(virtual_file_system, &pending_crash, storage_capacity)
            .await
    {
        log::warning!("Failed to import the pending crash report: {error}");
    }

    // Root and the provisioned administrator group may use the power device,
    // and therefore request a shutdown through the lifecycle subsystem.
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

    if let (Some(bootsplash), Some(graphics_manager)) = (bootsplash, graphics_manager) {
        // Wait briefly to show the bootsplash.
        task::Manager::sleep(Duration::from_secs(2)).await;
        bootsplash.stop(graphics_manager).await.unwrap();
    }

    // Headless sessions have no physical input with which to open the Debug
    // application. Start its subsystem-backed transport before the shell so
    // a host agent can inspect and control the UI immediately after boot.
    if config.interface == cli::InterfaceMode::Graphical
        && config.display_driver == cli::DisplayDriver::Headless
    {
        let debug_standard = Standard::open(
            &"/devices/standard_io/null",
            &"/devices/standard_io/null",
            &"/devices/standard_io/null",
            task,
            virtual_file_system,
            executable_context,
        )
        .await
        .expect("Failed to open standard files for the headless Debug app");
        executable::execute(
            executable_context,
            &"/applications/executables/debug",
            vec!["--background".into()],
            debug_standard,
            None,
        )
        .await
        .expect("Failed to start the headless Debug app");
    }

    // Everything already running is a runtime service; the shell (or the
    // startup application) and what it launches are stopped by the lifecycle
    // pipeline.
    let services = task_manager.get_children(task).await.unwrap();

    // - - A startup application (`/configuration/system/startup.json`)
    // replaces the graphical shell. If it is invalid or cannot start, the
    // graphical shell starts instead.
    let startup_application = match config.interface {
        cli::InterfaceMode::Graphical => {
            match startup_application::load(virtual_file_system, users_manager, task).await {
                Ok(application) => application,
                Err(error) => {
                    log::error!("Startup application: {error}; starting the shell instead");
                    None
                }
            }
        }
        cli::InterfaceMode::Terminal => None,
    };
    if let Some(application) = startup_application {
        use core::sync::atomic::{AtomicBool, Ordering};

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
            .expect("Failed to spawn the application session");
        // The application session runs until a lifecycle request, which ends
        // in `power::finish` like leaving the shell does.
        while !failed.load(Ordering::Acquire) {
            if !xila::lifecycle::SYSTEM.is_running() {
                power::finish(virtual_file_system, time_manager, task, &services, 0).await;
                return;
            }
            #[cfg(feature = "audio")]
            if let Some(runner) = &audio_runner
                && let Err(error) = runner.check()
            {
                log::warning!("Native audio stopped: {error}; restart to reopen the host output");
                audio_runner = None;
            }
            task::Manager::sleep(Duration::from_millis(100)).await;
        }
    }

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

    // - - Execute the shell
    let shell_path = match config.interface {
        cli::InterfaceMode::Graphical => "/applications/executables/graphical_shell",
        cli::InterfaceMode::Terminal => "/applications/executables/command_line_shell",
    };
    let shell =
        match executable::execute(executable_context, shell_path, vec![], standard, None).await {
            Ok(shell) => shell,
            Err(_) if !xila::lifecycle::SYSTEM.is_running() => {
                power::finish(virtual_file_system, time_manager, task, &services, 0).await;
                return;
            }
            Err(error) => panic!("Failed to start shell: {error:?}"),
        };
    let mut join = core::pin::pin!(shell.join());
    // Returning from this task does not stop the native executor: leaving the
    // shell (or a lifecycle request) always ends in `power::finish`.
    let status = loop {
        use core::{
            future::{Future, poll_fn},
            task::Poll,
        };
        if let Poll::Ready(status) =
            poll_fn(|context| Poll::Ready(join.as_mut().poll(context))).await
        {
            break power::shell_status(status);
        }
        if !xila::lifecycle::SYSTEM.is_running() {
            break 0;
        }
        #[cfg(feature = "audio")]
        if let Some(runner) = &audio_runner
            && let Err(error) = runner.check()
        {
            log::warning!("Native audio stopped: {error}; restart to reopen the host output");
            audio_runner = None;
        }
        task::Manager::sleep(Duration::from_millis(100)).await;
    };

    power::finish(virtual_file_system, time_manager, task, &services, status).await;
    #[cfg(feature = "audio")]
    drop(audio_runner);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn main() {
    panic!("This example is only for native platforms.");
}
