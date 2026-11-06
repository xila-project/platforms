//! UART endpoint over esp-hal's blocking driver.
//!
//! [`EspUart`] owns the driver and implements the peripherals UART contract:
//! `embedded_io::{Read, Write}` plus `UartControl`. esp-hal blocks inside
//! `read` and `write`, so non-blocking mode checks `read_ready` and
//! `write_ready` first and returns `WouldBlock` instead of waiting.
//!
//! Limits of this endpoint:
//! - RTS/CTS hardware flow control is not offered: it needs the RTS and CTS
//!   pins, which chip initialisation does not assign yet;
//! - `flush_rx` drains the receive FIFO by reading it; esp-hal exposes no FIFO
//!   reset, so `flush_tx` waits for queued output to be transmitted instead of
//!   discarding it.

use embedded_io::{ErrorType, Read, Write};
use esp_hal::{
    Blocking,
    uart::{
        Config as HalConfig, DataBits as HalDataBits, Parity as HalParity, RxError,
        StopBits as HalStopBits, SwFlowControl, Uart as HalUart,
    },
};
use peripherals::uart::{
    Capabilities, Config, DataBits, Error, FlowControl, Parity, StopBits, UartControl,
};

/// Capabilities of the ESP UART endpoint. Baud range is conservative until
/// validated on boards.
pub const CAPABILITIES: Capabilities = Capabilities {
    min_baud_rate: 300,
    max_baud_rate: 2_000_000,
    data_five: true,
    data_six: true,
    data_seven: true,
    data_eight: true,
    stop_two: true,
    parity_even: true,
    parity_odd: true,
    rts_cts: false,
    xon_xoff: true,
};

/// XON and XOFF characters used by software flow control.
const XON: u8 = 0x11;
const XOFF: u8 = 0x13;
/// Receive FIFO fill levels, in bytes, at which XON and XOFF are sent. Fixed
/// values until they can be tuned on hardware.
const XON_THRESHOLD: u8 = 20;
const XOFF_THRESHOLD: u8 = 100;

/// Owned UART endpoint.
pub struct EspUart {
    uart: HalUart<'static, Blocking>,
    config: Config,
    nonblocking: bool,
}

impl EspUart {
    /// Applies `config` to the driver. Fails without touching the driver when
    /// the endpoint cannot express the settings.
    pub fn new(uart: HalUart<'static, Blocking>, config: Config) -> Result<Self, Error> {
        CAPABILITIES.check(&config)?;

        let mut endpoint = Self {
            uart,
            config,
            nonblocking: false,
        };
        endpoint
            .uart
            .apply_config(&to_hal_config(&config)?)
            .map_err(|_| Error::Io)?;

        Ok(endpoint)
    }
}

impl ErrorType for EspUart {
    type Error = Error;
}

impl Read for EspUart {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.nonblocking && !self.uart.read_ready() {
            return Err(Error::WouldBlock);
        }

        self.uart.read(buffer).map_err(map_rx_error)
    }
}

impl Write for EspUart {
    fn write(&mut self, buffer: &[u8]) -> Result<usize, Error> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.nonblocking && !self.uart.write_ready() {
            return Err(Error::WouldBlock);
        }

        self.uart.write(buffer).map_err(|_| Error::Io)
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.uart.flush().map_err(|_| Error::Io)
    }
}

impl UartControl for EspUart {
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn config(&self) -> Config {
        self.config
    }

    fn configure(&mut self, config: Config) -> Result<(), Error> {
        CAPABILITIES.check(&config)?;
        self.uart
            .apply_config(&to_hal_config(&config)?)
            .map_err(|_| Error::Io)?;
        self.config = config;

        Ok(())
    }

    fn is_nonblocking(&self) -> bool {
        self.nonblocking
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), Error> {
        self.nonblocking = nonblocking;
        Ok(())
    }

    fn flush_rx(&mut self) -> Result<(), Error> {
        let mut scratch = [0u8; 16];
        while self.uart.read_ready() {
            // Bytes and receive errors are discarded; the FIFO is drained
            // until it reports no data.
            let _ = self.uart.read(&mut scratch);
        }

        Ok(())
    }

    fn flush_tx(&mut self) -> Result<(), Error> {
        // esp-hal has no TX FIFO reset: queued output is transmitted before
        // this returns, so nothing remains queued afterwards.
        self.uart.flush().map_err(|_| Error::Io)
    }
}

fn to_hal_config(config: &Config) -> Result<HalConfig, Error> {
    let data_bits = match config.data_bits {
        DataBits::Five => HalDataBits::_5,
        DataBits::Six => HalDataBits::_6,
        DataBits::Seven => HalDataBits::_7,
        DataBits::Eight => HalDataBits::_8,
    };
    let parity = match config.parity {
        Parity::None => HalParity::None,
        Parity::Even => HalParity::Even,
        Parity::Odd => HalParity::Odd,
    };
    let stop_bits = match config.stop_bits {
        StopBits::One => HalStopBits::_1,
        StopBits::Two => HalStopBits::_2,
    };
    let sw_flow_control = match config.flow_control {
        FlowControl::None => SwFlowControl::Disabled,
        FlowControl::XonXoff => SwFlowControl::Enabled {
            xon_char: XON,
            xoff_char: XOFF,
            xon_threshold: XON_THRESHOLD,
            xoff_threshold: XOFF_THRESHOLD,
        },
        FlowControl::RtsCts => return Err(Error::UnsupportedFlowControl),
    };

    Ok(HalConfig::default()
        .with_baudrate(config.baud_rate)
        .with_data_bits(data_bits)
        .with_parity(parity)
        .with_stop_bits(stop_bits)
        .with_sw_flow_ctrl(sw_flow_control))
}

fn map_rx_error(error: RxError) -> Error {
    match error {
        RxError::FifoOverflowed => Error::Overrun,
        RxError::FrameFormatViolated => Error::Framing,
        RxError::ParityMismatch => Error::ParityError,
        _ => Error::Io,
    }
}
