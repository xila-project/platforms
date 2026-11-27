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

#[cfg(any(feature = "graphics", feature = "wasm", feature = "native-linked"))]
mod applications;
#[cfg(feature = "display")]
pub mod backlight;
mod board;
mod flash;
#[cfg(feature = "display")]
pub mod graphics;
#[cfg(feature = "native-dev")]
pub mod native;
#[cfg(feature = "display")]
pub mod panel;
pub mod psram_self_test;
mod sd_card;
pub mod session;

use alloc::boxed::Box;
use core::{cell::RefCell, mem::MaybeUninit, ptr::addr_of_mut, time::Duration};

use critical_section::Mutex;
use drivers_espressif::{
    console,
    memory::{MemoryManager, PSRAM_REGION},
    power::BoardPower,
    time::TimeDevice,
};
use xila::{
    authentication,
    executable::{self, mount_executables},
    file_system::{AccessFlags, CreateFlags, Flags, Path},
    little_fs, log, memory_file_system, task, time, users,
    virtual_file_system::{self, AccessTimePolicy, File, MountOptions, mount_static},
};

#[cfg(feature = "display")]
pub use board::AsynchronousFlush;
pub use board::{Board, Display, ExternalMemory, MemoryReport, StoragePartition, parse_decimal};
pub use drivers_espressif;
pub use sd_card::{OpenedSdCard, SdCardSlot};

/// The static internal heap reservation.
#[cfg(not(feature = "wifi"))]
pub const HEAP_SIZE: usize = 256 * 1024;
// The radio and scheduler add internal code and data. Leave room for the
// executor's main stack in the shared instruction/data SRAM bank.
#[cfg(feature = "wifi")]
pub const HEAP_SIZE: usize = 188 * 1024;
static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

/// LittleFS cache: a divisor of the 4096-byte flash page.
const CACHE_SIZE: usize = 256;

/// When the firmware was built, in seconds since the Unix epoch. The minimum
/// timestamp a real-time clock reading must reach to seed UTC, and the fallback
/// epoch itself when no clock is available. Board crates use it to floor their
/// RTC lower half.
pub const BUILD_EPOCH: u64 = parse_decimal(env!("XILA_BUILD_EPOCH"));

/// The build date as a UTC timestamp: the initial clock value at boot, the
/// floor for real-time clock readings that may seed UTC, and the floor for
/// answers from the network.
fn build_date() -> time::Timestamp {
    time::Timestamp::from(Duration::from_secs(BUILD_EPOCH))
}

/// The command-line shell, which every board mounts.
pub const SHELL_PATH: &str = "/system/executables/command_line_shell";

/// The graphical shell, mounted when the firmware has the `graphics` feature.
pub const GRAPHICAL_SHELL_PATH: &str = "/system/executables/graphical_shell";

/// The WebAssembly runtime, mounted when the firmware has the `wasm` feature.
pub const WASM_PATH: &str = "/applications/executables/wasm";

/// How long the startup task waits for the boot report to reach the console
/// before it starts the sessions: at most 50 checks, 2 seconds in all.
const BOOT_REPORT_DRAIN_ATTEMPTS: usize = 50;
const BOOT_REPORT_DRAIN_INTERVAL: Duration = Duration::from_millis(40);

/// How often the startup task checks for a lifecycle request.
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(not(feature = "dual-core"))]
static TASK_MANAGER: task::Manager = task::Manager::new();

// Task headers contain the run and timer queue links. Keep them in internal
// RAM even when the heap uses PSRAM for the boxed futures and other data.
#[cfg(feature = "dual-core")]
static TASK_SLOTS: [task::TaskSlot; 64] = [const { task::TaskSlot::new() }; 64];
#[cfg(feature = "dual-core")]
static TASK_MANAGER: task::Manager = task::Manager::with_static_slots(&TASK_SLOTS);

#[cfg(feature = "dual-core")]
static APPLICATION_EXECUTOR: embassy_sync::signal::Signal<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    task::SpawnerIdentifier,
> = embassy_sync::signal::Signal::new();

