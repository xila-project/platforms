//! ESP32-S3 station device and optional boot-time connection.

extern crate alloc;

mod allocation;
mod configuration;
mod controller;
mod device;

use alloc::boxed::Box;

use embassy_sync::once_lock::OnceLock;
use esp_hal::{
    peripherals::{FROM_CPU_INTR0, TIMG0, WIFI},
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use file_system::{AccessFlags, DirectCharacterDevice, IoErrorKind, Result};
use network::{CONNECT_WIFI_CLIENT, Manager};
use virtual_file_system::{File, VirtualFileSystem};

use crate::memory::MemoryManager;

// Reserve room for the radio's internal allocations, including those performed by
// esp-rtos while starting. See the WT32-SC01 Plus board notes for the sizing.
const MINIMUM_FREE_INTERNAL_HEAP: usize = 96 * 1024;

static INTERNAL_HEAP: OnceLock<&'static MemoryManager> = OnceLock::new();

/// Peripherals reserved by the ESP32-S3 board for Wi-Fi.
pub struct Peripherals {
    pub radio: WIFI<'static>,
    pub scheduler_timer: TIMG0<'static>,
    pub scheduler_interrupt: FROM_CPU_INTR0<'static>,
}

/// Mounts the radio as `wlan0` and queues station association after reading the local profile.
/// A board without a profile leaves both the radio and its scheduler stopped.
pub async fn initialize(
    peripherals: Peripherals,
    heap: &'static MemoryManager,
    task_manager: &'static task::Manager,
    file_system: &'static VirtualFileSystem,
    time_manager: &'static time::Manager<'static>,
    random_device: &'static dyn DirectCharacterDevice,
) -> Result<Option<&'static Manager>> {
    let task = task_manager.get_current_task_identifier().await;
    let configuration = match configuration::Configuration::load(file_system, task).await {
        Ok(configuration) => configuration,
        Err(IoErrorKind::NotFound) => {
            log::information!("Wi-Fi: no station configuration; radio disabled");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    let free_before = allocation::free_bytes(heap);
    if free_before < MINIMUM_FREE_INTERNAL_HEAP {
        return Err(IoErrorKind::OutOfMemory);
    }
    INTERNAL_HEAP
        .init(heap)
        .map_err(|_| IoErrorKind::AlreadyExists)?;
    let scheduler_timer = TimerGroup::new(peripherals.scheduler_timer);
    // The existing executor stays on the first core as the scheduler's main thread.
    // esp-rtos runs the radio's preemptive tasks using TIMG0 and software interrupt 0.
    esp_rtos::start(scheduler_timer.timer0, peripherals.scheduler_interrupt);
    let radio = WifiController::new(
        peripherals.radio,
        ControllerConfig::default()
            .with_static_rx_buf_num(6)
            .with_dynamic_rx_buf_num(12)
            .with_dynamic_tx_buf_num(8),
    )
    .map_err(|_| IoErrorKind::DeviceUnavailable)?;
    let interface = Interface::try_station().ok_or(IoErrorKind::AlreadyExists)?;

    let manager = Box::leak(Box::new(network::initialize(
        task_manager,
        file_system,
        time_manager,
        random_device,
    )));
    let control = controller::StationControl(interface.mac_address());
    task_manager
        .spawn(task, "Wi-Fi station", None, move |_| controller::run(radio))
        .await
        .map_err(|_| IoErrorKind::InputOutput)?;
    manager
        .mount_interface(
            task,
            "wlan0",
            device::StationDevice(interface),
            control,
            None,
        )
        .await?;

    let mut interface = File::open(
        file_system,
        task,
        "/devices/network/wlan0",
        AccessFlags::Write.into(),
    )
    .await?;
    let result = async {
        configuration.apply_address(&mut interface).await?;
        interface
            .control(CONNECT_WIFI_CLIENT, &configuration.station)
            .await
    }
    .await;
    let closed = interface.close(file_system).await;
    result.and(closed)?;

    let free_after = allocation::free_bytes(heap);
    log::information!(
        "Wi-Fi: wlan0 ready; internal heap free before startup {free_before}, after startup {free_after} bytes"
    );
    Ok(Some(manager))
}
