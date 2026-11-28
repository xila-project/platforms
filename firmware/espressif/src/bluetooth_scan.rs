//! Smoke test for the mounted Bluetooth Low Energy device.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::num::NonZeroUsize;

use embassy_time::{Duration, with_timeout};
use xila::{
    bluetooth::{DEVICE_PATH, Event, START_SCAN, STOP_SCAN, ScanConfiguration, ScanResult},
    executable::{ExecutableTrait, MainFuture, Standard},
    file_system::{AccessFlags, ControlCommand, IoErrorKind, Result},
    virtual_file_system::File,
};

const SCAN_DURATION: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct BluetoothScan;

impl ExecutableTrait for BluetoothScan {
    fn main(mut standard: Standard<'static>, _: Vec<String>) -> MainFuture {
        Box::pin(async move {
            if let Err(error) = scan(&mut standard).await {
                let message = format!("Bluetooth scan failed: {error}\n");
                let _ = write_text(standard.error(), &message).await;
                return Err(NonZeroUsize::MIN);
            }
            Ok(())
        })
    }
}

async fn scan(standard: &mut Standard<'_>) -> Result<()> {
    let context = standard.context;
    let task = context.task_manager.get_current_task_identifier().await;
    let file_system = context.virtual_file_system;
    let mut device = File::open(
        file_system,
        task,
        DEVICE_PATH,
        AccessFlags::READ_WRITE.into(),
    )
    .await?;
    let result = scan_device(&mut device, standard.out()).await;
    let closed = device.close(file_system).await;
    result.and(closed)
}

async fn scan_device(device: &mut File<'_>, output: &mut File<'_>) -> Result<()> {
    device
        .control(
            START_SCAN,
            &ScanConfiguration {
                active: 0,
                filter_duplicates: 1,
            },
        )
        .await?;
    wait_for_completion(device, output, START_SCAN::IDENTIFIER.get_number()).await?;

    let reports = with_timeout(SCAN_DURATION, async {
        loop {
            report_event(output, read_event(device).await?).await?;
        }
    })
    .await;

    device.control(STOP_SCAN, &()).await?;
    wait_for_completion(device, output, STOP_SCAN::IDENTIFIER.get_number()).await?;
    if let Ok(result) = reports {
        let result: Result<()> = result;
        result?;
    }
    write_text(output, "Bluetooth scan complete\n").await
}

async fn read_event(device: &mut File<'_>) -> Result<Event> {
    let mut event = Event::default();
    if device.read(event.as_mutable_bytes()).await? != size_of::<Event>() {
        return Err(IoErrorKind::InvalidData);
    }
    Ok(event)
}

async fn wait_for_completion(
    device: &mut File<'_>,
    output: &mut File<'_>,
    command: u8,
) -> Result<()> {
    with_timeout(COMMAND_TIMEOUT, async {
        loop {
            let event = read_event(device).await?;
            if event.kind == Event::COMPLETION && event.command == command {
                return event.completion();
            }
            report_event(output, event).await?;
        }
    })
    .await
    .map_err(|_| IoErrorKind::TimedOut)?
}

async fn report_event(output: &mut File<'_>, event: Event) -> Result<()> {
    if let Some(report) = event.payload::<ScanResult>(Event::SCAN_RESULT) {
        let [a, b, c, d, e, f] = report.peer.bytes;
        let address_type = report.peer.kind;
        let rssi = report.rssi;
        let line = format!(
            "{f:02X}:{e:02X}:{d:02X}:{c:02X}:{b:02X}:{a:02X} type {address_type}, {rssi} dBm\n"
        );
        write_text(output, &line).await?;
    } else if let Some(count) = event.payload::<u32>(Event::OVERFLOW) {
        write_text(output, &format!("Bluetooth events dropped: {count}\n")).await?;
    }
    Ok(())
}

async fn write_text(output: &mut File<'_>, text: &str) -> Result<()> {
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        let count = output.write(bytes).await?;
        if count == 0 {
            return Err(IoErrorKind::WriteZero);
        }
        bytes = &bytes[count..];
    }
    Ok(())
}
