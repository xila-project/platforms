use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations,
};
use network::{GET_KIND, InterfaceKind};
use shared::AnyByLayout;

pub struct TunTapControllerDevice;

impl DirectBaseOperations for TunTapControllerDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &mut [u8],
        _: file_system::Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(IoErrorKind::Unsupported))
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &[u8],
        _: file_system::Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(IoErrorKind::Unsupported))
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        _: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| match command {
            GET_KIND::IDENTIFIER => {
                let kind = GET_KIND::cast_output(output)?;
                *kind = InterfaceKind::Ethernet as u8;
                Ok(())
            }
            _ => Err(IoErrorKind::Unsupported),
        })
    }
}

impl MountOperations for TunTapControllerDevice {}

impl DirectCharacterDevice for TunTapControllerDevice {}
