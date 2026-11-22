use core::{
    cell::RefCell,
    task::{Context, Poll},
};

use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    signal::Signal,
};
use embassy_time::Duration;
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ConnectionError, DisconnectReason,
    WifiController, WifiError, sta::StationConfig,
};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Result, Size,
};
use network::{
    CONNECT_WIFI_CLIENT, DISCONNECT_WIFI_CLIENT, GET_HARDWARE_ADDRESS, GET_KIND,
    GET_WIFI_CAPABILITIES, GET_WIFI_CONNECTION_INFO, GET_WIFI_CONNECTION_STATE,
    GET_WIFI_LAST_CONNECTION_ERROR, GET_WIFI_RADIO_STATE, GET_WIFI_SCAN_RESULT_COUNT,
    GET_WIFI_SCAN_STATE, IS_LINK_UP, InterfaceKind, MacAddress, WifiAuthentication,
    WifiCapabilities, WifiClientConfiguration, WifiConnectionErrorKind, WifiConnectionInfo,
    WifiConnectionState, WifiRadioState, WifiScanResultCount, WifiScanState, WifiSsid,
};
use shared::AnyByLayout;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const DISCONNECTION_TIMEOUT: Duration = Duration::from_secs(3);

enum Request {
    Connect(StationConfig, WifiSsid),
    Disconnect,
}

struct State {
    connection: WifiConnectionState,
    information: Option<WifiConnectionInfo>,
    error: WifiConnectionErrorKind,
}

static STATE: Mutex<CriticalSectionRawMutex, RefCell<State>> = Mutex::new(RefCell::new(State {
    connection: WifiConnectionState::DISCONNECTED,
    information: None,
    error: WifiConnectionErrorKind::NONE,
}));
static REQUEST: Signal<CriticalSectionRawMutex, Request> = Signal::new();

pub(super) struct StationControl(pub MacAddress);

impl StationControl {
    pub fn link_up() -> bool {
        STATE.lock(|state| state.borrow().connection == WifiConnectionState::CONNECTED)
    }

