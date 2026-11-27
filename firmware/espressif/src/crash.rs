//! Connect ESP32-S3 crash retention to boot and the Rust panic handler.

use alloc::boxed::Box;
use core::{ops::Range, panic::PanicInfo};

use drivers_espressif::crash::{self as retention, Registers};
use esp_hal::system::{Cpu, software_reset};
use xila::{
    crash::{
        Architecture, BuildIdentifier, CRASH_LOOP_LIMIT, Header, Limits, RecordKind, ResetReason,
        SECONDARY_CORE_UNHALTED, Writer, complete_reset_reason, invalidate,
    },
    log, task,
};

pub struct Metadata {
    pub target: &'static str,
    pub firmware_version: &'static str,
    pub image_identifier: &'static [u8; 32],
}

impl Metadata {
    fn header(&self, kind: RecordKind) -> Header<'static> {
        // SAFETY: The application descriptor is readable flash. The image
        // builder can replace the linked zeroes before it writes the image.
        let identifier = unsafe { core::ptr::read_volatile(self.image_identifier) };
        let build_identifier = if identifier == [0; 32] {
            BuildIdentifier::None
        } else {
            BuildIdentifier::ElfSha256(identifier)
        };
        Header {
            target_name: self.target,
            firmware_version: self.firmware_version,
            build_epoch: crate::BUILD_EPOCH,
            build_identifier,
            ..Header::new(kind, Architecture::XtensaEsp32S3)
        }
    }
}

/// Copy retention after the hardware and heap start, before mounting storage.
pub fn early_boot(metadata: Metadata) -> Option<Box<[u8]>> {
    task::active_task::set_core_identifier_provider(|| Cpu::current() as usize);
    let mut retention = retention::try_claim_at_boot()?;
    let (reason, raw) = retention::current_reset_reason();
    let slot = retention.slot();
    if let Ok(length) = complete_reset_reason(slot, reason, raw) {
        let copy = Box::from(&slot[..length]);
        invalidate(slot.first_chunk_mut().unwrap());
        return Some(copy);
    }
    invalidate(slot.first_chunk_mut().unwrap());

    let (kind, message) = match reason {
        ResetReason::Watchdog => (RecordKind::WatchdogReset, "Watchdog reset"),
        ResetReason::BrownOut => (RecordKind::AbnormalHardwareReset, "Brownout reset"),
        _ => return None,
    };
    let consecutive_crashes = retention.guard().record_crash();
    let header = Header {
        reset_reason: reason,
        raw_reset_reason: raw,
        consecutive_crashes,
        ..metadata.header(kind)
    };
    let slot = retention.slot();
    let record = Writer::new(slot, &header, Limits::ESPRESSIF)
        .unwrap()
        .finish(format_args!("{message}"), &[], |_| Some(0));
    let copy = Box::from(record);
    invalidate(slot.first_chunk_mut().unwrap());
    Some(copy)
}

/// Clear the crash-loop guard when the sessions have started and uptime is
/// at least ten seconds.
pub fn mark_stable() -> bool {
    if retention::uptime_microseconds() < 10_000_000 {
        return false;
    }
    let Some(mut retention) = retention::try_claim_at_boot() else {
        return false;
    };
    if retention.guard().consecutive_crashes() != 0 {
        retention.guard().mark_stable();
    }
    true
}

#[cold]
#[inline(never)]
pub fn panic(information: &PanicInfo<'_>, metadata: Metadata, summary: impl FnOnce()) -> ! {
    let mut retention = retention::claim_for_panic();
    let registers = Registers::capture();
    let core = Cpu::current() as usize;
    let bounds = stack_bounds(core, registers.stack_pointer);
    let active_task = task::active_task::snapshot();
    let count = retention.guard().record_crash();
    let header = Header {
        program_counter: registers.link_register.saturating_sub(3) as u64,
        link_register: registers.link_register as u64,
        stack_pointer: registers.stack_pointer as u64,
        stack_base: if bounds.is_empty() {
            0
        } else {
            registers.stack_pointer as u64
        },
        core_identifier: core as u32,
        task_identifier: active_task.map_or(0, |task| task.identifier),
        task_name: active_task
            .as_ref()
            .map_or("", task::active_task::ActiveTask::name),
        uptime_microseconds: retention::uptime_microseconds(),
        consecutive_crashes: count,
        flags: SECONDARY_CORE_UNHALTED,
        ..metadata.header(RecordKind::KernelPanic)
    };
    let slot = retention.slot();
    let mut writer = Writer::new(slot, &header, Limits::ESPRESSIF).unwrap();
    let mut frame_pointer = registers.stack_pointer;
    // SAFETY: A main or coroutine stack range is used only after validating
    // the captured stack pointer against that range.
    while let Some((address, parent)) = unsafe { retention::caller(frame_pointer, &bounds) } {
        if !writer.push_frame(address as u64) {
            break;
        }
        frame_pointer = parent;
    }
    let stack_length = bounds.end.saturating_sub(registers.stack_pointer);
    writer.finish_with_stack(
        format_args!("{information}"),
        stack_length,
        // SAFETY: The writer clamps the length and never reads above bounds.end.
        |output| unsafe { retention::copy_stack(registers.stack_pointer, output) },
        |output| Some(log::snapshot_tail(output)),
    );
    summary();
    if count < CRASH_LOOP_LIMIT {
        software_reset();
    }
    loop {
        core::hint::spin_loop();
    }
}

fn stack_bounds(core: usize, pointer: usize) -> Range<usize> {
    #[cfg(any(feature = "native-dev", feature = "native-linked"))]
    if let Some(bounds) = xila::executable::stackful::current_stack_bounds(core)
        && (bounds.start..bounds.end).contains(&pointer)
    {
        return bounds.start..bounds.end;
    }
    let _ = core;
    let bounds = retention::main_stack_bounds();
    if bounds.contains(&pointer) {
        bounds
    } else {
        0..0
    }
}
