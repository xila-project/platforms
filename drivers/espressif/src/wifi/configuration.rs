use file_system::{AccessFlags, CreateFlags, Flags, IoErrorKind, Kind, Permissions, Result};
use network::{
    ADD_DNS_SERVER, ADD_IP_ADDRESS, ADD_ROUTE, GET_DHCP_STATE, GET_DNS_SERVER,
    GET_DNS_SERVER_COUNT, GET_IP_ADDRESS, GET_IP_ADDRESS_COUNT, GET_ROUTE, GET_ROUTE_COUNT,
    IpAddress, IpCidr, REMOVE_DNS_SERVER, REMOVE_IP_ADDRESS, REMOVE_ROUTE, Route, SET_DHCP_STATE,
    WifiClientConfiguration,
};
use task::TaskIdentifier;
use virtual_file_system::{File, VirtualFileSystem};

use super::profile::{MAXIMUM_SIZE, Profile, StaticAddress};

pub(super) const INTERFACE_PATH: &str = "/devices/network/wlan0";

const PATH: &str = "/configuration/system/wifi";
const TEMPORARY: &str = "/configuration/system/.wifi-new";

pub(super) async fn load(file_system: &VirtualFileSystem, task: TaskIdentifier) -> Result<Profile> {
    let mut file = File::open(file_system, task, PATH, AccessFlags::Read.into()).await?;
    let mut contents = SecretBuffer([0; MAXIMUM_SIZE + 1]);
    let result = async {
        let statistics = file.get_statistics().await?;
        if statistics.kind != Kind::File
            || statistics.user.as_u16() != 0
            || !statistics.permissions.get_group().is_empty()
            || !statistics.permissions.get_others().is_empty()
        {
            return Err(IoErrorKind::PermissionDenied);
        }
        let mut length = 0;
        while length < contents.0.len() {
            let received = file.read(&mut contents.0[length..]).await?;
            if received == 0 {
                return Profile::parse(&contents.0[..length]);
            }
            length += received;
        }
        Err(IoErrorKind::InvalidData)
    }
    .await;
    let closed = file.close(file_system).await;
    result.and_then(|profile| closed.map(|()| profile))
}

pub(super) async fn save(
    file_system: &VirtualFileSystem,
    task: TaskIdentifier,
    station: &WifiClientConfiguration,
    address: Option<StaticAddress>,
) -> Result<()> {
    let mut contents = SecretBuffer([0; MAXIMUM_SIZE]);
    let length = Profile::encode(station, address, &mut contents.0)?;
    match file_system.remove(task, &TEMPORARY).await {
        Ok(()) | Err(IoErrorKind::NotFound) => (),
        Err(error) => return Err(error),
    }
    let mut file = File::open(
        file_system,
        task,
        TEMPORARY,
        Flags::new(
            AccessFlags::Write,
            Some(CreateFlags::CREATE_EXCLUSIVE),
            None,
        ),
    )
    .await?;
    let result = async {
        file.set_permissions(Permissions::USER_READ_WRITE).await?;
        let mut written = 0;
        while written < length {
            let count = file.write(&contents.0[written..length]).await?;
            if count == 0 {
                return Err(IoErrorKind::WriteZero);
            }
            written += count;
        }
        file.flush().await
    }
    .await;
    let closed = file.close(file_system).await;
    let result = match result.and(closed) {
        Ok(()) => file_system.rename(task, &TEMPORARY, &PATH).await,
        Err(error) => Err(error),
    };
    if result.is_err() {
        let _ = file_system.remove(task, &TEMPORARY).await;
    }
    result
}

pub(super) async fn apply_address(
    address: Option<StaticAddress>,
    interface: &mut File<'_>,
) -> Result<()> {
    if let Some(address) = address {
        interface
            .control(
                ADD_IP_ADDRESS,
                &IpCidr::new_ipv4(address.address.octets(), address.prefix),
            )
            .await?;
        interface
            .control(
                ADD_ROUTE,
                &Route::new_default_ipv4(address.gateway.octets()),
            )
            .await?;
        interface
            .control(
                ADD_DNS_SERVER,
                &IpAddress::new_ipv4(address.name_server.octets()),
            )
            .await?;
        Ok(())
    } else {
        interface.control(SET_DHCP_STATE, &true).await
    }
}

pub(super) async fn restart_dhcp(
    file_system: &VirtualFileSystem,
    task: TaskIdentifier,
) -> Result<()> {
    let mut interface =
        File::open(file_system, task, INTERFACE_PATH, AccessFlags::Write.into()).await?;
    let result = async {
        interface.control(SET_DHCP_STATE, &false).await?;
        apply_address(None, &mut interface).await
    }
    .await;
    let closed = interface.close(file_system).await;
    result.and(closed)
}

pub(super) async fn switch_to_dhcp(
    file_system: &VirtualFileSystem,
    task: TaskIdentifier,
    address: StaticAddress,
) -> Result<()> {
    let mut interface =
        File::open(file_system, task, INTERFACE_PATH, AccessFlags::Write.into()).await?;
    let result = async {
        let gateway = Route::new_default_ipv4(address.gateway.octets());
        for index in 0..interface.control(GET_ROUTE_COUNT, &()).await? {
            if interface.control(GET_ROUTE, &index).await? == gateway {
                interface.control(REMOVE_ROUTE, &index).await?;
                break;
            }
        }
        let name_server = IpAddress::new_ipv4(address.name_server.octets());
        for index in 0..interface.control(GET_DNS_SERVER_COUNT, &()).await? {
            if interface.control(GET_DNS_SERVER, &index).await? == name_server {
                interface.control(REMOVE_DNS_SERVER, &index).await?;
                break;
            }
        }
        let address = IpCidr::new_ipv4(address.address.octets(), address.prefix);
        for index in 0..interface.control(GET_IP_ADDRESS_COUNT, &()).await? {
            if interface.control(GET_IP_ADDRESS, &index).await? == address {
                interface.control(REMOVE_IP_ADDRESS, &index).await?;
                break;
            }
        }
        if !interface.control(GET_DHCP_STATE, &()).await? {
            apply_address(None, &mut interface).await?;
        }
        Ok(())
    }
    .await;
    let closed = interface.close(file_system).await;
    result.and(closed)
}

struct SecretBuffer<const LENGTH: usize>([u8; LENGTH]);

impl<const LENGTH: usize> Drop for SecretBuffer<LENGTH> {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // SAFETY: each byte is exclusively borrowed and remains live for the write.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
    }
}