    fn control_command(
        &self,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Result<()> {
        match command {
            GET_KIND::IDENTIFIER => *GET_KIND::cast_output(output)? = InterfaceKind::WiFi,
            GET_HARDWARE_ADDRESS::IDENTIFIER => {
                *GET_HARDWARE_ADDRESS::cast_output(output)? = self.0
            }
            IS_LINK_UP::IDENTIFIER => *IS_LINK_UP::cast_output(output)? = Self::link_up(),
            GET_WIFI_CAPABILITIES::IDENTIFIER => {
                *GET_WIFI_CAPABILITIES::cast_output(output)? = WifiCapabilities {
                    station: 1,
                    access_point: 0,
                    authentication_mask: 0b0011,
                    maximum_scan_results: 0,
                };
            }
            GET_WIFI_RADIO_STATE::IDENTIFIER => {
                *GET_WIFI_RADIO_STATE::cast_output(output)? = WifiRadioState::ON;
            }
            GET_WIFI_SCAN_STATE::IDENTIFIER => {
                *GET_WIFI_SCAN_STATE::cast_output(output)? = WifiScanState::default();
            }
            GET_WIFI_SCAN_RESULT_COUNT::IDENTIFIER => {
                *GET_WIFI_SCAN_RESULT_COUNT::cast_output(output)? = WifiScanResultCount::default();
            }
            GET_WIFI_CONNECTION_STATE::IDENTIFIER => {
                *GET_WIFI_CONNECTION_STATE::cast_output(output)? =
                    STATE.lock(|state| state.borrow().connection);
            }
            GET_WIFI_CONNECTION_INFO::IDENTIFIER => {
                *GET_WIFI_CONNECTION_INFO::cast_output(output)? = STATE
                    .lock(|state| state.borrow().information)
                    .ok_or(IoErrorKind::NotFound)?;
            }
            GET_WIFI_LAST_CONNECTION_ERROR::IDENTIFIER => {
                *GET_WIFI_LAST_CONNECTION_ERROR::cast_output(output)? =
                    STATE.lock(|state| state.borrow().error);
            }
            CONNECT_WIFI_CLIENT::IDENTIFIER => {
                let configuration = CONNECT_WIFI_CLIENT::cast_input(input)?;
                connect(configuration)?;
            }
            DISCONNECT_WIFI_CLIENT::IDENTIFIER => {
                DISCONNECT_WIFI_CLIENT::cast_input(input)?;
                disconnect()?;
            }
            _ => return Err(IoErrorKind::Unsupported),
        }

        Ok(())
    }
}

impl DirectBaseOperations for StationControl {
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn control(
        &self,
        _: &mut Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        Poll::Ready(self.control_command(command, input, output))
    }
}

impl MountOperations for StationControl {}
impl DirectCharacterDevice for StationControl {}

fn connect(configuration: &WifiClientConfiguration) -> Result<()> {
    configuration.validate()?;

    let authentication = match configuration.authentication {
        WifiAuthentication::OPEN => AuthenticationMethodConfig::Open,
        WifiAuthentication::WPA2_PERSONAL => AuthenticationMethodConfig::Wpa2Personal(
            configuration
                .credential
                .as_bytes()?
                .try_into()
                .map_err(|_| IoErrorKind::InvalidArgument)?,
        ),
        _ => return Err(IoErrorKind::Unsupported),
    };

    let mut station = StationConfig::default()
        .with_ssid(
            configuration
                .ssid
                .as_bytes()?
                .try_into()
                .map_err(|_| IoErrorKind::InvalidArgument)?,
        )
        .with_authentication(authentication);
    if configuration.use_bssid != 0 {
        station = station.with_bssid(configuration.bssid);
    }
    if configuration.channel != 0 {
        if configuration.channel > 14 {
            return Err(IoErrorKind::InvalidArgument);
        }
        station = station.with_channel(configuration.channel);
    }

    STATE.lock(|state| {
        let mut state = state.borrow_mut();
        if state.connection != WifiConnectionState::DISCONNECTED {
            return Err(IoErrorKind::AlreadyInProgress);
        }
        state.connection = WifiConnectionState::CONNECTING;
        state.error = WifiConnectionErrorKind::NONE;
        Ok(())
    })?;
    REQUEST.signal(Request::Connect(station, configuration.ssid));
    Ok(())
}

fn disconnect() -> Result<()> {
    let queued = STATE.lock(|state| {
        let mut state = state.borrow_mut();
        match state.connection {
            WifiConnectionState::CONNECTED => {
                state.connection = WifiConnectionState::DISCONNECTING;
                Ok(true)
            }
            WifiConnectionState::DISCONNECTED => Ok(false),
            _ => Err(IoErrorKind::AlreadyInProgress),
        }
    })?;
    if queued {
        REQUEST.signal(Request::Disconnect);
    }
    Ok(())
}

pub(super) async fn run(mut radio: WifiController<'static>) {
    let mut request = REQUEST.wait().await;
    loop {
        match request {
            Request::Connect(station, ssid) => match associate(&mut radio, station, ssid).await {
                Ok(information) => {
                    STATE.lock(|state| {
                        let mut state = state.borrow_mut();
                        state.connection = WifiConnectionState::CONNECTED;
                        state.information = Some(information);
                    });
                    log::information!("Wi-Fi: station connected");
                }
                Err(error) => {
                    set_disconnected(error);
                    log::warning!("Wi-Fi: connection failed ({})", error_label(error));
                }
            },
            Request::Disconnect => {
                let error = stop_association(&mut radio).await;
                if radio.is_connected() {
                    STATE.lock(|state| {
                        let mut state = state.borrow_mut();
                        state.connection = WifiConnectionState::CONNECTED;
                        state.error = error;
                    });
                    log::warning!("Wi-Fi: disconnect failed ({})", error_label(error));
                } else {
                    set_disconnected(error);
                    log::information!("Wi-Fi: station disconnected");
                }
            }
        }

        request = if StationControl::link_up() {
            match select(REQUEST.wait(), radio.wait_for_disconnect_async()).await {
                Either::First(request) => request,
                Either::Second(event) => {
                    // A disconnect command may have been queued in the same executor turn.
                    // Take it first if present so it cannot overwrite a subsequent connect.
                    if let Some(request) = REQUEST.try_take() {
                        request
                    } else {
                        let error = event.map_or(WifiConnectionErrorKind::RADIO, |event| {
                            classify(event.reason)
                        });
                        set_disconnected(error);
                        log::warning!("Wi-Fi: link lost ({})", error_label(error));
                        REQUEST.wait().await
                    }
                }
            }
        } else {
            REQUEST.wait().await
        };
    }
}

async fn associate(
    radio: &mut WifiController<'static>,
    station: StationConfig,
    ssid: WifiSsid,
) -> core::result::Result<WifiConnectionInfo, WifiConnectionErrorKind> {
    radio
        .set_config(&Config::Station(station))
        .map_err(|_| WifiConnectionErrorKind::RADIO)?;

    let result = embassy_time::with_timeout(CONNECTION_TIMEOUT, radio.connect_async()).await;
    let connected = match result {
        Ok(Ok(connected)) => connected,
        Ok(Err(ConnectionError::Failed(failure))) => return Err(classify(failure.reason)),
        Ok(Err(_)) => return Err(WifiConnectionErrorKind::RADIO),
        Err(_) => {
            stop_association(radio).await;
            return Err(WifiConnectionErrorKind::TIMEOUT);
        }
    };

    Ok(WifiConnectionInfo {
        ssid,
        bssid: connected.bssid,
        channel: connected.channel,
        rssi: radio.rssi().unwrap_or_default().clamp(-128, 127) as i8,
        authentication: match connected.authmode {
            AuthenticationMethod::None => WifiAuthentication::OPEN,
            AuthenticationMethod::Wpa2Personal => WifiAuthentication::WPA2_PERSONAL,
            AuthenticationMethod::Wpa3Personal => WifiAuthentication::WPA3_PERSONAL,
            AuthenticationMethod::Wpa2Wpa3Personal => WifiAuthentication::WPA2_WPA3_PERSONAL,
            _ => WifiAuthentication::UNSUPPORTED,
        },
    })
}

async fn stop_association(radio: &mut WifiController<'static>) -> WifiConnectionErrorKind {
    match embassy_time::with_timeout(DISCONNECTION_TIMEOUT, radio.disconnect_async()).await {
        Ok(Ok(_)) | Ok(Err(WifiError::NotConnected)) => WifiConnectionErrorKind::NONE,
        Ok(Err(_)) => WifiConnectionErrorKind::RADIO,
        Err(_) => WifiConnectionErrorKind::TIMEOUT,
    }
}

fn set_disconnected(error: WifiConnectionErrorKind) {
    STATE.lock(|state| {
        let mut state = state.borrow_mut();
        state.connection = WifiConnectionState::DISCONNECTED;
        state.information = None;
        state.error = error;
    });
}

fn classify(reason: DisconnectReason) -> WifiConnectionErrorKind {
    use DisconnectReason::*;
    match reason {
        AuthenticationFailed
        | AuthenticationExpired
        | AssociationNotAuthenticated
        | FourWayHandshakeTimeout
        | HandshakeTimeout
        | MicFailure => WifiConnectionErrorKind::AUTHENTICATION,
        NoAccessPointFound | NoAccessPointFoundInRssiThreshold => {
            WifiConnectionErrorKind::ACCESS_POINT_NOT_FOUND
        }
        NoAccessPointFoundWithCompatibleSecurity | NoAccessPointFoundInAuthmodeThreshold => {
            WifiConnectionErrorKind::UNSUPPORTED_CONFIGURATION
        }
        Timeout | BeaconTimeout => WifiConnectionErrorKind::TIMEOUT,
        _ => WifiConnectionErrorKind::ASSOCIATION,
    }
}

fn error_label(error: WifiConnectionErrorKind) -> &'static str {
    match error {
        WifiConnectionErrorKind::NONE => "none",
        WifiConnectionErrorKind::AUTHENTICATION => "authentication",
        WifiConnectionErrorKind::ACCESS_POINT_NOT_FOUND => "access point not found",
        WifiConnectionErrorKind::ASSOCIATION => "association",
        WifiConnectionErrorKind::TIMEOUT => "timeout",
        WifiConnectionErrorKind::UNSUPPORTED_CONFIGURATION => "unsupported configuration",
        WifiConnectionErrorKind::RADIO => "radio",
        _ => "driver",
    }
}
