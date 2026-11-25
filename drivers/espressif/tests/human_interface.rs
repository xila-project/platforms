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
        let instance = OtgInstance {
            // The registers are never read: building allocates endpoints only.
            regs: unsafe { Otg::from_ptr(0x1000 as *mut ()) },
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

        let mut config = embassy_usb::Config::new(0x1209, 0x0001);
        config.max_packet_size_0 = ENDPOINT_MAX_PACKET_SIZE as u8;

        let mut builder = Builder::new(
            driver,
            config,
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
