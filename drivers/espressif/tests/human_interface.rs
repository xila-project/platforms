// The transport-independent logic of the USB human interface lower half
// (src/human_interface/logic.rs) compiles on the host, so its unit tests run here.
#[path = "../src/human_interface/logic.rs"]
mod logic;

/// Builds the same composite device as `UsbHumanInterface::new` on a fake OTG
/// instance, so that an endpoint allocation failure fails here instead of at boot
/// on the board. The control endpoint 0 OUT is allocated from the OUT buffer too,
/// which is why `ENDPOINT_OUT_BUFFER_SIZE` counts three max packet sizes.
mod composite_device {
    use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
    use embassy_usb::class::hid::{
        Config as HidConfig, HidBootProtocol, HidReaderWriter, HidSubclass, State as HidState,
    };
    use embassy_usb::{Builder, Handler};
    use embassy_usb_synopsys_otg::{
        Config, Driver, OtgInstance, PhyType, StateStorage, otg_v1::Otg,
    };
    use peripherals::human_interface::REPORT_DESCRIPTOR;

    use super::logic::{
        CONTROL_BUFFER_SIZE, ENDPOINT_MAX_PACKET_SIZE, ENDPOINT_OUT_BUFFER_SIZE,
        HID_POLL_MILLISECONDS,
    };

    struct NoOpHandler;

    impl Handler for NoOpHandler {}

    #[test]
    fn the_composite_device_builds_with_the_out_buffer_size() {
        let storage: &'static StateStorage<7> = Box::leak(Box::new(StateStorage::new()));
        // The registers are never read: building allocates endpoints only.
        let registers = unsafe { Otg::from_ptr(0x1000 as *mut ()) };
        let instance = OtgInstance {
            regs: registers,
            state: storage.as_state(),
            fifo_depth_words: 256,
            extra_rx_fifo_words: 30,
            phy_type: PhyType::InternalFullSpeed,
            calculate_trdt_fn: |_| 5,
        };
        let out: &'static mut [u8] =
            Box::leak(vec![0u8; ENDPOINT_OUT_BUFFER_SIZE].into_boxed_slice());
        let driver = Driver::new(out, instance, Config::default());

        let config_descriptor: &'static mut [u8] = Box::leak(vec![0u8; 256].into_boxed_slice());
        let bos_descriptor: &'static mut [u8] = Box::leak(vec![0u8; 16].into_boxed_slice());
        let msos_descriptor: &'static mut [u8] = Box::leak(vec![0u8; 16].into_boxed_slice());
        let control: &'static mut [u8] =
            Box::leak(vec![0u8; CONTROL_BUFFER_SIZE].into_boxed_slice());

        let mut configuration = embassy_usb::Config::new(0x1209, 0x0001);
        configuration.max_packet_size_0 = ENDPOINT_MAX_PACKET_SIZE as u8;

        let mut builder = Builder::new(
            driver,
            configuration,
            config_descriptor,
            bos_descriptor,
            msos_descriptor,
            control,
        );
        let cdc_state: &'static mut CdcState = Box::leak(Box::new(CdcState::new()));
        let _cdc = CdcAcmClass::new(&mut builder, cdc_state, ENDPOINT_MAX_PACKET_SIZE as u16);
        let hid_state: &'static mut HidState = Box::leak(Box::new(HidState::new()));
        let _hid = HidReaderWriter::<_, 64, 64>::new(
            &mut builder,
            hid_state,
            HidConfig {
                report_descriptor: REPORT_DESCRIPTOR,
                request_handler: None,
                poll_ms: HID_POLL_MILLISECONDS,
                max_packet_size: ENDPOINT_MAX_PACKET_SIZE as u16,
                hid_subclass: HidSubclass::No,
                hid_boot_protocol: HidBootProtocol::None,
            },
        );
        let bus_event_handler: &'static mut NoOpHandler = Box::leak(Box::new(NoOpHandler));
        builder.handler(bus_event_handler);

        // Allocates the control endpoint 0 in and out; the OUT one takes its max
        // packet size from the OUT buffer, after the CDC and HID OUT endpoints.
        let _device = builder.build();
    }
}

