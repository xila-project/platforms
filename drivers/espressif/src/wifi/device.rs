use esp_radio::wifi::{Interface, WifiRxToken, WifiTxToken};
use smoltcp::{
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
};

use super::controller::StationControl;

pub(super) struct StationDevice(pub Interface);

impl Device for StationDevice {
    type RxToken<'a> = ReceiveToken;
    type TxToken<'a> = TransmitToken;

    fn receive(&mut self, _: Instant) -> Option<(ReceiveToken, TransmitToken)> {
        if !StationControl::link_up() {
            return None;
        }
        self.0
            .receive()
            .map(|(receive, transmit)| (ReceiveToken(receive), TransmitToken(transmit)))
    }

    fn transmit(&mut self, _: Instant) -> Option<TransmitToken> {
        if !StationControl::link_up() {
            return None;
        }
        self.0.transmit().map(TransmitToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let radio = embassy_net_driver::Driver::capabilities(&self.0);
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = radio.max_transmission_unit;
        capabilities.max_burst_size = radio.max_burst_size;
        capabilities
    }
}

pub(super) struct ReceiveToken(WifiRxToken);

impl RxToken for ReceiveToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, consume: F) -> R {
        self.0.consume_token(|buffer| consume(buffer))
    }
}

pub(super) struct TransmitToken(WifiTxToken);

impl TxToken for TransmitToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, length: usize, consume: F) -> R {
        self.0.consume_token(length, consume)
    }
}
