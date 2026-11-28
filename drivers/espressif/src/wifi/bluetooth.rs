//! Bluetooth Low Energy on the ESP32-S3 station's shared radio.

use alloc::boxed::Box;

use bluetooth::{BluetoothDevice, DEVICE_PATH, hci::controller::ExternalController};
use esp_hal::peripherals::BT;
use esp_radio::ble::{Config, controller::BleConnector};
use file_system::{IoErrorKind, Result};
use task::{Manager, TaskIdentifier};
use virtual_file_system::VirtualFileSystem;

use super::allocation;
use crate::memory::MemoryManager;

pub(super) async fn start(
    peripheral: BT<'static>,
    heap: &MemoryManager,
    file_system: &'static VirtualFileSystem,
    task_manager: &'static Manager,
    task: TaskIdentifier,
) -> Result<()> {
    let before = allocation::free_bytes(heap);
    let connector = allocation::internal(|| {
        BleConnector::new(peripheral, Config::default().with_max_connections(1))
    })
    .map_err(|_| IoErrorKind::InputOutput)?;
    let after = allocation::free_bytes(heap);
    log::information!(
        "Bluetooth: internal heap free before controller startup {before}, after startup {after} bytes"
    );

    // Both crate versions use the same packet transport trait.
    let controller = ExternalController::<_, 1>::new(connector);
    let device: &'static BluetoothDevice = Box::leak(Box::new(BluetoothDevice::new()));
    bluetooth::mount(file_system, task, device).await?;
    task_manager
        .spawn(task, "Bluetooth host", None, move |_| async move {
            if let Err(error) = allocation::poll_internal(device.run(controller)).await {
                log::warning!("Bluetooth: host stopped ({error})");
            }
        })
        .await
        .map_err(|_| IoErrorKind::InputOutput)?;

    log::information!("Bluetooth: {DEVICE_PATH} mounted");
    Ok(())
}
