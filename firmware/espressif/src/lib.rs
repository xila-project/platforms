//! The Espressif firmware shared by every board.
//!
//! A board crate initializes its chip, builds a [`Board`] description and calls
//! [`run`]. The firmware then starts Xila: the console, the memory report, the
//! subsystems, LittleFS on the board's storage partition, the devices, the
//! accounts, the executables and the sessions, and finally the lifecycle
//! supervisor. Everything that differs between boards comes from the
//! description, so this crate has no chip or board branches.
//!
//! The board crate selects the chip: it enables exactly one chip feature of
//! esp-hal, `xila-drivers-espressif`, esp-storage and the bootloader crate.
//! It also provides the panic handler and the application descriptor.
#![no_std]
#![cfg(all(
    target_os = "none",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]

extern crate alloc;

#[cfg(feature = "graphics")]
mod applications;
mod board;
mod flash;
#[cfg(feature = "graphics")]
pub mod graphics;
#[cfg(feature = "native-dev")]
pub mod native;
pub mod session;

use alloc::boxed::Box;
use core::{cell::RefCell, mem::MaybeUninit, ptr::addr_of_mut, time::Duration};

use critical_section::Mutex;
use drivers_espressif::{console, memory::MemoryManager, power::BoardPower, time::TimeDevice};
use xila::{
    authentication,
    executable::{self, mount_executables},
    file_system::{AccessFlags, CreateFlags, Flags, Path},
    little_fs, log,
    memory::ManagerTrait,
    memory_file_system, task, time, users,
    virtual_file_system::{self, AccessTimePolicy, File, MountOptions, mount_static},
};

pub use board::{Board, Display, ExternalMemory, MemoryReport, StoragePartition, parse_decimal};
pub use drivers_espressif;

/// The static internal heap reservation.
pub const HEAP_SIZE: usize = 256 * 1024;
static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

/// LittleFS cache: a divisor of the 4096-byte flash page.
const CACHE_SIZE: usize = 256;

/// When the firmware was built, in seconds since the Unix epoch.
const BUILD_EPOCH: u64 = parse_decimal(env!("XILA_BUILD_EPOCH"));

/// The command-line shell, which every board mounts.
pub const SHELL_PATH: &str = "/system/executables/command_line_shell";

/// The graphical shell, mounted when the firmware has the `graphics` feature.
pub const GRAPHICAL_SHELL_PATH: &str = "/system/executables/graphical_shell";

/// How long the startup task waits for the boot report to reach the console
/// before it starts the sessions: at most 50 checks, 2 seconds in all.
const BOOT_REPORT_DRAIN_ATTEMPTS: usize = 50;
const BOOT_REPORT_DRAIN_INTERVAL: Duration = Duration::from_millis(40);

/// How often the startup task checks for a lifecycle request.
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

static TASK_MANAGER: task::Manager = task::Manager::new();

drivers_espressif::instantiate_global_allocator!();

static BOARD: Mutex<RefCell<Option<Board>>> = Mutex::new(RefCell::new(None));

/// Hands the static internal heap reservation to the allocator and returns the
/// allocator, for the board to add its PSRAM. Call it first, once.
pub fn initialize_heap() -> &'static MemoryManager {
    let heap = &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR;
    // SAFETY: the reservation is static SRAM that only the allocator uses;
    // `init_internal` fails when called a second time.
    assert!(
        unsafe { heap.init_internal(addr_of_mut!(HEAP).cast(), HEAP_SIZE) },
        "The heap was already initialized"
    );
    heap
}

/// The allocator behind the global heap.
pub fn heap() -> &'static MemoryManager {
    &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR
}

/// Starts Xila on `board`. Call it from the board's entry point, after
/// [`initialize_heap`] and the time driver.
pub fn run(board: Board) -> ! {
    critical_section::with(|section| {
        let previous = BOARD.borrow(section).borrow_mut().replace(board);
        assert!(previous.is_none(), "The firmware was already started");
    });
    start();
    unreachable!()
}

