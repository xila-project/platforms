//! ESP32-S3 station device and system-owned connection profile.

extern crate alloc;

mod allocation;
mod configuration;
mod controller;
mod device;
mod layout;
mod profile;

pub(crate) use allocation::internal_allocation_required;

use alloc::boxed::Box;

use embassy_sync::once_lock::OnceLock;
use esp_hal::peripherals::{FROM_CPU_INTR0, TIMG0, WIFI};
use esp_radio::wifi::Interface;
use file_system::{AccessFlags, DirectCharacterDevice, IoErrorKind, Result};
use network::{Manager, SET_HARDWARE_ADDRESS};
use virtual_file_system::{File, VirtualFileSystem};

use crate::memory::MemoryManager;

const MINIMUM_FREE_INTERNAL_HEAP: usize = 96 * 1024;
static INTERNAL_HEAP: OnceLock<&'static MemoryManager> = OnceLock::new();

/// Peripherals reserved by the ESP32-S3 board for Wi-Fi.
pub struct Peripherals {
    pub radio: WIFI<'static>,
    pub scheduler_timer: TIMG0<'static>,
    pub scheduler_interrupt: FROM_CPU_INTR0<'static>,
}

/// A board's radio attachment. Emulator boards can exercise the profile and
/// unavailable-radio startup path without accessing nonexistent radio registers.
pub enum Hardware {
    Station(Peripherals),
    Unavailable,
}

/// Mounts the station device and starts the system task that owns the radio.
pub async fn initialize(
    hardware: Hardware,
    heap: &'static MemoryManager,
    task_manager: &'static task::Manager,
    file_system: &'static VirtualFileSystem,
    time_manager: &'static time::Manager<'static>,
    random_device: &'static dyn DirectCharacterDevice,
) -> Result<&'static Manager> {
    let task = task_manager.get_current_task_identifier().await;
    let profile = match configuration::load(file_system, task).await {
        Ok(profile) => {
            log::information!("Wi-Fi: station profile loaded");
            Some(profile)
        }
        Err(IoErrorKind::NotFound) => None,
        Err(error) => {
            log::warning!("Wi-Fi: station profile rejected ({error})");
            None
        }
    };

    let Hardware::Station(peripherals) = hardware else {
        return Err(IoErrorKind::DeviceUnavailable);
    };
    let interface = Interface::try_station().ok_or(IoErrorKind::AlreadyExists)?;
    let address = interface.mac_address();
    let manager = Box::leak(Box::new(network::initialize(
        task_manager,
        file_system,
        time_manager,
        random_device,
    )));
    manager
        .mount_interface(
            task,
            "wlan0",
            device::StationDevice(interface),
            controller::StationControl,
            None,
        )
        .await?;

    let static_address = profile.as_ref().and_then(|profile| profile.address);
    let mut interface = File::open(
        file_system,
        task,
        configuration::INTERFACE_PATH,
        AccessFlags::Write.into(),
    )
    .await?;
    let result = async {
        interface.control(SET_HARDWARE_ADDRESS, &address).await?;
        configuration::apply_address(static_address, &mut interface).await
    }
    .await;
    let closed = interface.close(file_system).await;
    result.and(closed)?;

    allocation::poll_internal(
        task_manager.spawn(task, "Wi-Fi station", None, move |task| {
            controller::run(peripherals, heap, file_system, task, profile)
        }),
    )
    .await
    .map_err(|_| IoErrorKind::InputOutput)?;
    log::information!("Wi-Fi: wlan0 registered");
    Ok(manager)
}