/// Starts the secondary executor. Call once, before running the firmware.
#[cfg(feature = "dual-core")]
pub fn start_application_executor(
    cpu_control: esp_hal::peripherals::CPU_CTRL<'static>,
    primary_interrupt: esp_hal::peripherals::FROM_CPU_INTR1<'static>,
    secondary_interrupt: esp_hal::peripherals::FROM_CPU_INTR2<'static>,
) {
    drivers_espressif::executor::dual_core::start(
        cpu_control,
        primary_interrupt,
        secondary_interrupt,
        |spawner| {
            let identifier = TASK_MANAGER
                .register_spawner(spawner)
                .expect("Failed to register the application executor");
            APPLICATION_EXECUTOR.signal(identifier);
        },
    );
}

#[cfg(not(feature = "wifi"))]
drivers_espressif::instantiate_global_allocator!();
// Prefer PSRAM for ordinary allocations when present to reserve internal
// memory for the radio and other allocations with hardware constraints.
#[cfg(feature = "wifi")]
static __DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR: MemoryManager =
    MemoryManager::with_large_allocation_threshold(Some(0));
#[cfg(feature = "wifi")]
drivers_espressif::instantiate_global_allocator!(&__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR);

static BOARD: Mutex<RefCell<Option<Board>>> = Mutex::new(RefCell::new(None));

/// Hands the static internal heap reservation to the allocator and returns the
/// allocator, for the board to add its PSRAM. Call it first, once.
pub fn initialize_heap() -> &'static MemoryManager {
    console::paint_main_stack();
    let heap = heap();
    // SAFETY: the reservation is static SRAM that only the allocator uses;
    // `init_internal` fails when called a second time.
    assert!(
        unsafe { heap.init_internal(addr_of_mut!(HEAP).cast(), HEAP_SIZE) },
        "The heap was already initialized"
    );
    heap
}

/// Uses a board-provided internal SRAM reservation in place of the default.
///
/// # Safety
///
/// The reservation must be in DMA-capable internal data SRAM that remains
/// accessible during flash operations. Call once, before starting firmware.
pub unsafe fn initialize_heap_from_slice(
    reservation: &'static mut [MaybeUninit<u8>],
) -> &'static MemoryManager {
    console::paint_main_stack();
    let heap = heap();
    assert!(
        unsafe { heap.init_internal_from_slice(reservation) },
        "Could not initialize the internal heap"
    );
    heap
}

/// The allocator behind the global heap.
pub fn heap() -> &'static MemoryManager {
    &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR
}

#[cfg(any(feature = "native-dev", feature = "native-linked"))]
unsafe fn allocate_stack(layout: core::alloc::Layout) -> *mut u8 {
    use xila::memory::ManagerTrait as _;

    // SAFETY: `heap()` is initialized before `run()`. Requesting
    // `CapabilityFlags::AlwaysAccessible` guarantees internal SRAM (never
    // PSRAM), keeping coroutine stacks valid and accessible across flash
    // operations and interrupts.
    unsafe { heap().allocate(xila::memory::CapabilityFlags::AlwaysAccessible, layout) }
        .map_or(core::ptr::null_mut(), core::ptr::NonNull::as_ptr)
}

#[cfg(any(feature = "native-dev", feature = "native-linked"))]
unsafe fn deallocate_stack(ptr: *mut u8, layout: core::alloc::Layout) {
    use xila::memory::ManagerTrait as _;

    if let Some(ptr) = core::ptr::NonNull::new(ptr) {
        // SAFETY: `ptr` was allocated by `allocate_stack` from `heap()` with
        // the exact matching `layout`.
        unsafe { heap().deallocate(ptr, layout) };
    }
}

#[cfg(feature = "wifi")]
static WIFI: Mutex<RefCell<Option<drivers_espressif::wifi::Hardware>>> =
    Mutex::new(RefCell::new(None));