#[xila::task::run(
    task_path = xila::task,
    executor = drivers_espressif::executor::Executor::take(),
    manager = &TASK_MANAGER
)]
async fn start(task_manager: &'static task::Manager) {
    let board = critical_section::with(|section| BOARD.borrow(section).borrow_mut().take())
        .expect("The board was already taken");
    let Board {
        name,
        console: console_port,
        flash_storage,
        storage,
        random_device,
        has_entropy_source,
        check_random_device,
        memory,
        memory_check,
        system_check,
        display,
        sessions,
    } = board;

    let task = task_manager.get_current_task_identifier().await;

    // - Console: serial input, output and logging over the board's port.
    log::initialize(&console::LOGGER).expect("Failed to initialize logger");
    let console_runner = console::ConsoleRunner::new(console_port);
    task_manager
        .spawn(task, "Serial console", None, async move |_| {
            console_runner.run().await
        })
        .await
        .expect("Failed to spawn the serial console");

    log::information!("Xila ESP32 startup");
    log::information!("Xila: board {name}");
    #[cfg(feature = "native-dev")]
    native::initialize();
    if let Some(external) = memory.external {
        log::information!(
            "Xila: PSRAM {} bytes (registered: {})",
            external.size,
            external.registered
        );
        if external.size == 0 {
            log::warning!("Xila: no PSRAM detected; the heap uses internal SRAM only");
        }
    }
    if let Some(check) = memory_check {
        check(&memory);
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
        flash::LittleFsPartition::new(flash_storage, storage),
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
    let random_device = Box::leak(Box::new(random_device));
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
    if check_random_device {
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
        assert_ne!(first, second, "RNG returned a constant value");
        log::information!("Random generator works");
        task::yield_now().await;
    }

    // - Accounts. A file system this start just formatted is a first
    // installation: only then may the default account be created.
    if formatted && has_entropy_source {
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
    if has_entropy_source {
        authentication::initialize_seeded(&authentication_context).await
    } else {
        log::warning!(
            "Default account creation is disabled because no entropy source is available"
        );
        authentication::load_all_users_and_groups(&authentication_context).await
    }
    .expect("Failed to load the accounts");

    // - Graphics, on boards with a screen.
    #[cfg(feature = "graphics")]
    let graphics_manager = match display {
        Some(display) => {
            let manager = graphics::start(task_manager, task, time_manager, display).await;
            xila::graphics::appearance::restore_appearance(manager, virtual_file_system, task)
                .await;
            Some(manager)
        }
        None => None,
    };
    #[cfg(not(feature = "graphics"))]
    if display.is_some() {
        log::warning!("Xila: the board has a screen, but the firmware has no graphics");
    }

    // - Executables.
    let executable_context: &'static executable::ExecutableContext =
        Box::leak(Box::new(executable::ExecutableContext {
            #[cfg(feature = "native-dev")]
            formats: native::FORMATS,
            #[cfg(feature = "graphics")]
            graphics_manager,
            ..executable::ExecutableContext::new(
                task_manager,
                users_manager,
                virtual_file_system,
                time_manager,
            )
        }));
    // The shells, in memory: nothing from an earlier boot is there.
    #[cfg(not(feature = "graphics"))]
    let mounted = mount_executables!(
        executable_context,
        virtual_file_system,
        task,
        &[(&SHELL_PATH, command_line_shell::ShellExecutable)]
    )
    .await;
    #[cfg(feature = "graphics")]
    let mounted = mount_executables!(
        executable_context,
        virtual_file_system,
        task,
        &[
            (&SHELL_PATH, command_line_shell::ShellExecutable),
            (&GRAPHICAL_SHELL_PATH, graphical_shell::ShellExecutable),
        ]
    )
    .await;
    if let Err(error) = mounted {
        log::error!("Xila: cannot mount the shells: {error}");
    }
    #[cfg(feature = "graphics")]
    applications::mount(executable_context, virtual_file_system, task).await;

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

    if let Some(check) = system_check {
        check();
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
    if memory.external.is_some() {
        let statistics = heap.get_statistics();
        // Region 0 is the static internal heap reservation, region 1 the
        // external PSRAM.
        for (index, region) in statistics.region_stats.iter().enumerate() {
            if let Some(region) = region {
                if index == 0 {
                    log::information!(
                        "Xila heap region 0 (internal heap reservation of {HEAP_SIZE} bytes, \
                         out of the chip's {} bytes of SRAM): \
                         {} bytes used, {} free, {} total",
                        memory.internal_sram_size,
                        region.used,
                        region.free,
                        region.size
                    );
                } else {
                    log::information!(
                        "Xila heap region {index} (external PSRAM): \
                         {} bytes used, {} free, {} total",
                        region.used,
                        region.free,
                        region.size
                    );
                }
            }
        }
    }
    task::yield_now().await;

    // - Sessions. Everything already running is a runtime service; the
    // sessions and what they launch are stopped by the lifecycle pipeline.
    let services = task_manager.get_children(task).await.unwrap_or_default();
    // Let the boot report reach the console before the login prompt, unless
    // nobody reads the port yet (a USB host that has not opened it).
    for _ in 0..BOOT_REPORT_DRAIN_ATTEMPTS {
        if console::is_output_idle() {
            break;
        }
        task::Manager::sleep(BOOT_REPORT_DRAIN_INTERVAL).await;
    }
    // From now on a person may be typing on the console: keep the log quiet.
    console::set_interactive(true);
    for session in sessions {
        session::spawn(task_manager, task, executable_context, session).await;
    }
    while xila::lifecycle::SYSTEM.is_running() {
        task::Manager::sleep(LIFECYCLE_POLL_INTERVAL).await;
    }
    console::set_interactive(false);

    // A privileged `reboot` was requested: stop, commit, then reset.
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
