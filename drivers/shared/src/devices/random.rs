use file_system::{DirectBaseOperations, DirectCharacterDevice, MountOperations, Size};

pub struct RandomDevice;

impl DirectBaseOperations for RandomDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            getrandom::fill(buffer).map_err(|_| io_error::Kind::InputOutput)?;

            Ok(buffer.len())
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &[u8],
        _: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| Err(io_error::Kind::Unsupported))
    }

    fn set_position(
        &self,
        _: Size,
        _position: &file_system::Position,
    ) -> io_error::Result<file_system::Size> {
        Err(io_error::Kind::Unsupported)
    }
}

impl MountOperations for RandomDevice {}

impl DirectCharacterDevice for RandomDevice {}
