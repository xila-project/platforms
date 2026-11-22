extern crate alloc;

use alloc::{boxed::Box, vec};
use core::{cell::RefCell, mem::MaybeUninit, panic::PanicInfo, ptr::addr_of_mut, time::Duration};

use critical_section::Mutex;
use drivers_espressif::{
    console::{self, ConsoleRunner},
    power::BoardPower,
    random::RandomDevice,
    time::TimeDevice,
};
use esp_hal::{
    Blocking,
    timer::systimer::SystemTimer,
    uart::{Config as UartConfiguration, Uart},
};
use esp_storage::FlashStorage;
use xila::{
    authentication,
    executable::{self, Standard, mount_executables},
    file_system::{AccessFlags, CreateFlags, Flags, Path},
    little_fs, log,
    memory::ManagerTrait,
    memory_file_system, task, time, users,
    virtual_file_system::{self, AccessTimePolicy, File, MountOptions, mount_static},
};

#[cfg(feature = "qemu-display")]
mod display;
mod flash;

#[cfg(not(any(feature = "esp32s3", feature = "esp32c6", feature = "esp32p4")))]
compile_error!("Select one target chip feature: esp32s3, esp32c6 or esp32p4.");

const HEAP_SIZE: usize = 256 * 1024;
static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

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

const SHELL_PATH: &str = "/system/executables/command_line_shell";
const HAS_ENTROPY_SOURCE: bool = !cfg!(any(feature = "esp32p4", feature = "qemu-esp32c6"));

static TASK_MANAGER: task::Manager = task::Manager::new();

drivers_espressif::instantiate_global_allocator!();
esp_bootloader_esp_idf::esp_app_desc!();

struct StartupHardware {
    serial: Uart<'static, Blocking>,
    flash_storage: FlashStorage<'static>,
    random_device: RandomDevice,
    #[cfg(feature = "esp32p4")]
    external_memory: (usize, bool),
}

static STARTUP_HARDWARE: Mutex<RefCell<Option<StartupHardware>>> = Mutex::new(RefCell::new(None));

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let heap = &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR;
    // SAFETY: startup hands this static SRAM exclusively to the allocator.
    assert!(unsafe { heap.init_internal(addr_of_mut!(HEAP).cast(), HEAP_SIZE) });

    #[cfg(feature = "esp32p4")]
    let external_memory = {
        let mapped_memory =
            esp_hal::psram::Psram::new(peripherals.PSRAM, esp_hal::psram::PsramConfig::default());
        let (_, size) = mapped_memory.raw_parts();
        // SAFETY: The detected PSRAM is reserved exclusively for this allocator.
        let registered = unsafe { heap.init_psram_from_hal(&mapped_memory) };
        (size, registered)
    };

    let system_timer = SystemTimer::new(peripherals.SYSTIMER);
    drivers_espressif::time::initialize(system_timer.alarm0);

    #[cfg(feature = "esp32s3")]
    let (transmit_pin, receive_pin) = (peripherals.GPIO43, peripherals.GPIO44);
    #[cfg(feature = "esp32c6")]
    let (transmit_pin, receive_pin) = (peripherals.GPIO16, peripherals.GPIO17);
    #[cfg(feature = "esp32p4")]
    let (transmit_pin, receive_pin) = (peripherals.GPIO37, peripherals.GPIO38);
    let serial = Uart::new(peripherals.UART0, UartConfiguration::default())
        .expect("Failed to configure UART0")
        .with_tx(transmit_pin)
        .with_rx(receive_pin);

    let flash_storage = FlashStorage::new(peripherals.FLASH);

    #[cfg(not(any(feature = "esp32p4", feature = "qemu-esp32c6")))]
    let random_device = RandomDevice::new(peripherals.RNG, peripherals.ADC1);
    #[cfg(any(feature = "esp32p4", feature = "qemu-esp32c6"))]
    let random_device = RandomDevice::from_entropy_source(|_| {
        // esp-hal lacks ESP32-P4 entropy support, and the pinned ESP32-C6
        // emulator lacks the random register.
        Err(xila::file_system::IoErrorKind::DeviceUnavailable)
    });

    critical_section::with(|section| {
        *STARTUP_HARDWARE.borrow(section).borrow_mut() = Some(StartupHardware {
            serial,
            flash_storage,
            random_device,
            #[cfg(feature = "esp32p4")]
            external_memory,
        });
    });

    run();
    unreachable!()
}

