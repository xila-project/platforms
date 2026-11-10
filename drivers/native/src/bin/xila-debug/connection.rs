//! One deadline covers connect, request, response header and screenshot payload.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Instant,
};
pub(super) struct Connection {
    stream: UnixStream,
    deadline: Instant,
}
impl Connection {
    pub fn new(endpoint: &Path, deadline: Instant) -> io::Result<Self> {
        Ok(Self {
            stream: super::wait::connect(endpoint, deadline)?,
            deadline,
        })
    }
}
impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            super::wait::ready(&self.stream, libc::POLLIN, self.deadline)?;
            match self.stream.read(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            super::wait::ready(&self.stream, libc::POLLOUT, self.deadline)?;
            match self.stream.write(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
