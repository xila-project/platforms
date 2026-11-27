//! Route each executor's wake-ups to its own core.

use core::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

use esp_hal::{
    interrupt::software::SoftwareInterrupt,
    peripherals::{CPU_CTRL, FROM_CPU_INTR1, FROM_CPU_INTR2},
    system::{Cpu, CpuControl, Stack},
};
use static_cell::StaticCell;

use super::{Executor, WorkSignal};

static STARTED: AtomicBool = AtomicBool::new(false);
static SECONDARY_WORK: WorkSignal = WorkSignal::new();
static SECONDARY_EXECUTOR: StaticCell<Executor> = StaticCell::new();

// The stack must stay in internal RAM when flash operations disable the
// cache. It is independent of the stack on core 0.
const STACK_SIZE: usize = 24 * 1024;
static mut STACK: MaybeUninit<Stack<STACK_SIZE>> = MaybeUninit::uninit();

/// Start the application executor on core 1. Call from core 0 once at boot.
/// Interrupt 0 is reserved for the radio scheduler.
pub fn start(
    cpu_control: CPU_CTRL<'static>,
    primary_interrupt: FROM_CPU_INTR1<'static>,
    secondary_interrupt: FROM_CPU_INTR2<'static>,
    initialize: impl FnOnce(embassy_executor::Spawner) + Send + 'static,
) {
    assert_eq!(Cpu::current(), Cpu::ProCpu);
    assert!(!STARTED.swap(true, Ordering::Relaxed));
    SoftwareInterrupt::new(primary_interrupt).set_interrupt_handler(wake_primary);

    // SAFETY: only this call can take the stack, and the bootloader has
    // handed over to the application. Stack contains only uninitialized bytes.
    let stack = unsafe { (&mut *&raw mut STACK).assume_init_mut() };
    let guard = CpuControl::new(cpu_control)
        .start_app_core(stack, move || {
            SoftwareInterrupt::new(secondary_interrupt).set_interrupt_handler(wake_secondary);
            SECONDARY_EXECUTOR
                .init(Executor::new(&SECONDARY_WORK))
                .start(initialize);
        })
        .expect("Failed to start the application core");
    // The application executor and its stack live for the whole boot.
    core::mem::forget(guard);
}

pub(super) fn interrupt_if_remote(work: &WorkSignal) {
    if core::ptr::eq(work, &SECONDARY_WORK) {
        if Cpu::current() != Cpu::AppCpu {
            secondary_interrupt().raise();
        }
    } else if Cpu::current() != Cpu::ProCpu {
        primary_interrupt().raise();
    }
}

fn primary_interrupt() -> SoftwareInterrupt<'static, 1> {
    // SAFETY: start owns this peripheral. This copy only raises or acknowledges
    // the interrupt. It does not change its handler or interrupt routing.
    SoftwareInterrupt::new(unsafe { FROM_CPU_INTR1::steal() })
}

fn secondary_interrupt() -> SoftwareInterrupt<'static, 2> {
    // SAFETY: as for primary_interrupt, but for the second executor.
    SoftwareInterrupt::new(unsafe { FROM_CPU_INTR2::steal() })
}

#[esp_hal::handler]
fn wake_primary() {
    primary_interrupt().reset();
}

#[esp_hal::handler]
fn wake_secondary() {
    secondary_interrupt().reset();
}