/// Starts Xila and makes the board's optional ESP32-S3 radio available after mounting storage.
#[cfg(feature = "wifi")]
pub fn run_with_wifi(board: Board, radio: drivers_espressif::wifi::Hardware) -> ! {
    critical_section::with(|section| WIFI.borrow(section).replace(Some(radio)));
    run(board)
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

/// How long the bootsplash stays on screen at least, so its animation is seen.
#[cfg(feature = "graphics")]
const BOOTSPLASH_MINIMUM_DURATION: Duration = Duration::from_secs(1);

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
        sd_card,
        display,
        gpio,
        real_time_clock,
        #[cfg(feature = "usb-human-interface")]
        human_interface,
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
    #[cfg(feature = "dual-core")]
    {
        let executor = APPLICATION_EXECUTOR.wait().await;
        let (probe, _) = task_manager
            .spawn_on_executor(task, "Application executor check", executor, |_| async {
                // Check initial dispatch and a timer wake from core 0.
                assert_eq!(
                    esp_hal::system::Cpu::current(),
                    esp_hal::system::Cpu::AppCpu
                );
                task::sleep(Duration::from_millis(1)).await;
                assert_eq!(
                    esp_hal::system::Cpu::current(),
                    esp_hal::system::Cpu::AppCpu
                );
            })
            .await
            .expect("Failed to dispatch to the application executor");
        probe.join().await;
        log::information!("Xila: application executor ready on core 1");
    }

    #[cfg(any(feature = "native-dev", feature = "native-linked"))]
    {
        // SAFETY: called once at start-up before any stackful coroutine is active;
        // `Cpu::current()` returns 0 (`ProCpu`) or 1 (`AppCpu`), both `< MAX_CORES`.
        unsafe {
            xila::executable::stackful::set_core_id_provider(|| {
                esp_hal::system::Cpu::current() as usize
            });
        }
        // SAFETY: called once at start-up before any stackful coroutine is active;
        // `heap()` is initialized before `run()`, `allocate_stack` allocates
        // exclusive `CapabilityFlags::AlwaysAccessible` internal SRAM (never PSRAM,
        // safe across flash operations and interrupts), and `deallocate_stack`
        // releases blocks back to `heap()` with the exact matching layout.
        unsafe {
            xila::executable::stackful::set_stack_allocator(
                xila::executable::stackful::StackAllocator::new(allocate_stack, deallocate_stack),
            );
        }
    }
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
    // from the build date until something sets a better one. The build date is
    // its own source, so the write-back task never stores it in the RTC.
    let time_manager: &'static time::Manager<'static> =
        Box::leak(Box::new(time::Manager::unsynchronized()));
    time_manager
        .set_realtime(time::RealtimeSample {
            timestamp: build_date(),
            source: time::ClockSource::BuildDate,
            resolution: Duration::from_secs(1),
            accuracy: None,
        })
        .expect("Invalid build date");
    // Hardware clock to system, like Linux `hctosys`: when the RTC holds a valid
    // time not earlier than the build date, it seeds UTC; otherwise the
    // build-date fallback stays.
    if let Some(real_time_clock) = real_time_clock {
        match time::real_time_clock::synchronize_from_real_time_clock(
            time_manager,
            real_time_clock,
            build_date(),
        ) {
            Ok(true) => log::information!("Xila: UTC initialized from the real-time clock"),
            Ok(false) => {
                log::information!(
                    "Xila: the real-time clock holds no valid time; using the build date"
                )
            }
            Err(error) => {
                log::warning!("Xila: real-time clock read failed ({error}); using the build date")
            }
        }
    }

    // - Root file system, in internal flash.
    let drive: &'static flash::LittleFsPartition = Box::leak(Box::new(
        flash::LittleFsPartition::new(flash_storage, storage),
    ));
    let little_file_system_options = little_fs::Options::new(CACHE_SIZE).set_block_cycles(500);
    let (file_system, formatted) =
        match little_fs::FileSystem::new(drive, little_file_system_options) {
            Ok(file_system) => (file_system, false),
            Err(error) => {
                log::warning!("Xila: LittleFS mount failed ({error:?}); formatting storage");
                (
                    little_fs::FileSystem::new_format(drive, little_file_system_options)
                        .expect("Failed to format and mount LittleFS"),
                    true,
                )
            }
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

    #[cfg(feature = "neural-network")]
    {
        virtual_file_system::ignore_already_exists_error(
            virtual_file_system
                .create_directory(task, &"/devices/nn")
                .await,
        )
        .expect("Failed to create neural network device directory");
        // Allocations of 4 KiB or more prefer PSRAM, including the model buffers and tensor arena.
        drivers_shared::devices::neural_network::mount_loader(
            virtual_file_system,
            task,
            "/devices/nn/cpu",
            drivers_shared::devices::neural_network::Target::Cpu,
            256 * 1024,
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

    drivers_peripherals::mount_peripheral(
        virtual_file_system,
        task,
        xila::lifecycle::POWER_DEVICE_PATH,
        drivers_peripherals::power::PowerHandle::new(BoardPower),
    )
    .await
    .expect("Failed to mount the power device");

    // - Real-time clock: the device node, and the write-back task that stores
    // UTC in the clock when it is known to be good (system to hardware clock,
    // like Linux `systohc`). The task runs outside the clock manager, because
    // an external chip blocks on its I2C bus.
    if let Some(real_time_clock) = real_time_clock {
        drivers_peripherals::real_time_clock::mount_real_time_clock(
            virtual_file_system,
            task,
            real_time_clock,
        )
        .await
        .expect("Failed to mount the real-time clock device");
        task_manager
            .spawn(task, "Real-time clock write-back", None, async move |_| {
                time::real_time_clock::write_back_to_real_time_clock(time_manager, real_time_clock)
                    .await
            })
            .await
            .expect("Failed to spawn the real-time clock write-back task");
        log::information!("Xila: real-time clock mounted at /devices/time/real_time_clock");
    }

    // - microSD card, on boards with a slot.
    if let Some(slot) = sd_card {
        sd_card::start(slot, virtual_file_system, task, time_manager).await;
        task::yield_now().await;
    }

    if let Some(gpio) = gpio {
        drivers_espressif::shared::mount_pin_devices(virtual_file_system, task, gpio)
            .await
            .expect("Failed to mount the GPIO devices");
    }

    // - Human interface emulation, on boards that provide a controller (the
    // composite USB device of the `usb-human-interface` feature).
    #[cfg(feature = "usb-human-interface")]
    if let Some(controller) = human_interface {
        drivers_peripherals::human_interface::mount_human_interface(
            virtual_file_system,
            task,
            "emulation",
            controller,
        )
        .await
        .expect("Failed to mount the human interface emulation device");
        log::information!("Xila: human interface emulation mounted");
    }

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
    let (graphics_manager, bootsplash) = match display {
        Some(display) => {
            let manager = graphics::start(task_manager, task, time_manager, display).await;
            xila::graphics::appearance::restore_appearance(manager, virtual_file_system, task)
                .await;
            let bootsplash = match xila::bootsplash::Bootsplash::new(manager).await {
                Ok(bootsplash) => Some((bootsplash, time_manager.monotonic_now())),
                Err(error) => {
                    log::warning!("Xila: cannot start the bootsplash: {error:?}");
                    None
                }
            };
            (Some(manager), bootsplash)
        }
        None => (None, None),
    };
    #[cfg(all(feature = "display", not(feature = "graphics")))]
    let graphics_manager = if let Some(display) = display {
        let manager = graphics::start(task_manager, task, time_manager, display).await;
        graphics::show_touch_test(manager).await;
        Some(manager)
    } else {
        None
    };
    #[cfg(not(feature = "display"))]
    if display.is_some() {
        log::warning!("Xila: the board has a screen, but the firmware has no graphics");
    }
    #[cfg(feature = "wifi")]
    let network_manager = {
        let peripherals = critical_section::with(|section| WIFI.borrow(section).take());
        match peripherals {
            Some(peripherals) => match drivers_espressif::wifi::initialize(
                peripherals,
                heap(),
                task_manager,
                virtual_file_system,
                time_manager,
                random_device,
            )
            .await
            {
                Ok(manager) => Some(manager),
                Err(error) => {
                    log::warning!("Wi-Fi: startup failed ({error})");
                    None
                }
            },
            None => None,
        }
    };

    #[cfg(feature = "wifi")]
    if let Some(manager) = network_manager {
        let http_client = Box::leak(Box::new(drivers_shared::devices::HttpClientDevice::new(
            manager,
            task_manager,
        )));
        let https_client = Box::leak(Box::new(drivers_shared::devices::HttpsClientDevice::new(
            random_device,
            manager,
            task_manager,
        )));
        // The PSRAM-first global allocator also serves the TLS record buffers.
        mount_static!(
            virtual_file_system,
            task,
            &[
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
            ]
        )
        .await
        .expect("Failed to mount HTTP client devices");

        // Set UTC from the network once Wi-Fi is up. The build date stays the
        // initial value until the first successful synchronization, and is
        // also the floor: answers before it are rejected.
        if let Err(error) = xila::network::sntp::Service::new(
            task_manager,
            manager,
            time_manager,
            virtual_file_system,
            build_date(),
        )
        .start(task)
        .await
        {
            log::warning!("Xila: cannot start the SNTP service ({error:?})");
        }
    }

    // - Executables.
    let executable_context: &'static executable::ExecutableContext =
        Box::leak(Box::new(executable::ExecutableContext {
            #[cfg(feature = "native-dev")]
            formats: native::FORMATS,
            #[cfg(feature = "display")]
            graphics_manager,
            #[cfg(feature = "wifi")]
            network_manager,
            ..executable::ExecutableContext::new(
                task_manager,
                users_manager,
                virtual_file_system,
                time_manager,
            )
        }));
    // The shells, in memory: nothing from an earlier boot is there. The
    // graphical shell is mounted even when a startup application replaces it:
    // the mount is one small in-memory entry, its code is in flash either way,
    // and it must be there if the application cannot start. What a startup
    // application saves is the shell's running state (login, desk, dock).
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
    #[cfg(any(feature = "graphics", feature = "wasm", feature = "native-linked"))]
    applications::mount(executable_context, virtual_file_system, task).await;

    task_manager
        .set_environment_variable(
            task,
            "Paths",
            "/system/executables:/applications/executables",
        )
        .await
        .expect("Failed to set the environment");
    task_manager
        .set_environment_variable(task, "Host", "xila")
        .await
        .expect("Failed to set the environment");

    // - - Time zone: the stored setting becomes `TZ` in this task and in the
    // services already started, so every task started from here inherits it.
    // A missing or invalid setting gives UTC with one warning.
    xila::local_time::initialize(task_manager, virtual_file_system, task).await;

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

    let statistics = heap().get_statistics();
    let internal_budget: usize = statistics
        .region_stats
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != PSRAM_REGION)
        .filter_map(|(_, region)| region.as_ref().map(|region| region.size))
        .sum();
    log::information!(
        "Xila heap: {} bytes used, {} free, {} total ({}-byte budget)",
        statistics.current_usage,
        statistics.size - statistics.current_usage,
        statistics.size,
        internal_budget
    );
    if memory.external.is_some() || statistics.region_stats.iter().flatten().count() > 1 {
        for (index, region) in statistics.region_stats.iter().enumerate() {
            if let Some(region) = region {
                if index == PSRAM_REGION {
                    log::information!(
                        "Xila heap region {index} (external PSRAM): {} bytes used, {} free, {} total",
                        region.used,
                        region.free,
                        region.size
                    );
                } else {
                    log::information!(
                        "Xila heap region {index} (internal SRAM): {} bytes used, {} free, {} total \
                         (of {} bytes of chip SRAM)",
                        region.used,
                        region.free,
                        region.size,
                        memory.internal_sram_size,
                    );
                }
            }
        }
    }
    task::yield_now().await;

    // - Startup application: one application in place of the graphical
    // shell, when `/configuration/system/startup.json` names one. Any problem
    // is reported here, while the console still logs, and the board's usual
    // sessions start instead.
    let startup_application =
        match startup_application::load(virtual_file_system, users_manager, task).await {
            Ok(None) => None,
            Ok(Some(application)) => {
                log::information!(
                    "Xila: startup application {}",
                    startup_application::describe(&application)
                );
                let application: &'static startup_application::Application =
                    Box::leak(Box::new(application));
                Some(application)
            }
            Err(error) => {
                log::error!("Xila: startup application: {error}; starting the shell instead");
                None
            }
        };

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
    #[cfg(feature = "graphics")]
    if let (Some(manager), Some((bootsplash, started))) = (graphics_manager, bootsplash) {
        // Keep the animation on screen for at least a second: only wait for
        // what the rest of the boot has not already taken.
        let shown = time_manager.monotonic_now().saturating_sub(started);
        if let Some(remaining) = BOOTSPLASH_MINIMUM_DURATION.checked_sub(shown) {
            task::Manager::sleep(remaining).await;
        }
        if let Err(error) = bootsplash.stop(manager).await {
            log::warning!("Xila: cannot stop the bootsplash: {error:?}");
        }
    }
    // From now on a person may be typing on the console: keep the log quiet.
    console::set_interactive(true);
    match startup_application {
        None => {
            for session in sessions {
                session::spawn(task_manager, task, executable_context, session).await;
            }
        }
        Some(application) => {
            let plan = startup_application::plan(sessions, session::Session::is_graphical);
            for session in plan.kept {
                session::spawn(task_manager, task, executable_context, session).await;
            }
            session::spawn_application(
                task_manager,
                task,
                executable_context,
                application,
                plan.replaced,
            )
            .await;
        }
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