#[xila::task::run(
    task_path = xila::task,
    executor = drivers_espressif::executor::Executor::take(),
    manager = &TASK_MANAGER
)]
async fn run(task_manager: &'static task::Manager) {
    let hardware =
        critical_section::with(|section| STARTUP_HARDWARE.borrow(section).borrow_mut().take())
            .expect("Startup hardware was already taken");

    let task = task_manager.get_current_task_identifier().await;

    // - Console: serial input, output and logging over UART0.
    log::initialize(&console::LOGGER).expect("Failed to initialize logger");
    let console_runner = ConsoleRunner::new(hardware.serial);
    task_manager
        .spawn(task, "Serial console", None, async move |_| {
            console_runner.run().await
        })
        .await
        .expect("Failed to spawn the serial console");

    log::information!("Xila ESP32 startup");
    #[cfg(feature = "esp32p4")]
    {
        let (size, registered) = hardware.external_memory;
        log::information!("Xila: PSRAM {size} bytes (registered: {registered})");
    }
    task::yield_now().await;

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

    // - Root file system, in internal flash.
    let drive: &'static flash::LittleFsPartition = Box::leak(Box::new(
        flash::LittleFsPartition::new(hardware.flash_storage),
    ));
    let little_file_system_options = little_fs::Options::new(CACHE_SIZE).set_block_cycles(500);
    // Keep existing data: format only when the partition holds no valid file
    // system (a fresh, erased image in the emulator, or a new board).
    let (file_system, formatted) =
        match little_fs::FileSystem::new(drive, little_file_system_options) {
            Ok(file_system) => (file_system, false),
            Err(_) => (
                little_fs::FileSystem::new_format(drive, little_file_system_options)
                    .expect("Failed to format and mount LittleFS"),
                true,
            ),
        };
    log::information!("Xila: LittleFS mounted");
    task::yield_now().await;

    let virtual_file_system: &'static virtual_file_system::VirtualFileSystem = Box::leak(Box::new(
        virtual_file_system::initialize_with_options(
            task_manager,
            users_manager,
            time_manager,
            file_system,
            MountOptions::new().with_access_time_policy(AccessTimePolicy::Never),
        )
        .expect("Failed to initialize the virtual file system"),
    ));
    virtual_file_system
        .mount_file_system(memory_file_system::FileSystem::new(), Path::DEVICES, task)
        .await
        .expect("Failed to mount the in-memory device file system");
    virtual_file_system::create_default_hierarchy(virtual_file_system, task)
        .await
        .expect("Failed to create the default hierarchy");
    virtual_file_system
        .mount_file_system(
            memory_file_system::FileSystem::new(),
            Path::SYSTEM_EXECUTABLES,
            task,
        )
        .await
        .expect("Failed to mount the in-memory executable file system");
    virtual_file_system::clear_temporary(virtual_file_system, task)
        .await
        .expect("Failed to clear temporary storage");

    let terminal_input = Box::leak(Box::new(console::standard_input()));
    let terminal_output = Box::leak(Box::new(console::standard_output()));
    let random_device = Box::leak(Box::new(hardware.random_device));
    mount_static!(
        virtual_file_system,
        task,
        &[
            (
                &"/devices/standard_io/input",
                CharacterDevice,
                *terminal_input
            ),
            (
                &"/devices/standard_io/output",
                CharacterDevice,
                *terminal_output
            ),
            (
                &"/devices/standard_io/error",
                CharacterDevice,
                *terminal_output
            ),
            (
                &"/devices/standard_io/null",
                CharacterDevice,
                drivers_core::NullDevice
            ),
            (&"/devices/time/clock", CharacterDevice, TimeDevice),
            (&"/devices/random/source", CharacterDevice, *random_device),
            (
                &"/devices/compute/hasher",
                CharacterDevice,
                drivers_shared::devices::HashDevice
            ),
        ]
    )
    .await
    .expect("Failed to mount the devices");
    drivers_peripherals::mount_peripheral(
        virtual_file_system,
        task,
        xila::lifecycle::POWER_DEVICE_PATH,
        drivers_peripherals::power::PowerHandle::new(BoardPower),
    )
    .await
    .expect("Failed to mount the power device");

    // The ESP32-S3 smoke test checks that the device returns changing values.
    #[cfg(feature = "esp32s3")]
    {
        let mut sample = [0u8; 8];
        File::read_slice_from_path(
            virtual_file_system,
            task,
            "/devices/random/source",
            &mut sample,
        )
        .await
        .expect("Failed to read from the random device");
        let first = u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
        let second = u32::from_le_bytes([sample[4], sample[5], sample[6], sample[7]]);
        log::information!("Xila: RNG {first:08x} {second:08x}");
        assert_ne!(first, second, "RNG returned a constant value");
        task::yield_now().await;
    }

    // - Accounts. A file system this start just formatted is a first
    // installation: only then may the default account be created.
    if formatted && HAS_ENTROPY_SOURCE {
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
    let authentication_context = authentication::Context {
        virtual_file_system,
        task_manager,
        users_manager,
        task,
    };
    if HAS_ENTROPY_SOURCE {
        authentication::initialize_seeded(&authentication_context).await
    } else {
        log::warning!(
            "Default account creation is disabled because no entropy source is available"
        );
        authentication::load_all_users_and_groups(&authentication_context).await
    }
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
        .set_environment_variable(task, "Paths", "/system/executables:/")
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

    #[cfg(feature = "qemu-display")]
    {
        display::render();
        task::yield_now().await;
    }

    let heap = &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR;
    log::information!(
        "Xila heap: {} bytes used, {} free, {} total ({}-byte budget)",
        heap.get_used(),
        heap.get_free(),
        heap.get_total_size(),
        HEAP_SIZE
    );
    task::yield_now().await;

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

#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    esp_println::println!("Xila panic: {information}");
    loop {
        core::hint::spin_loop();
    }
}
