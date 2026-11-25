use core::{
    cell::RefCell,
    task::{Context, Poll},
};

use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    signal::Signal,
};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::timer::timg::{MwdtStage, MwdtStageAction, TimerGroup, Wdt};
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig,
    DisconnectReason, WifiController, WifiError,
    scan::{ScanConfig, ScanTypeConfig},
    sta::StationConfig,
};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Result, Size,
};
use network::{
    CONNECT_WIFI_CLIENT, DISCONNECT_WIFI_CLIENT, GET_KIND, GET_WIFI_CAPABILITIES,
    GET_WIFI_CONNECTION_INFO, GET_WIFI_CONNECTION_STATE, GET_WIFI_LAST_CONNECTION_ERROR,
    GET_WIFI_RADIO_STATE, GET_WIFI_SCAN_RESULT, GET_WIFI_SCAN_RESULT_COUNT, GET_WIFI_SCAN_STATE,
    IS_LINK_UP, InterfaceKind, SET_WIFI_RADIO_STATE, START_WIFI_SCAN, WifiAccessPoint,
    WifiAuthentication, WifiCapabilities, WifiClientConfiguration, WifiConnectionErrorKind,
    WifiConnectionInfo, WifiConnectionState, WifiRadioState, WifiScanConfiguration, WifiScanPhase,
    WifiScanResultCount, WifiScanState, WifiSsid,
};
use shared::AnyByLayout;
use task::TaskIdentifier;
use virtual_file_system::VirtualFileSystem;

use super::{
    INTERNAL_HEAP, MINIMUM_FREE_INTERNAL_HEAP, Peripherals, allocation, configuration,
    profile::{
        Profile, StaticAddress, address_for_station, needs_dhcp_restart, retry_station,
        validate_station,
    },
};
use crate::memory::MemoryManager;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const DISCONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
const LINK_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const MAXIMUM_SCAN_RESULTS: usize = 16;
const STARTUP_MARKER: &str = "/configuration/system/.wifi-starting";

enum Request {
    SetRadio(WifiRadioState),
    Connect(WifiClientConfiguration),
    Disconnect,
    Scan(WifiScanConfiguration),
}

struct State {
    radio: WifiRadioState,
    busy: bool,
    stopping: bool,
    connection: WifiConnectionState,
    information: Option<WifiConnectionInfo>,
    error: WifiConnectionErrorKind,
    scan: WifiScanState,
    results: [Option<WifiAccessPoint>; MAXIMUM_SCAN_RESULTS],
    count: u16,
}

static STATE: Mutex<CriticalSectionRawMutex, RefCell<State>> = Mutex::new(RefCell::new(State {
    radio: WifiRadioState::OFF,
    busy: false,
    stopping: false,
    connection: WifiConnectionState::DISCONNECTED,
    information: None,
    error: WifiConnectionErrorKind::NONE,
    scan: WifiScanState {
        generation: 0,
        phase: WifiScanPhase::IDLE,
        reserved: [0; 3],
    },
    results: [None; MAXIMUM_SCAN_RESULTS],
    count: 0,
}));
static REQUEST: Signal<CriticalSectionRawMutex, Request> = Signal::new();

fn update_state<R>(update: impl FnOnce(&mut State) -> R) -> R {
    STATE.lock(|state| update(&mut state.borrow_mut()))
}

pub(super) struct StationControl;

impl StationControl {
    pub fn link_up() -> bool {
        update_state(|state| state.connection == WifiConnectionState::CONNECTED)
    }

