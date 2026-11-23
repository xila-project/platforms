use core::{fmt::Write, net::Ipv4Addr};

use file_system::{IoErrorKind, Result};
use network::{WifiAuthentication, WifiClientConfiguration};

pub(super) const MAXIMUM_SIZE: usize = 160;

pub(super) struct Profile {
    pub station: WifiClientConfiguration,
    pub address: Option<StaticAddress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StaticAddress {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
    pub name_server: Ipv4Addr,
}

impl Profile {
    pub fn parse(contents: &[u8]) -> Result<Self> {
        if contents.len() > MAXIMUM_SIZE {
            return Err(IoErrorKind::InvalidData);
        }
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
        validate_station(&station)?;
        let address = if let Some(address) = lines.next() {
            let address = core::str::from_utf8(address).map_err(|_| IoErrorKind::InvalidData)?;
            let (address, prefix) = address.split_once('/').ok_or(IoErrorKind::InvalidData)?;
            let prefix = prefix.parse::<u8>().map_err(|_| IoErrorKind::InvalidData)?;
            if prefix > 32 {
                return Err(IoErrorKind::InvalidData);
            }
            let address = StaticAddress {
                address: parse_address(Some(address.as_bytes()))?,
                prefix,
                gateway: parse_address(lines.next())?,
                name_server: parse_address(lines.next())?,
            };
            if lines.next().is_some() {
                return Err(IoErrorKind::InvalidData);
            }
            Some(address)
        } else {
            None
        };
        Ok(Self { station, address })
    }

    pub fn encode(
        station: &WifiClientConfiguration,
        address: Option<StaticAddress>,
        buffer: &mut [u8; MAXIMUM_SIZE],
    ) -> Result<usize> {
        validate_station(station)?;
        let ssid = station.ssid.as_bytes()?;
        let password = station.credential.as_bytes()?;
        let mut writer = BufferWriter { buffer, length: 0 };
        writer.append(ssid)?;
        writer.append(b"\n")?;
        writer.append(password)?;
        writer.append(b"\n")?;
        if let Some(address) = address {
            writeln!(
                writer,
                "{}/{}\n{}\n{}",
                address.address, address.prefix, address.gateway, address.name_server,
            )
            .map_err(|_| IoErrorKind::InvalidData)?;
        }
        Ok(writer.length)
    }
}

pub(super) fn validate_station(station: &WifiClientConfiguration) -> Result<()> {
    station.validate()?;
    if station.channel > 14
        || station
            .ssid
            .as_bytes()?
            .iter()
            .any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
    {
        return Err(IoErrorKind::InvalidArgument);
    }
    if !matches!(
        station.authentication,
        WifiAuthentication::OPEN | WifiAuthentication::WPA2_PERSONAL
    ) {
        return Err(IoErrorKind::Unsupported);
    }
    Ok(())
}

fn parse_address(line: Option<&[u8]>) -> Result<Ipv4Addr> {
    let line = core::str::from_utf8(line.ok_or(IoErrorKind::InvalidData)?)
        .map_err(|_| IoErrorKind::InvalidData)?;
    let address = line
        .parse::<Ipv4Addr>()
        .map_err(|_| IoErrorKind::InvalidData)?;
    if address.is_unspecified() || address.is_multicast() || address.is_broadcast() {
        return Err(IoErrorKind::InvalidData);
    }
    Ok(address)
}

struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

impl BufferWriter<'_> {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self.length + bytes.len();
        self.buffer
            .get_mut(self.length..end)
            .ok_or(IoErrorKind::InvalidData)?
            .copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }
}

impl Write for BufferWriter<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.append(value.as_bytes()).map_err(|_| core::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_network_with_crlf() {
        let profile = Profile::parse(b"X\r\n\r\n").unwrap();
        assert_eq!(profile.station.authentication, WifiAuthentication::OPEN);
        assert!(profile.address.is_none());
    }

    #[test]
    fn round_trips_maximum_credentials_and_static_ipv4() {
        let name = [b'X'; 32];
        let secret = [b'0'; 64];
        let station =
            WifiClientConfiguration::new(&name, WifiAuthentication::WPA2_PERSONAL, &secret)
                .unwrap();
        let address = StaticAddress {
            address: Ipv4Addr::new(192, 0, 2, 10),
            prefix: 24,
            gateway: Ipv4Addr::new(192, 0, 2, 1),
            name_server: Ipv4Addr::new(192, 0, 2, 2),
        };
        let mut buffer = [0; MAXIMUM_SIZE];
        let length = Profile::encode(&station, Some(address), &mut buffer).unwrap();
        let profile = Profile::parse(&buffer[..length]).unwrap();
        assert!(profile.station.ssid.as_bytes().unwrap() == name);
        assert!(profile.station.credential.as_bytes().unwrap() == secret);
        assert_eq!(profile.address, Some(address));
    }

    #[test]
    fn rejects_truncated_oversized_and_invalid_profiles() {
        for fixture in [
            b"".as_slice(),
            b"X\n\n192.0.2.10/24\n",
            b"X\n\n192.0.2.10/33\n192.0.2.1\n192.0.2.2\n",
            b"X\n\n192.0.2.10/24\n0.0.0.0\n192.0.2.2\n",
            &[b'X'; MAXIMUM_SIZE + 1],
        ] {
            assert!(Profile::parse(fixture).is_err());
        }
    }
}
