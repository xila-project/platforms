//! The start of the system.

extern crate alloc;

use alloc::{boxed::Box, format, vec};
use core::time::Duration;

use drivers_nordic::{
    flash::InternalFlashDevice,
    random::RandomDevice,
    usb_console::{self, UsbConsole, UsbConsoleConfig},
};
use xila::{
    authentication,
    executable::{self, Standard, mount_executables},
    file_system::{AccessFlags, CreateFlags, Flags},
    little_fs, log,
    memory::ManagerTrait,
    task, time, users,
    virtual_file_system::{self, File, mount_static},
};

use crate::board;

/// LittleFS cache: a divisor of the 4096-byte flash page.
const CACHE_SIZE: usize = 256;

/// When the firmware was built, in seconds since the Unix epoch.
const BUILD_EPOCH: u64 = {
    let digits = env!("XILA_BUILD_EPOCH").as_bytes();
    let mut value = 0;
    let mut index = 0;
    while index < digits.len() {
        value = value * 10 + (digits[index] - b'0') as u64;
        index += 1;
    }
    value
};

const SHELL_PATH: &str = "/applications/executables/command_line_shell";

static TASK_MANAGER: task::Manager = task::Manager::new();

drivers_nordic::memory::instantiate_global_allocator!();

#[cortex_m_rt::entry]
fn entry() -> ! {
    // The state a reset leaves, whatever the bootloader left.
    board::quiesce();
    board::stop_clocks();

    let (start, size) = board::heap_region();
    // SAFETY: the linker reserves this RAM for the heap, and nothing has
    // allocated yet.
    unsafe { __DRIVERS_NORDIC_MEMORY_ALLOCATOR.init(start, size) };

    run();
    unreachable!()
}

