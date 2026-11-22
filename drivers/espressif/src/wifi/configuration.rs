use core::net::Ipv4Addr;

use file_system::{AccessFlags, IoErrorKind, Kind, Result};
use network::{
    ADD_DNS_SERVER, ADD_IP_ADDRESS, ADD_ROUTE, IpAddress, IpCidr, Route, SET_DHCP_STATE,
    WifiAuthentication, WifiClientConfiguration,
};
use task::TaskIdentifier;
use virtual_file_system::{File, VirtualFileSystem};

pub const PATH: &str = "/configuration/system/wifi";

pub(super) struct Configuration {
    pub station: WifiClientConfiguration,
    address: Option<StaticAddress>,
}

struct StaticAddress {
    address: IpCidr,
    gateway: Route,
    name_server: IpAddress,
}

impl Configuration {
    pub async fn load(file_system: &VirtualFileSystem, task: TaskIdentifier) -> Result<Self> {
        let mut file = File::open(file_system, task, PATH, AccessFlags::Read.into()).await?;
        // Sized for both credential lines, three IPv4 settings and CRLF terminators.
        let mut contents = [0; 161];
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
            loop {
                if length == contents.len() {
                    return Err(IoErrorKind::InvalidData);
                }
                let received = file.read(&mut contents[length..]).await?;
                if received == 0 {
                    return Self::parse(&contents[..length]);
                }
                length += received;
            }
        }
        .await;
        // The file buffer may contain a passphrase, including on a parse failure.
        for byte in &mut contents {
            // SAFETY: each byte is exclusively borrowed and remains live for the write.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        let closed = file.close(file_system).await;
        result.and_then(|configuration| closed.map(|()| configuration))
    }

    fn parse(contents: &[u8]) -> Result<Self> {
        let mut lines = contents
            .strip_suffix(b"\n")
            .unwrap_or(contents)
            .split(|byte| *byte == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
        let ssid = lines.next().ok_or(IoErrorKind::InvalidData)?;
        let password = lines.next().ok_or(IoErrorKind::InvalidData)?;
        let authentication = if password.is_empty() {
            WifiAuthentication::OPEN
        } else {
            WifiAuthentication::WPA2_PERSONAL
        };
        let station = WifiClientConfiguration::new(ssid, authentication, password)?;
        let address = if let Some(address) = lines.next() {
            let address = core::str::from_utf8(address).map_err(|_| IoErrorKind::InvalidData)?;
            let (address, prefix) = address.split_once('/').ok_or(IoErrorKind::InvalidData)?;
            let address = address
                .parse::<Ipv4Addr>()
                .map_err(|_| IoErrorKind::InvalidData)?;
            let prefix = prefix.parse::<u8>().map_err(|_| IoErrorKind::InvalidData)?;
            if prefix > 32
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
            {
                return Err(IoErrorKind::InvalidData);
            }
            let gateway = parse_address(lines.next())?;
            let name_server = parse_address(lines.next())?;
            if lines.next().is_some() {
                return Err(IoErrorKind::InvalidData);
            }
            Some(StaticAddress {
                address: IpCidr::new_ipv4(address.octets(), prefix),
                gateway: Route::new_default_ipv4(gateway),
                name_server: IpAddress::new_ipv4(name_server),
            })
        } else {
            None
        };
        Ok(Self { station, address })
    }

    pub async fn apply_address(&self, interface: &mut File<'_>) -> Result<()> {
        if let Some(address) = &self.address {
            interface.control(ADD_IP_ADDRESS, &address.address).await?;
            interface.control(ADD_ROUTE, &address.gateway).await?;
            interface
                .control(ADD_DNS_SERVER, &address.name_server)
                .await?;
            Ok(())
        } else {
            interface.control(SET_DHCP_STATE, &true).await
        }
    }
}

fn parse_address(line: Option<&[u8]>) -> Result<[u8; 4]> {
    let line = core::str::from_utf8(line.ok_or(IoErrorKind::InvalidData)?)
        .map_err(|_| IoErrorKind::InvalidData)?;
    let address = line
        .parse::<Ipv4Addr>()
        .map_err(|_| IoErrorKind::InvalidData)?;
    if address.is_unspecified() || address.is_multicast() || address.is_broadcast() {
        return Err(IoErrorKind::InvalidData);
    }
    Ok(address.octets())
}