/// The write loop of the USB runner races the endpoint write against a
/// generation change, so that a report popped before an unplug or a suspend is
/// not sent after the host comes back. These tests pin the pattern on the
/// connection state: with the stale check as the first `select` argument, a bus
/// event during a pending write wins over the write, and wins too when the write
/// and the event become ready in the same poll, which is the re-plug that
/// re-enables the endpoint.
mod stale_write_race {
    use core::future::{Future, poll_fn};
    use core::pin::pin;
    use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    use std::cell::RefCell;
    use std::rc::Rc;

    use embassy_futures::select::{Either, select};

    use super::logic::{BusEvent, ConnectionState};

    fn noop_waker() -> Waker {
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(core::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
        // SAFETY: the vtable functions never dereference the null data pointer.
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
    }

    /// The stale check of the write loop: ready once a bus event bumped the
    /// generation past the one captured when the report was popped.
    fn stale_check(
        connection: &Rc<RefCell<ConnectionState>>,
        generation: u32,
    ) -> impl Future<Output = ()> + '_ {
        let connection = connection.clone();
        poll_fn(move |_| {
            if connection.borrow().is_stale(generation) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }

    #[test]
    fn a_bus_event_during_a_pending_write_wins_over_the_write() {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        let connection = Rc::new(RefCell::new(ConnectionState::new()));
        connection.borrow_mut().apply(BusEvent::Configured);
        let generation = connection.borrow().generation();

        let stale = stale_check(&connection, generation);
        // A write that stays pending, like an interrupt IN write blocked while the
        // bus is away; it would complete only after the host comes back.
        let write = poll_fn(|_| Poll::<Result<(), ()>>::Pending);
        let mut selected = pin!(select(stale, write));
        // First poll: the write is pending and no event happened yet.
        assert!(selected.as_mut().poll(&mut context).is_pending());
        // The bus goes away while the write is pending: the event bumps the
        // generation, as `apply_bus_event` does before waking the runner.
        connection.borrow_mut().apply(BusEvent::Suspended);
        match selected.as_mut().poll(&mut context) {
            Poll::Ready(Either::First(())) => {}
            Poll::Ready(Either::Second(_)) => {
                panic!("a pending write must not win over the stale check")
            }
            Poll::Pending => panic!("the stale check must resolve after a bus event"),
        }
    }

    #[test]
    fn a_bus_event_and_a_ready_write_in_the_same_poll_still_drop_the_report() {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        let connection = Rc::new(RefCell::new(ConnectionState::new()));
        connection.borrow_mut().apply(BusEvent::Configured);
        let generation = connection.borrow().generation();
        // Unplug, then re-plug: the host configures the device again and the
        // endpoint write becomes ready in the same poll as the event.
        connection.borrow_mut().apply(BusEvent::Reset);
        connection.borrow_mut().apply(BusEvent::Configured);

        let stale = stale_check(&connection, generation);
        let write = poll_fn(|_| Poll::Ready(Ok::<(), ()>(())));
        let mut selected = pin!(select(stale, write));
        match selected.as_mut().poll(&mut context) {
            Poll::Ready(Either::First(())) => {}
            _ => panic!("the stale check must win when both are ready in the same poll"),
        }
    }

    #[test]
    fn a_write_that_completes_without_a_bus_event_is_delivered() {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        let connection = Rc::new(RefCell::new(ConnectionState::new()));
        connection.borrow_mut().apply(BusEvent::Configured);
        let generation = connection.borrow().generation();

        let stale = stale_check(&connection, generation);
        let write = poll_fn(|_| Poll::Ready(Ok::<(), ()>(())));
        let mut selected = pin!(select(stale, write));
        match selected.as_mut().poll(&mut context) {
            Poll::Ready(Either::Second(Ok(()))) => {}
            _ => panic!("a write without a bus event must complete"),
        }
    }
}