#[xila::task::run(
    task_path = xila::task,
    executor = drivers_nordic::executor::instantiate_static_executor!(),
    manager = &TASK_MANAGER
)]
async fn run(task_manager: &'static task::Manager) {
    let mut configuration = embassy_nrf::config::Config::default();
    // USB needs the crystal; `init` waits for it.
    configuration.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    // The board has no 32.768 kHz crystal.
    configuration.lfclk_source = embassy_nrf::config::LfclkSource::InternalRC;
    // Leave the debug port as it is: the default would write `UICR.APPROTECT`.
    configuration.debug = embassy_nrf::config::Debug::NotConfigured;
    let peripherals = embassy_nrf::init(configuration);

    #[cfg(feature = "bring-up")]
    board::start_bootloader_fallback();

    let task = task_manager.get_current_task_identifier().await;

    // - Console: the only link to the outside.
    let serial_number: &'static str =
        Box::leak(format!("{:016X}", board::device_identifier()).into_boxed_str());
    let console = UsbConsole::new(
        peripherals.USBD,
        UsbConsoleConfig {
            // The test identifiers of pid.codes.
            vendor_id: 0x1209,
            product_id: 0x0001,
            manufacturer: Some("Xila"),
            product: Some("Xila on Arduino Nano 33 BLE"),
            serial_number: Some(serial_number),
            on_1200_bps_touch: Some(board::enter_bootloader),
        },
    )
    .expect("USB console already taken");
    log::initialize(&usb_console::LOGGER).expect("logger already set");
    let runner = console.runner;
    task_manager
        .spawn(task, "USB console", None, async move |_| runner.run().await)
        .await
        .expect("Failed to spawn the USB console");

    #[cfg(feature = "bring-up")]
    {
        while !usb_console::host_seen() {
            task::Manager::sleep(Duration::from_millis(50)).await;
        }
        // The time for the terminal to settle before the first line.
        task::Manager::sleep(Duration::from_millis(200)).await;
    }

    log::information!("Xila on Arduino Nano 33 BLE");
    let mut crash_message = [0; 248];
    if let Some(message) = board::take_crash_message(&mut crash_message) {
        log::error!("The previous run crashed: {message}");
    }
    // Let the console send the lines above before the start goes on: what
    // follows may crash before it yields.
    #[cfg(feature = "bring-up")]
    task::Manager::sleep(Duration::from_millis(500)).await;

    // - Subsystems.
    let users_manager: &'static users::Manager = Box::leak(Box::new(users::Manager::new()));
    // No clock survives a power cycle, and the file system needs a date: start
    // from the build date until something sets a better one.
    let time_manager: &'static time::Manager<'static> =
        Box::leak(Box::new(time::Manager::unsynchronized()));
    time_manager
        .set_realtime(time::RealtimeSample {
            timestamp: time::Timestamp::from(Duration::from_secs(BUILD_EPOCH)),
            source: time::ClockSource::Manual,
            resolution: Duration::from_secs(1),
            accuracy: None,
        })
        .expect("Invalid build date");

    // - Root file system, in the internal flash.
    let (storage_start, storage_size) = board::storage_region();
    let drive: &'static InternalFlashDevice = Box::leak(Box::new(
        InternalFlashDevice::new(peripherals.NVMC, storage_start, storage_size)
            .expect("Invalid storage region"),
    ));
    let (file_system, formatted) = match little_fs::FileSystem::new(drive, CACHE_SIZE) {
        Ok(file_system) => (file_system, false),
        Err(error) => {
            log::warning!("No file system in the internal flash ({error:?}): formatting it");
            (
                little_fs::FileSystem::new_format(drive, CACHE_SIZE)
                    .expect("Failed to format the internal flash"),
                true,
            )
        }
    };

    let virtual_file_system: &'static virtual_file_system::VirtualFileSystem = Box::leak(Box::new(
        virtual_file_system::initialize(task_manager, users_manager, time_manager, file_system)
            .expect("Failed to initialize the virtual file system"),
    ));
    xila::abi_definitions::initialize(xila::abi_definitions::RuntimeContext {
        virtual_file_system,
        time_manager,
    });
    virtual_file_system::create_default_hierarchy(virtual_file_system, task)
        .await
        .expect("Failed to create the default hierarchy");
    virtual_file_system::clear_temporary(virtual_file_system, task)
        .await
        .expect("Failed to clear temporary storage");

    let terminal: &'static drivers_shared::devices::TerminalInputDevice<
        usb_console::StandardInDevice,
        usb_console::StandardOutDevice,
    > = Box::leak(Box::new(drivers_shared::devices::TerminalInputDevice::new(
        usb_console::StandardInDevice,
        usb_console::StandardOutDevice,
    )));
    let random: &'static RandomDevice = Box::leak(Box::new(RandomDevice::new(peripherals.RNG)));
    mount_static!(
        virtual_file_system,
        task,
        &[
            (&"/devices/standard_io/input", CharacterDevice, *terminal),
            (
                &"/devices/standard_io/output",
                CharacterDevice,
                usb_console::StandardOutDevice
            ),
            (
                &"/devices/standard_io/error",
                CharacterDevice,
                usb_console::StandardOutDevice
            ),
            (
                &"/devices/standard_io/null",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (
                &"/devices/time/clock",
                CharacterDevice,
                drivers_nordic::time::TimeDevice
            ),
            (&"/devices/random/source", CharacterDevice, *random),
            (
                &"/devices/compute/hasher",
                CharacterDevice,
                drivers_shared::devices::HashDevice
            ),
        ]
    )
    .await
    .expect("Failed to mount the devices");
    // A previous start left the node on flash; its handle is gone.
    let _ = virtual_file_system
        .remove(task, xila::lifecycle::POWER_DEVICE_PATH)
        .await;
    drivers_peripherals::mount_peripheral(
        virtual_file_system,
        task,
        xila::lifecycle::POWER_DEVICE_PATH,
        drivers_peripherals::power::PowerHandle::new(board::BoardPower),
    )
    .await
    .expect("Failed to mount the power device");
    crate::pins::mount_gpio_pins(virtual_file_system, task)
        .await
        .expect("Failed to mount GPIO pins");

    // - Accounts. A file system this start just formatted is a first
    // installation: only then may the default account be created.
    if formatted {
        let mut marker = File::open(
            virtual_file_system,
            task,
            authentication::SEED_PENDING_PATH,
            Flags::new(AccessFlags::Write, Some(CreateFlags::CREATE_TRUNCATE), None),
        )
        .await
        .expect("Failed to mark the first installation");
        marker
            .write(authentication::SEED_PENDING_CONTENT)
            .await
            .expect("Failed to mark the first installation");
        marker
            .close(virtual_file_system)
            .await
            .expect("Failed to mark the first installation");
    }
    authentication::initialize_seeded(&authentication::Context {
        virtual_file_system,
        task_manager,
        users_manager,
        task,
    })
    .await
    .expect("Failed to load the accounts");

    // - Executables.
    let executable_context: &'static executable::ExecutableContext =
        Box::leak(Box::new(executable::ExecutableContext::new(
            task_manager,
            users_manager,
            virtual_file_system,
            time_manager,
        )));
    mount_executables!(
        executable_context,
        virtual_file_system,
        task,
        &[(&SHELL_PATH, command_line_shell::ShellExecutable)]
    )
    .await
    .expect("Failed to mount the executables");

    task_manager
        .set_environment_variable(task, "Paths", "/")
        .await
        .expect("Failed to set the environment");
    task_manager
        .set_environment_variable(task, "Host", "xila")
        .await
        .expect("Failed to set the environment");

    // Root and the administrator group may use the power device, and
    // therefore request a restart through the lifecycle subsystem.
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

    log::information!(
        "File system ready (formatted: {formatted}); heap: {} bytes used, {} free",
        __DRIVERS_NORDIC_MEMORY_ALLOCATOR.get_used(),
        __DRIVERS_NORDIC_MEMORY_ALLOCATOR.get_free()
    );

    // A start that got this far is not a crash loop.
    board::mark_stable();

    // - The shell, again whenever a session ends, until a lifecycle request.
    let services = task_manager.get_children(task).await.unwrap_or_default();
    while xila::lifecycle::SYSTEM.is_running() {
        let standard = Standard::open(
            &"/devices/standard_io/input",
            &"/devices/standard_io/output",
            &"/devices/standard_io/error",
            task,
            virtual_file_system,
            executable_context,
        )
        .await
        .expect("Failed to open the console");
        let shell = executable::execute(executable_context, SHELL_PATH, vec![], standard, None)
            .await
            .expect("Failed to start the shell");
        let status = shell.join().await;
        log::information!("The shell ended with status {status}");
    }

    // A privileged `reboot` ended the session: stop, commit, then reset.
    let supervisor = xila::lifecycle::Supervisor {
        virtual_file_system,
        time_manager,
        task,
        services: &services,
        budget: Duration::from_secs(5),
    };
    if let Err(error) = supervisor.run(&xila::lifecycle::SYSTEM, &mut ()).await {
        log::error!("Shutdown failed: {error}; resources are retained");
    }
}
