use file_system::IoErrorKind;
use std::io;

/// Map a host I/O error to the closest Xila kind. Prefer the raw OS error when it carries more
/// precision than `std::io::ErrorKind` (see the host file system driver for the full table).
pub fn map_error(error: io::Error) -> IoErrorKind {
    match error.kind() {
        io::ErrorKind::PermissionDenied => IoErrorKind::PermissionDenied,
        io::ErrorKind::NotFound => IoErrorKind::NotFound,
        io::ErrorKind::AlreadyExists => IoErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput => IoErrorKind::InvalidArgument,
        io::ErrorKind::InvalidData => IoErrorKind::InvalidData,
        io::ErrorKind::TimedOut => IoErrorKind::TimedOut,
        io::ErrorKind::WouldBlock => IoErrorKind::WouldBlock,
        io::ErrorKind::Interrupted => IoErrorKind::Interrupted,
        io::ErrorKind::WriteZero => IoErrorKind::WriteZero,
        io::ErrorKind::OutOfMemory => IoErrorKind::OutOfMemory,
        io::ErrorKind::Unsupported => IoErrorKind::Unsupported,
        _ => IoErrorKind::InputOutput,
    }
}