    fn control_command(
        &self,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Result<()> {
        match command {
            GET_KIND::IDENTIFIER => *GET_KIND::cast_output(output)? = InterfaceKind::WiFi as u8,
            IS_LINK_UP::IDENTIFIER => *IS_LINK_UP::cast_output(output)? = Self::link_up() as u8,
            GET_WIFI_CAPABILITIES::IDENTIFIER => {
                *GET_WIFI_CAPABILITIES::cast_output(output)? = WifiCapabilities {
                    station: 1,
                    access_point: 0,
                    authentication_mask: 0b0011,
                    maximum_scan_results: MAXIMUM_SCAN_RESULTS as u8,
                };
            }
            GET_WIFI_RADIO_STATE::IDENTIFIER => {
                *GET_WIFI_RADIO_STATE::cast_output(output)? = update_state(|state| state.radio);
            }
            GET_WIFI_SCAN_STATE::IDENTIFIER => {
                *GET_WIFI_SCAN_STATE::cast_output(output)? = update_state(|state| state.scan);
            }
            GET_WIFI_SCAN_RESULT_COUNT::IDENTIFIER => {
                *GET_WIFI_SCAN_RESULT_COUNT::cast_output(output)? =
                    update_state(|state| WifiScanResultCount {
                        generation: state.scan.generation,
                        count: state.count,
                        reserved: [0; 2],
                    });
            }
            GET_WIFI_SCAN_RESULT::IDENTIFIER => {
                let (request, result) = GET_WIFI_SCAN_RESULT::cast(input, output)?;
                *result = update_state(|state| {
                    if request.generation != state.scan.generation || request.index >= state.count {
                        return Err(IoErrorKind::InvalidArgument);
                    }
                    state.results[usize::from(request.index)].ok_or(IoErrorKind::InvalidArgument)
                })?;
            }
            GET_WIFI_CONNECTION_STATE::IDENTIFIER => {
                *GET_WIFI_CONNECTION_STATE::cast_output(output)? =
                    update_state(|state| state.connection);
            }
            GET_WIFI_CONNECTION_INFO::IDENTIFIER => {
                *GET_WIFI_CONNECTION_INFO::cast_output(output)? =
                    update_state(|state| state.information).ok_or(IoErrorKind::NotFound)?;
            }
            GET_WIFI_LAST_CONNECTION_ERROR::IDENTIFIER => {
                *GET_WIFI_LAST_CONNECTION_ERROR::cast_output(output)? =
                    update_state(|state| state.error);
            }
            SET_WIFI_RADIO_STATE::IDENTIFIER => {
                let desired = *SET_WIFI_RADIO_STATE::cast_input(input)?;
                if !matches!(desired, WifiRadioState::ON | WifiRadioState::OFF) {
                    return Err(IoErrorKind::InvalidArgument);
                }
                enqueue(Request::SetRadio(desired))?;
            }
            START_WIFI_SCAN::IDENTIFIER => {
                let configuration = *START_WIFI_SCAN::cast_input(input)?;
                configuration.validate()?;
                if configuration.channel > 14 {
                    return Err(IoErrorKind::InvalidArgument);
                }
                enqueue(Request::Scan(configuration))?;
            }
            CONNECT_WIFI_CLIENT::IDENTIFIER => {
                let configuration = CONNECT_WIFI_CLIENT::cast_input(input)?;
                validate_station(configuration)?;
                enqueue(Request::Connect(configuration.clone()))?;
            }
            DISCONNECT_WIFI_CLIENT::IDENTIFIER => {
                DISCONNECT_WIFI_CLIENT::cast_input(input)?;
                enqueue(Request::Disconnect)?;
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

fn enqueue(request: Request) -> Result<()> {
    update_state(|state| {
        if matches!(request, Request::SetRadio(WifiRadioState::OFF))
            && state.radio == WifiRadioState::ON
        {
            // Finish the current bounded operation, then turn off. The off
            // request supersedes one that is still waiting in the mailbox.
            state.stopping = true;
            REQUEST.signal(request);
            return Ok(());
        }
        if state.busy || state.stopping {
            return Err(IoErrorKind::AlreadyInProgress);
        }
        match request {
            Request::SetRadio(radio) if state.radio == radio => return Ok(()),
            Request::SetRadio(_) => (),
            _ if state.radio == WifiRadioState::OFF => return Err(IoErrorKind::NetworkDown),
            Request::Scan(_) => state.scan.phase = WifiScanPhase::SCANNING,
            Request::Connect(_) if state.connection != WifiConnectionState::DISCONNECTED => {
                return Err(IoErrorKind::AlreadyInProgress);
            }
            Request::Connect(_) => {
                state.connection = WifiConnectionState::CONNECTING;
                state.error = WifiConnectionErrorKind::NONE;
            }
            Request::Disconnect => state.connection = WifiConnectionState::DISCONNECTING,
        }
        state.busy = true;
        REQUEST.signal(request);
        Ok(())
    })
}

pub(super) async fn run(
    mut peripherals: Peripherals,
    heap: &'static MemoryManager,
    file_system: &'static VirtualFileSystem,
    task: TaskIdentifier,
    profile: Option<Profile>,
) {
    let mut preferred = profile.as_ref().map(|profile| profile.station.clone());
    let mut storage = Storage {
        file_system,
        task,
        address: profile.and_then(|profile| profile.address),
        last_associated_network: None,
    };

    let mut start_now = match file_system.get_statistics(&STARTUP_MARKER).await {
        Err(IoErrorKind::NotFound) => true,
        Ok(_) => {
            log::warning!("Wi-Fi: previous startup was interrupted; radio stays off");
            false
        }
        Err(error) => {
            log::warning!("Wi-Fi: cannot read startup marker ({error})");
            false
        }
    };
    let timer_group = TimerGroup::new(peripherals.scheduler_timer);
    let mut scheduler = Some((timer_group.timer0, peripherals.scheduler_interrupt));
    let mut watchdog = timer_group.wdt;
    loop {
        if !start_now {
            // Only enabling the radio is accepted while it is off.
            if !matches!(REQUEST.wait().await, Request::SetRadio(WifiRadioState::ON)) {
                continue;
            }
        }
        start_now = false;
        update_state(|state| state.busy = true);
        let marker = storage.begin_startup().await;
        if let Err(error) = marker {
            log::warning!("Wi-Fi: cannot protect radio startup ({error})");
            set_radio_failure();
            continue;
        }
        let before = allocation::free_bytes(heap);
        if before < MINIMUM_FREE_INTERNAL_HEAP {
            storage.finish_startup().await;
            log::warning!("Wi-Fi: insufficient internal heap for radio startup");
            set_radio_failure();
            continue;
        }
        arm_watchdog(&mut watchdog);
        if let Some((timer, interrupt)) = scheduler.take() {
            let _ = INTERNAL_HEAP.init(heap);
            // The existing executor remains on the scheduler's main thread.
            allocation::internal(|| {
                esp_rtos::start(timer, interrupt);
                allocation::record_main_task();
            });
        }
        let radio = allocation::internal(|| {
            WifiController::new(
                peripherals.radio.reborrow(),
                ControllerConfig::default()
                    .with_static_rx_buf_num(6)
                    .with_dynamic_rx_buf_num(12)
                    .with_dynamic_tx_buf_num(8)
                    .with_rx_queue_size(12),
            )
        });
        let mut radio = match radio {
            Ok(radio) => radio,
            Err(_) => {
                watchdog.disable();
                storage.finish_startup().await;
                log::warning!("Wi-Fi: radio startup failed");
                set_radio_failure();
                continue;
            }
        };
        let after = allocation::free_bytes(heap);
        log::information!(
            "Wi-Fi: radio on; internal heap free before startup {before}, after startup {after} bytes"
        );
        update_state(|state| {
            state.radio = WifiRadioState::ON;
            state.error = WifiConnectionErrorKind::NONE;
            state.busy = true;
        });
        let mut retry = Retry::new();
        let mut connection = preferred.clone();
        if let Some(station) = &connection {
            try_connect(
                &mut radio,
                station,
                &mut retry,
                &mut storage,
                preferred.as_ref(),
            )
            .await;
        }
        watchdog.disable();
        storage.finish_startup().await;
        update_state(|state| state.busy = false);

        drive(
            &mut radio,
            &mut storage,
            &mut preferred,
            &mut connection,
            retry,
        )
        .await;
        // Deinitializing the controller shuts down Wi-Fi and the modem clocks.
        allocation::internal(|| drop(radio));
        update_state(|state| {
            state.radio = WifiRadioState::OFF;
            state.busy = false;
            state.stopping = false;
            state.scan.phase = WifiScanPhase::IDLE;
            state.scan.generation = state.scan.generation.wrapping_add(1);
            state.count = 0;
        });
        log::information!("Wi-Fi: radio off");
    }
}

struct Storage {
    file_system: &'static VirtualFileSystem,
    task: TaskIdentifier,
    address: Option<StaticAddress>,
    last_associated_network: Option<WifiSsid>,
}

impl Storage {
    async fn begin_startup(&self) -> Result<()> {
        match self.file_system.remove(self.task, &STARTUP_MARKER).await {
            Ok(()) | Err(IoErrorKind::NotFound) => (),
            Err(error) => return Err(error),
        }
        self.file_system
            .create_directory(self.task, &STARTUP_MARKER)
            .await
    }

    async fn finish_startup(&self) {
        if let Err(error) = self.file_system.remove(self.task, &STARTUP_MARKER).await {
            log::warning!("Wi-Fi: cannot clear startup marker ({error})");
        }
    }
}

fn arm_watchdog(watchdog: &mut Wdt<esp_hal::peripherals::TIMG0<'static>>) {
    watchdog.set_timeout(MwdtStage::Stage0, esp_hal::time::Duration::from_secs(30));
    watchdog.set_stage_action(MwdtStage::Stage0, MwdtStageAction::ResetSystem);
    watchdog.feed();
    watchdog.enable();
}

struct Retry {
    deadline: Option<Instant>,
    delay_seconds: u64,
}

impl Retry {
    fn new() -> Self {
        Self {
            deadline: None,
            delay_seconds: 2,
        }
    }

    fn schedule(&mut self) {
        self.deadline = Some(Instant::now() + Duration::from_secs(self.delay_seconds));
        self.delay_seconds = (self.delay_seconds * 2).min(60);
    }
}

async fn drive(
    radio: &mut WifiController<'_>,
    storage: &mut Storage,
    preferred: &mut Option<WifiClientConfiguration>,
    connection: &mut Option<WifiClientConfiguration>,
    mut retry: Retry,
) {
    let mut save_on_success = false;
    loop {
        let request = if StationControl::link_up() {
            // The radio can disconnect between the SDK's state check and its event subscription.
            match select(
                REQUEST.wait(),
                embassy_time::with_timeout(
                    LINK_RECHECK_INTERVAL,
                    allocation::poll_internal(radio.wait_for_disconnect_async()),
                ),
            )
            .await
            {
                Either::First(request) => Some(request),
                Either::Second(event) => {
                    let reason = match event {
                        Ok(Ok(event)) => classify(event.reason),
                        Err(_) if radio.is_connected() => continue,
                        _ => WifiConnectionErrorKind::RADIO,
                    };
                    set_disconnected(reason);
                    if connection.is_some() {
                        retry.schedule();
                    }
                    log::warning!("Wi-Fi: link lost ({})", error_label(reason));
                    continue;
                }
            }
        } else if let Some(deadline) = retry.deadline {
            match select(REQUEST.wait(), Timer::at(deadline)).await {
                Either::First(request) => Some(request),
                Either::Second(_) => None,
            }
        } else {
            Some(REQUEST.wait().await)
        };

        let automatic = request.is_none();
        let attempt = match request {
            Some(Request::SetRadio(WifiRadioState::OFF)) => {
                let error = stop_association(radio).await;
                set_disconnected(error);
                return;
            }
            Some(Request::SetRadio(_)) => false,
            Some(Request::Connect(station)) => {
                *connection = Some(station);
                save_on_success = true;
                retry = Retry::new();
                true
            }
            Some(Request::Disconnect) => {
                *connection = None;
                retry = Retry::new();
                let error = stop_association(radio).await;
                if radio.is_connected() {
                    update_state(|state| {
                        state.connection = WifiConnectionState::CONNECTED;
                        state.error = error;
                    });
                    log::warning!("Wi-Fi: disconnect failed ({})", error_label(error));
                } else {
                    set_disconnected(error);
                    log::information!("Wi-Fi: station disconnected");
                }
                false
            }
            Some(Request::Scan(configuration)) => {
                scan(radio, configuration).await;
                false
            }
            None => {
                // Reserve this operation against an incoming control request.
                update_state(|state| {
                    if state.busy {
                        return false;
                    }
                    state.busy = true;
                    true
                })
            }
        };
        if attempt && let Some(station) = connection {
            if automatic {
                retry.deadline = None;
                *station = retry_station(station);
            }
            if try_connect(radio, station, &mut retry, storage, preferred.as_ref()).await
                && save_on_success
            {
                save_on_success = false;
                *preferred = Some(retry_station(station));
                match configuration::save(
                    storage.file_system,
                    storage.task,
                    station,
                    storage.address,
                )
                .await
                {
                    Ok(()) => log::information!("Wi-Fi: station profile saved"),
                    Err(error) => {
                        update_state(|state| state.error = WifiConnectionErrorKind::DRIVER);
                        log::warning!("Wi-Fi: cannot save station profile ({error})");
                    }
                }
            }
        }
        // A request may have arrived after the retry timer fired.
        if automatic && !attempt {
            continue;
        }
        update_state(|state| state.busy = false);
    }
}

async fn scan(radio: &mut WifiController<'_>, requested: WifiScanConfiguration) {
    let mut configuration = ScanConfig::default()
        .with_max(usize::from(requested.result_limit).min(MAXIMUM_SCAN_RESULTS));
    if requested.channel != 0 {
        configuration = configuration.with_channel(requested.channel);
    }
    if requested.passive != 0 {
        configuration = configuration.with_scan_type(ScanTypeConfig::Passive(
            esp_hal::time::Duration::from_millis(110),
        ));
    }
    let scan = embassy_time::with_timeout(
        SCAN_TIMEOUT,
        allocation::poll_internal(radio.scan_async(&configuration)),
    )
    .await;
    if !matches!(scan, Ok(Ok(_))) {
        log::warning!("Wi-Fi: scan failed");
    }
    update_state(|state| match scan {
        Ok(Ok(results)) => {
            state.count = 0;
            for access_point in results {
                // The SDK's public string view excludes invalid UTF-8 bytes.
                let name = access_point.ssid.as_str().as_bytes();
                if name.len() != access_point.ssid.len()
                    || name.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
                {
                    continue;
                }
                let Ok(ssid) = WifiSsid::new(name) else {
                    continue;
                };
                state.results[usize::from(state.count)] = Some(WifiAccessPoint {
                    ssid,
                    bssid: access_point.bssid,
                    channel: access_point.channel,
                    rssi: access_point.signal_strength,
                    authentication: access_point
                        .auth_method
                        .map_or(WifiAuthentication::UNSUPPORTED, authentication_kind),
                });
                state.count += 1;
            }
            state.scan.generation = state.scan.generation.wrapping_add(1);
            state.scan.phase = WifiScanPhase::COMPLETED;
        }
        _ => {
            state.scan.phase = WifiScanPhase::FAILED;
            if state.connection == WifiConnectionState::DISCONNECTED {
                state.error = WifiConnectionErrorKind::RADIO;
            }
        }
    });
}

async fn try_connect(
    radio: &mut WifiController<'_>,
    station: &WifiClientConfiguration,
    retry: &mut Retry,
    storage: &mut Storage,
    preferred: Option<&WifiClientConfiguration>,
) -> bool {
    update_state(|state| state.connection = WifiConnectionState::CONNECTING);
    let result = async {
        let information = associate(radio, station).await?;
        let addressing = match storage.address {
            Some(address) if address_for_station(preferred, Some(address), station).is_none() => {
                configuration::switch_to_dhcp(storage.file_system, storage.task, address)
                    .await
                    .map(|()| storage.address = None)
            }
            None if needs_dhcp_restart(storage.last_associated_network, &station.ssid) => {
                configuration::restart_dhcp(storage.file_system, storage.task).await
            }
            _ => Ok(()),
        };
        if let Err(error) = addressing {
            stop_association(radio).await;
            log::warning!("Wi-Fi: cannot update addressing ({error})");
            return Err(WifiConnectionErrorKind::DRIVER);
        }
        storage.last_associated_network = Some(station.ssid);
        Ok(information)
    }
    .await;
    match result {
        Ok(information) => {
            update_state(|state| {
                state.connection = WifiConnectionState::CONNECTED;
                state.information = Some(information);
                state.error = WifiConnectionErrorKind::NONE;
            });
            *retry = Retry::new();
            log::information!("Wi-Fi: station connected");
            true
        }
        Err(error) => {
            set_disconnected(error);
            retry.schedule();
            log::warning!(
                "Wi-Fi: connection failed ({}); retry scheduled",
                error_label(error)
            );
            false
        }
    }
}

fn station_configuration(configuration: &WifiClientConfiguration) -> Result<StationConfig> {
    let authentication = match configuration.authentication {
        WifiAuthentication::OPEN => AuthenticationMethodConfig::Open,
        // esp-radio cannot join with WPA3 (SAE), but a WPA2/WPA3 transition
        // network also accepts WPA2 clients.
        WifiAuthentication::WPA2_PERSONAL | WifiAuthentication::WPA2_WPA3_PERSONAL => {
            AuthenticationMethodConfig::Wpa2Personal(
                configuration
                    .credential
                    .as_bytes()?
                    .try_into()
                    .map_err(|_| IoErrorKind::InvalidArgument)?,
            )
        }
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
        station = station.with_channel(configuration.channel);
    }
    Ok(station)
}

async fn associate(
    radio: &mut WifiController<'_>,
    station: &WifiClientConfiguration,
) -> core::result::Result<WifiConnectionInfo, WifiConnectionErrorKind> {
    let radio_configuration = station_configuration(station)
        .map_err(|_| WifiConnectionErrorKind::UNSUPPORTED_CONFIGURATION)?;
    allocation::internal(|| radio.set_config(&Config::Station(radio_configuration)))
        .map_err(|_| WifiConnectionErrorKind::RADIO)?;
    let result = embassy_time::with_timeout(
        CONNECTION_TIMEOUT,
        allocation::poll_internal(radio.connect_async()),
    )
    .await;
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
        ssid: station.ssid,
        bssid: connected.bssid,
        channel: connected.channel,
        rssi: allocation::internal(|| radio.rssi())
            .unwrap_or(i32::from(i8::MIN))
            .clamp(-128, 127) as i8,
        authentication: authentication_kind(connected.authmode),
    })
}

async fn stop_association(radio: &mut WifiController<'_>) -> WifiConnectionErrorKind {
    match embassy_time::with_timeout(
        DISCONNECTION_TIMEOUT,
        allocation::poll_internal(radio.disconnect_async()),
    )
    .await
    {
        Ok(Ok(_)) | Ok(Err(WifiError::NotConnected)) => WifiConnectionErrorKind::NONE,
        Ok(Err(_)) => WifiConnectionErrorKind::RADIO,
        Err(_) => WifiConnectionErrorKind::TIMEOUT,
    }
}

fn set_disconnected(error: WifiConnectionErrorKind) {
    update_state(|state| {
        state.connection = WifiConnectionState::DISCONNECTED;
        state.information = None;
        state.error = error;
    });
}

fn set_radio_failure() {
    update_state(|state| {
        state.radio = WifiRadioState::OFF;
        state.busy = false;
        state.error = WifiConnectionErrorKind::RADIO;
    });
}

fn authentication_kind(method: AuthenticationMethod) -> WifiAuthentication {
    match method {
        AuthenticationMethod::None => WifiAuthentication::OPEN,
        // A WPA/WPA2 mixed network accepts WPA2 clients, which is how the
        // station joins it.
        AuthenticationMethod::Wpa2Personal | AuthenticationMethod::WpaWpa2Personal => {
            WifiAuthentication::WPA2_PERSONAL
        }
        AuthenticationMethod::Wpa3Personal => WifiAuthentication::WPA3_PERSONAL,
        AuthenticationMethod::Wpa2Wpa3Personal => WifiAuthentication::WPA2_WPA3_PERSONAL,
        _ => WifiAuthentication::UNSUPPORTED,
    }
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
