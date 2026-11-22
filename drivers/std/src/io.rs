use file_system::IoErrorKind;
use std::io;

/// Map a host I/O error to the closest Xila kind with the shared Core mapping. The host file
/// system driver refines it with the raw OS error where that carries more precision.
pub fn map_error(error: io::Error) -> IoErrorKind {
    io_error::map_std_error(error)
}
