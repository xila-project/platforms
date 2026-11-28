//! Connect ESP32-S3 crash retention to boot and the Rust panic handler.

use alloc::boxed::Box;
use core::{ops::Range, panic::PanicInfo};

use drivers_espressif::crash::{self as retention, Registers};
use esp_hal::system::{Cpu, software_reset};
use xila::{
    crash::{
        Architecture, BuildIdentifier, CRASH_LOOP_LIMIT, ESPRESSIF_RECORD_CAPACITY, Header, Limits,
        RawWriter, RecordKind, ResetReason, SECONDARY_CORE_UNHALTED, Writer, complete_reset_reason,
        invalidate,
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
/// The retained slot stays valid until `release_retained` runs after the
/// report is durable in storage, so a reset in between cannot lose it.
pub fn early_boot(metadata: Metadata) -> Option<Box<[u8]>> {
    task::active_task::set_core_identifier_provider(|| Cpu::current() as usize);
    let mut retention = retention::try_claim_at_boot()?;
    let (reason, raw) = retention::current_reset_reason();
    let slot = retention.slot()?;
    if let Ok(length) = complete_reset_reason(slot, reason, raw) {
        return Some(Box::from(&slot[..length]));
    }

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
    let slot = retention.slot()?;
    let record = Writer::new(slot, &header, Limits::ESPRESSIF)
        .unwrap()
        .finish(format_args!("{message}"), &[], |_| Some(0));
    Some(Box::from(record))
}

/// Invalidate the retained slot once `xila::crash::import` reported the
/// report durable in storage, so the next boot does not read it again.
pub fn release_retained() {
    if let Some(mut retention) = retention::try_claim_at_boot()
        && let Some(slot) = retention.slot()
    {
        invalidate(slot.first_chunk_mut().unwrap());
    }
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
    let count = retention.record_crash();
    // The secondary core only counts as unhalted when it really started (the
    // Wi-Fi RTOS is what starts it) and is not the panicking core itself.
    let flags = if core == 0 && retention::secondary_core_started() {
        SECONDARY_CORE_UNHALTED
    } else {
        0
    };
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
        flags,
        ..metadata.header(RecordKind::KernelPanic)
    };
    // Write the record directly into the retained slot in both cases: the
    // panic stack budget is 512 bytes (xila-project/core#900), so the record
    // is never staged on the stack. The owned path uses the exclusive slot
    // reference; the unowned path uses the raw writer, which touches the slot
    // only with volatile reads and writes through the raw cursor.
    if retention.owns_claim() {
        if let Some(slot) = retention.slot() {
            write_record(slot, &header, &registers, &bounds, information);
        }
    } else {
        // SAFETY: The claim is held by the other core, which may hold a live
        // `&mut` to the slot from `slot()`: no Rust reference to the slot is
        // created without the claim, because two live `&mut` references to the
        // same memory would be undefined behavior. The raw writer writes
        // every field with volatile stores through the raw cursor, computes
        // the checksum over a volatile read-back, and commits the magic last
        // between fences. The concurrent-write window is bounded (the claim
        // holder resets the chip right after committing), and a record torn by
        // the concurrent write fails the checksum on the next boot.
        let slot = retention.slot_raw();
        write_record_raw(slot, &header, &registers, &bounds, information);
    }
    summary();
    if count < CRASH_LOOP_LIMIT {
        software_reset();
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Write one panic record into the exclusively owned slot.
fn write_record(
    slot: &mut [u8],
    header: &Header<'_>,
    registers: &Registers,
    bounds: &Range<usize>,
    information: &PanicInfo<'_>,
) {
    let mut writer = Writer::new(slot, header, Limits::ESPRESSIF).unwrap();
    write_frames(&mut writer, registers, bounds);
    let stack_length = bounds.end.saturating_sub(registers.stack_pointer);
    writer.finish_with_stack(
        format_args!("{information}"),
        stack_length,
        // SAFETY: The writer clamps the length and never reads above bounds.end.
        |output| unsafe { retention::copy_stack(registers.stack_pointer, output) },
        |output| Some(log::snapshot_tail(output)),
    );
}

/// Write one panic record into the retained slot through the raw writer,
/// without creating any Rust reference to the slot.
fn write_record_raw(
    slot: *mut [u8; ESPRESSIF_RECORD_CAPACITY],
    header: &Header<'_>,
    registers: &Registers,
    bounds: &Range<usize>,
    information: &PanicInfo<'_>,
) {
    // SAFETY: The slot is the retained record slot; the panic path without the
    // claim guarantees that no reference to it is created here, so the raw
    // writer's volatile accesses are the only ones in flight.
    let mut writer = unsafe {
        RawWriter::new(
            slot.cast::<u8>(),
            ESPRESSIF_RECORD_CAPACITY,
            header,
            Limits::ESPRESSIF,
        )
    }
    .unwrap();
    write_frames(&mut writer, registers, bounds);
    let stack_length = bounds.end.saturating_sub(registers.stack_pointer);
    writer.finish_with_stack(
        format_args!("{information}"),
        stack_length,
        // SAFETY: The writer clamps the length and never reads above bounds.end.
        |output, length| unsafe {
            retention::copy_stack_raw(registers.stack_pointer, output, length)
        },
        |output, length| {
            // SAFETY: The window is into the retained slot and is valid for
            // `length` bytes; no reference to it is created.
            Some(unsafe { log::snapshot_tail_raw(output, length) })
        },
    );
}

trait FrameSink {
    fn push_frame(&mut self, address: u64) -> bool;
}

impl FrameSink for Writer<'_> {
    fn push_frame(&mut self, address: u64) -> bool {
        Writer::push_frame(self, address)
    }
}

impl FrameSink for RawWriter {
    fn push_frame(&mut self, address: u64) -> bool {
        RawWriter::push_frame(self, address)
    }
}

/// Walk the captured frames into the record writer.
fn write_frames(sink: &mut impl FrameSink, registers: &Registers, bounds: &Range<usize>) {
    let mut frame_pointer = registers.stack_pointer;
    // SAFETY: A main or coroutine stack range is used only after validating
    // the captured stack pointer against that range.
    while let Some((address, parent)) = unsafe { retention::caller(frame_pointer, bounds) } {
        if !sink.push_frame(address as u64) {
            break;
        }
        frame_pointer = parent;
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
