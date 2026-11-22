//! Capability-scoped view of a native host directory.
//!
//! `HostFileSystem` acquires ambient authority exactly once, when constructed.
//! All later namespace operations are relative to the open `cap_std::fs::Dir`.
//! `cap-std` provides a portable directory-capability model for Linux/macOS/Windows.
//! On Linux (kernel 5.6+ with `openat2` and procfs mounted), file opens pin the
//! target with `O_PATH` before opening it, so device nodes are rejected without
//! invoking a host driver. Xila paths and returned names are UTF-8;
//! unrepresentable host names are omitted from directory enumeration. Final
//! symlinks are rejected for file opens on Unix, while directory resolution may
//! follow links only within the root.
//!
//! On Linux, the filesystem also implements the directory-entry operations of
//! [`FileSystemOperations`]. The virtual file system then walks a mount one
//! component at a time with pinned directory descriptors and asks this backend
//! only about a single entry name inside an already-opened directory. Entry
//! operations never follow a symbolic link in that final entry (`O_NOFOLLOW`,
//! `AT_SYMLINK_NOFOLLOW`, `unlinkat`, `renameat`, `linkat`) and never resolve a
//! parent path again, so replacing an ancestor directory with a link after the
//! virtual file system has walked it cannot redirect the operation. This is what
//! allows `supports_confined_namespace` to hold for WASI preopens on host
//! mounts. Other hosts keep the path-based operations, and confined lookups that
//! enter a host mount there fail with `PathEscapesRoot`.

#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, AsRawFd};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path as StdPath, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(any(unix, windows))]
use cap_std::fs::MetadataExt;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, ReadDir},
    time::{SystemClock, SystemTime},
};
use file_system::{
    AccessFlags, AttributeFlags, AttributeOperations, Attributes, BaseOperations, Context,
    CreateFlags, DirectoryOperations, Entry, FileOperations, FileSystemOperations, Flags,
    IoErrorKind, Kind, MountOperations, Path, Permissions, Position, Result, Size, Time,
};

/// Options controlling a host directory mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFileSystemOptions {
    /// Reject all operations that can modify host state.
    pub read_only: bool,
    /// Synthetic Xila owner; never translated to a host UID.
    pub user: users::UserIdentifier,
    /// Synthetic Xila group; never translated to a host GID.
    pub group: users::GroupIdentifier,
    /// Synthetic permissions reported for regular files.
    pub file_permissions: Permissions,
    /// Synthetic permissions reported for directories.
    pub directory_permissions: Permissions,
}

impl Default for HostFileSystemOptions {
    fn default() -> Self {
        Self {
            read_only: false,
            user: users::UserIdentifier::ROOT,
            group: users::GroupIdentifier::ROOT,
            file_permissions: Permissions::FILE_DEFAULT,
            directory_permissions: Permissions::DIRECTORY_DEFAULT,
        }
    }
}

/// A filesystem view confined to one host directory capability.
pub struct HostFileSystem {
    root: Mutex<Option<Arc<Dir>>>,
    // Used only to reopen a verified O_PATH descriptor. The descriptor number
    // comes from our pinned file handle, never from an Xila path.
    #[cfg(target_os = "linux")]
    proc_fd: Dir,
    options: HostFileSystemOptions,
}

struct FileHandle(Arc<Mutex<cap_std::fs::File>>);

struct DirectoryHandle {
    directory: Dir,
    /// Created on the first read, so directories opened only to walk a path
    /// do not allocate a host directory stream.
    entries: Option<ReadDir>,
    position: Size,
}

impl DirectoryHandle {
    fn new(directory: Dir) -> Self {
        Self {
            directory,
            entries: None,
            position: 0,
        }
    }

    fn entries(&mut self) -> Result<&mut ReadDir> {
        if self.entries.is_none() {
            self.entries = Some(self.directory.entries().map_err(map_io_error)?);
        }
        self.entries.as_mut().ok_or(IoErrorKind::InputOutput)
    }
}

impl HostFileSystem {
    /// Open a host directory and retain its capability for this filesystem's lifetime.
    pub fn open(path: impl AsRef<StdPath>, options: HostFileSystemOptions) -> Result<Self> {
        let root_path = path.as_ref().to_path_buf();
        let root = Dir::open_ambient_dir(&root_path, ambient_authority()).map_err(map_io_error)?;
        #[cfg(target_os = "linux")]
        let proc_fd =
            Dir::open_ambient_dir("/proc/self/fd", ambient_authority()).map_err(map_io_error)?;
        Ok(Self {
            root: Mutex::new(Some(Arc::new(root))),
            #[cfg(target_os = "linux")]
            proc_fd,
            options,
        })
    }

    fn root(&self) -> Result<Arc<Dir>> {
        self.root
            .lock()
            .map_err(|_| IoErrorKind::InputOutput)?
            .as_ref()
            .cloned()
            .ok_or(IoErrorKind::DeviceUnavailable)
    }

    fn relative_path(path: &Path) -> Result<PathBuf> {
        let source = path.as_str();
        let mut result = PathBuf::new();
        for component in StdPath::new(source).components() {
            match component {
                Component::RootDir => {}
                Component::CurDir => {}
                Component::Normal(part) => result.push(part),
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(IoErrorKind::InvalidPath);
                }
            }
        }
        Ok(if result.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            result
        })
    }

    fn writable(&self) -> Result<()> {
        if self.options.read_only {
            Err(IoErrorKind::ReadOnlyFileSystem)
        } else {
            Ok(())
        }
    }

    /// The size requested by a path or entry attribute update, the only field
    /// a host file accepts. `None` means the update is an empty no-op.
    fn writable_size(attributes: &Attributes) -> Result<Option<Size>> {
        let mask = attributes.get_mask();
        if mask.is_empty() {
            return Ok(None);
        }
        if mask != AttributeFlags::Size {
            return Err(IoErrorKind::Unsupported);
        }
        attributes
            .get_size()
            .copied()
            .map(Some)
            .ok_or(IoErrorKind::InvalidArgument)
    }

    /// The pinned host directory of a parent context opened by this filesystem.
    #[cfg(target_os = "linux")]
    fn entry_parent(context: &Context) -> Result<&Dir> {
        context
            .get_private_data_of_type::<DirectoryHandle>()
            .map(|handle| &handle.directory)
            .ok_or(IoErrorKind::InvalidState)
    }

    /// Open the child directory `name` of the pinned `parent` without
    /// following a link in `name`: Linux reports a link there as not a
    /// directory. `O_DIRECTORY` also keeps device nodes closed, since the
    /// kernel checks it before calling a driver.
    #[cfg(target_os = "linux")]
    fn open_directory_entry(parent: &Dir, name: &StdPath) -> Result<Dir> {
        let descriptor = rustix::fs::openat(
            parent.as_fd(),
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| map_io_error(error.into()))?;
        Ok(Dir::from_std_file(std::fs::File::from(descriptor)))
    }

    fn open_options(&self) -> OpenOptions {
        let mut options = OpenOptions::new();
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOCTTY);
        options
    }

    /// Open a verified regular file. On Linux, O_PATH pins the final object
    /// without invoking a device driver's open method. We then reopen only
    /// that pinned descriptor through procfs, avoiding a pathname race.
    #[cfg(target_os = "linux")]
    fn open_regular_file(
        &self,
        directory: &Dir,
        relative: &StdPath,
        read: bool,
        write: bool,
        truncate: bool,
    ) -> Result<cap_std::fs::File> {
        let pinned = rustix::fs::openat2(
            directory.as_fd(),
            relative,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|error| match error {
            // RESOLVE_BENEATH reports a path leaving the mounted root as EXDEV: it is an
            // escape from the mounted root, not a cross-device operation.
            rustix::io::Errno::XDEV => IoErrorKind::PathEscapesRoot,
            error => map_io_error(error.into()),
        })?;
        let pinned = cap_std::fs::File::from_std(std::fs::File::from(pinned));
        let metadata = pinned.metadata().map_err(map_io_error)?;
        if !metadata.is_file() {
            return Err(if metadata.is_dir() {
                IoErrorKind::IsADirectory
            } else if metadata.file_type().is_symlink() {
                // `O_PATH | O_NOFOLLOW` pins a final link itself; report it
                // like `O_NOFOLLOW` does.
                IoErrorKind::SymlinkLoop
            } else {
                IoErrorKind::Unsupported
            });
        }

        let access = match (read, write) {
            (true, true) => rustix::fs::OFlags::RDWR,
            (false, true) => rustix::fs::OFlags::WRONLY,
            (true, false) => rustix::fs::OFlags::RDONLY,
            (false, false) => return Err(IoErrorKind::InvalidFlags),
        };
        let mut flags = access | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK;
        if truncate {
            flags |= rustix::fs::OFlags::TRUNC;
        }
        let descriptor_name = pinned.as_fd().as_raw_fd().to_string();
        let descriptor = rustix::fs::openat(
            &self.proc_fd,
            descriptor_name.as_str(),
            flags,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| map_io_error(error.into()))?;
        Ok(cap_std::fs::File::from_std(std::fs::File::from(descriptor)))
    }

    #[cfg(target_os = "linux")]
    fn open_new_regular_file(
        &self,
        directory: &Dir,
        relative: &StdPath,
        read: bool,
        write: bool,
    ) -> Result<cap_std::fs::File> {
        let mut options = self.open_options();
        options.read(read).write(write).create_new(true);
        directory
            .open_with(relative, &options)
            .map_err(map_io_error)
    }

    #[cfg(not(target_os = "linux"))]
    fn open_regular_file(
        &self,
        directory: &Dir,
        relative: &StdPath,
        read: bool,
        write: bool,
        truncate: bool,
    ) -> Result<cap_std::fs::File> {
        let mut options = self.open_options();
        options.read(read).write(write).truncate(truncate);
        let file = directory
            .open_with(relative, &options)
            .map_err(map_io_error)?;
        let metadata = file.metadata().map_err(map_io_error)?;
        if !metadata.is_file() {
            return Err(if metadata.is_dir() {
                IoErrorKind::IsADirectory
            } else {
                IoErrorKind::Unsupported
            });
        }
        Ok(file)
    }

    /// Open the regular file `relative` beneath `directory` for `flags`.
    fn open_file(
        &self,
        directory: &Dir,
        relative: &StdPath,
        flags: Flags,
    ) -> Result<cap_std::fs::File> {
        let access = flags.get_access();
        let create = flags.get_create();
        let write = access.contains(AccessFlags::Write);
        if !access.contains(AccessFlags::Read) && !write {
            return Err(IoErrorKind::InvalidFlags);
        }
        if create.contains(CreateFlags::Exclusive) && !create.contains(CreateFlags::Create) {
            return Err(IoErrorKind::InvalidFlags);
        }
        if create.contains(CreateFlags::Create) && !write {
            return Err(IoErrorKind::InvalidFlags);
        }
        if write {
            self.writable()?;
        }
        let read = access.contains(AccessFlags::Read);
        let truncate = create.contains(CreateFlags::Truncate);
        if truncate && !write {
            return Err(IoErrorKind::InvalidFlags);
        }
        #[cfg(target_os = "linux")]
        let file = if create.contains(CreateFlags::Exclusive) {
            self.open_new_regular_file(directory, relative, read, write)?
        } else {
            match self.open_regular_file(directory, relative, read, write, truncate) {
                Ok(file) => file,
                Err(IoErrorKind::NotFound) if create.contains(CreateFlags::Create) => {
                    match self.open_new_regular_file(directory, relative, read, write) {
                        Err(IoErrorKind::AlreadyExists) => {
                            self.open_regular_file(directory, relative, read, write, truncate)?
                        }
                        result => result?,
                    }
                }
                Err(error) => return Err(error),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let file = {
            let mut options = self.open_options();
            options.read(read).write(write);
            options.create(create.contains(CreateFlags::Create));
            options.create_new(create.contains(CreateFlags::Exclusive));
            options.truncate(truncate);
            let file = directory
                .open_with(relative, &options)
                .map_err(map_io_error)?;
            let metadata = file.metadata().map_err(map_io_error)?;
            if !metadata.is_file() {
                return Err(if metadata.is_dir() {
                    IoErrorKind::IsADirectory
                } else {
                    IoErrorKind::Unsupported
                });
            }
            file
        };
        Ok(file)
    }

    fn metadata(&self, path: &Path) -> Result<cap_std::fs::Metadata> {
        let path = Self::relative_path(path)?;
        let metadata = self.root()?.symlink_metadata(&path).map_err(map_io_error)?;
        if !metadata.is_file() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
            return Err(IoErrorKind::Unsupported);
        }
        Ok(metadata)
    }

    fn populate_attributes(&self, metadata: &cap_std::fs::Metadata, attributes: &mut Attributes) {
        let requested = attributes.get_mask();
        let mut result = Attributes::new();
        let kind = if metadata.file_type().is_symlink() {
            Some(Kind::SymbolicLink)
        } else if metadata.is_file() {
            Some(Kind::File)
        } else if metadata.is_dir() {
            Some(Kind::Directory)
        } else {
            None
        };

        if requested.contains(AttributeFlags::Kind)
            && let Some(kind) = kind
        {
            result = result.set_kind(kind);
        }
        if requested.contains(AttributeFlags::Size)
            && (metadata.is_file() || metadata.file_type().is_symlink())
        {
            result = result.set_size(metadata.len());
        }
        if requested.contains(AttributeFlags::Inode)
            && let Some(inode) = stable_inode(metadata)
        {
            result = result.set_inode(inode);
        }
        if requested.contains(AttributeFlags::Links)
            && let Some(links) = link_count(metadata)
        {
            result = result.set_links(links);
        }
        if requested.contains(AttributeFlags::Permissions) {
            result = result.set_permissions(if metadata.is_dir() {
                self.options.directory_permissions
            } else {
                self.options.file_permissions
            });
        }
        if requested.contains(AttributeFlags::User) {
            result = result.set_user(self.options.user);
        }
        if requested.contains(AttributeFlags::Group) {
            result = result.set_group(self.options.group);
        }
        if requested.contains(AttributeFlags::ModificationTime)
            && let Some(time) = metadata.modified().ok().and_then(system_time)
        {
            result = result.set_modification(time);
        }
        if requested.contains(AttributeFlags::CreationTime)
            && let Some(time) = metadata.created().ok().and_then(system_time)
        {
            result = result.set_creation(time);
        }
        if requested.contains(AttributeFlags::AccessTime)
            && let Some(time) = metadata.accessed().ok().and_then(system_time)
        {
            result = result.set_access(time);
        }
        #[cfg(unix)]
        if requested.contains(AttributeFlags::StatusTime) {
            let seconds = MetadataExt::ctime(metadata);
            if let Ok(seconds) = u64::try_from(seconds) {
                result = result.set_status(Time::new(seconds));
            }
        }
        *attributes = result;
    }

    fn get_path_attributes(&self, path: &Path, attributes: &mut Attributes) -> Result<()> {
        if attributes.get_mask().is_empty() {
            return Ok(());
        }
        let metadata = self.metadata(path)?;
        self.populate_attributes(&metadata, attributes);
        Ok(())
    }
}

/// Check a single directory entry name handed over by the virtual file system:
/// it must name one entry of an opened directory, never a path to resolve.
#[cfg(target_os = "linux")]
fn entry_name(name: &str) -> Result<&StdPath> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(IoErrorKind::InvalidPath);
    }
    Ok(StdPath::new(name))
}

/// Metadata of the entry `name` itself, without following a link, limited to
/// the kinds this filesystem exposes.
#[cfg(target_os = "linux")]
fn entry_metadata(parent: &Dir, name: &StdPath) -> Result<cap_std::fs::Metadata> {
    let metadata = parent.symlink_metadata(name).map_err(map_io_error)?;
    if !metadata.is_file() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Err(IoErrorKind::Unsupported);
    }
    Ok(metadata)
}

/// Check a link target before storing it on the host.
fn symlink_target(target: &Path) -> Result<PathBuf> {
    if target.get_length() > 4096 {
        return Err(IoErrorKind::NameTooLong);
    }
    if !target.is_valid() || target.is_empty() {
        return Err(IoErrorKind::InvalidPath);
    }
    if target.is_absolute() {
        // The VFS converts absolute Xila targets into relative namespace
        // paths before calling this native adapter. An unadapted absolute
        // target is ambiguous with a host-absolute link and is rejected.
        return Err(IoErrorKind::Unsupported);
    }
    Ok(StdPath::new(target.as_str()).to_path_buf())
}

/// Copy a stored link target into `buffer`.
fn copy_symlink_target(target: &StdPath, buffer: &mut [u8]) -> Result<usize> {
    if target.is_absolute() {
        // An absolute target in a pre-existing host tree belongs to the
        // host namespace and cannot safely be exposed as an Xila target.
        return Err(IoErrorKind::CorruptData);
    }
    let target = target.to_str().ok_or(IoErrorKind::CorruptData)?.as_bytes();
    if target.len() > buffer.len() {
        return Err(IoErrorKind::NameTooLong);
    }
    buffer[..target.len()].copy_from_slice(target);
    Ok(target.len())
}

fn stable_inode(metadata: &cap_std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        Some(MetadataExt::ino(metadata))
    }
    #[cfg(windows)]
    {
        MetadataExt::file_index(metadata)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        None
    }
}

fn link_count(metadata: &cap_std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        Some(MetadataExt::nlink(metadata))
    }
    #[cfg(windows)]
    {
        MetadataExt::number_of_links(metadata).map(u64::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        None
    }
}

/// The modification time as `Entry` expects it: `0` when the host doesn't report one.
fn modification_time(metadata: &cap_std::fs::Metadata) -> Time {
    metadata
        .modified()
        .ok()
        .and_then(system_time)
        .unwrap_or(Time::new(0))
}

fn system_time(value: SystemTime) -> Option<Time> {
    value
        .duration_since(SystemClock::UNIX_EPOCH)
        .ok()
        .map(Time::from)
}

fn map_io_error(error: std::io::Error) -> IoErrorKind {
    // cap-std reports a path that leaves the mounted root as a `PermissionDenied` error without
    // an OS error code, which a genuine EACCES from the host always carries.
    if error.kind() == std::io::ErrorKind::PermissionDenied && error.raw_os_error().is_none() {
        return IoErrorKind::PathEscapesRoot;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error()
        && let Some(kind) = map_errno(code)
    {
        return kind;
    }
    map_io_error_kind(error.kind())
}

/// Map a host `errno` to the I/O kind that keeps its meaning. Returns `None` for values the
/// portable `std::io::ErrorKind` mapping already handles.
#[cfg(unix)]
fn map_errno(code: i32) -> Option<IoErrorKind> {
    use libc::*;
    Some(match code {
        ELOOP => IoErrorKind::SymlinkLoop,
        EXDEV => IoErrorKind::CrossDevice,
        ENOSYS => IoErrorKind::NotImplemented,
        // EOPNOTSUPP is the same value as ENOTSUP on Linux.
        ENOTSUP => IoErrorKind::Unsupported,
        EMFILE => IoErrorKind::ProcessFileLimit,
        ENFILE => IoErrorKind::SystemFileLimit,
        ENAMETOOLONG => IoErrorKind::NameTooLong,
        EBADF => IoErrorKind::BadDescriptor,
        EINVAL => IoErrorKind::InvalidArgument,
        ESPIPE => IoErrorKind::InvalidSeek,
        EROFS => IoErrorKind::ReadOnlyFileSystem,
        EPERM => IoErrorKind::OperationNotPermitted,
        ESTALE => IoErrorKind::StaleHandle,
        ENOSPC | EDQUOT => IoErrorKind::NoSpace,
        ENOMEM => IoErrorKind::OutOfMemory,
        EINTR => IoErrorKind::Interrupted,
        // EWOULDBLOCK is the same value as EAGAIN on Linux.
        EAGAIN => IoErrorKind::WouldBlock,
        EBUSY | ETXTBSY => IoErrorKind::ResourceBusy,
        ENODEV | ENXIO => IoErrorKind::DeviceUnavailable,
        EFBIG => IoErrorKind::FileTooLarge,
        ETIMEDOUT => IoErrorKind::TimedOut,
        _ => return None,
    })
}

fn map_io_error_kind(kind: std::io::ErrorKind) -> IoErrorKind {
    use std::io::ErrorKind as Io;
    match kind {
        Io::NotFound => IoErrorKind::NotFound,
        Io::AlreadyExists => IoErrorKind::AlreadyExists,
        Io::PermissionDenied => IoErrorKind::PermissionDenied,
        Io::ReadOnlyFilesystem => IoErrorKind::ReadOnlyFileSystem,
        Io::NotADirectory => IoErrorKind::NotADirectory,
        Io::IsADirectory => IoErrorKind::IsADirectory,
        Io::DirectoryNotEmpty => IoErrorKind::DirectoryNotEmpty,
        Io::InvalidInput | Io::InvalidFilename => IoErrorKind::InvalidPath,
        Io::InvalidData => IoErrorKind::InvalidData,
        Io::WouldBlock => IoErrorKind::WouldBlock,
        Io::ResourceBusy | Io::ExecutableFileBusy => IoErrorKind::ResourceBusy,
        Io::Unsupported => IoErrorKind::Unsupported,
        Io::CrossesDevices => IoErrorKind::CrossDevice,
        Io::NotSeekable => IoErrorKind::InvalidSeek,
        Io::Interrupted => IoErrorKind::Interrupted,
        Io::OutOfMemory => IoErrorKind::OutOfMemory,
        Io::StorageFull | Io::QuotaExceeded => IoErrorKind::NoSpace,
        Io::FileTooLarge => IoErrorKind::FileTooLarge,
        Io::TimedOut => IoErrorKind::TimedOut,
        _ => IoErrorKind::InputOutput,
    }
}

impl FileSystemOperations for HostFileSystem {
    fn writable_attributes(&self) -> AttributeFlags {
        AttributeFlags::Size
    }

    fn supports_confined_namespace(&self) -> bool {
        self.supports_directory_entry_operations()
    }

    fn supports_directory_entry_operations(&self) -> bool {
        cfg!(target_os = "linux")
    }

    #[cfg(target_os = "linux")]
    fn lookup_directory_entry(
        &self,
        parent: &Context,
        name: &str,
        context: &mut Context,
    ) -> Result<()> {
        let directory = Self::open_directory_entry(Self::entry_parent(parent)?, entry_name(name)?)?;
        context.set_private_data(Box::new(DirectoryHandle::new(directory)));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn lookup_file_entry(
        &self,
        parent: &Context,
        name: &str,
        context: &mut Context,
        flags: Flags,
    ) -> Result<()> {
        let file = self.open_file(Self::entry_parent(parent)?, entry_name(name)?, flags)?;
        context.set_private_data(Box::new(FileHandle(Arc::new(Mutex::new(file)))));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn create_directory_entry(&self, parent: &Context, name: &str) -> Result<()> {
        self.writable()?;
        let name = entry_name(name)?;
        Self::entry_parent(parent)?
            .create_dir(name)
            .map_err(map_io_error)
    }

    #[cfg(target_os = "linux")]
    fn create_file_entry(&self, parent: &Context, name: &str) -> Result<()> {
        self.writable()?;
        let name = entry_name(name)?;
        // `O_CREAT | O_EXCL` fails on an existing entry, including a link.
        Self::entry_parent(parent)?
            .open_with(name, self.open_options().write(true).create_new(true))
            .map(|_| ())
            .map_err(map_io_error)
    }

    #[cfg(target_os = "linux")]
    fn remove_entry(&self, parent: &Context, name: &str) -> Result<()> {
        self.writable()?;
        let name = entry_name(name)?;
        let parent = Self::entry_parent(parent)?;
        let metadata = entry_metadata(parent, name)?;
        // `unlinkat` acts on the entry itself. If it is swapped for another
        // kind after this check, the removal fails instead of following it.
        if metadata.is_dir() {
            parent.remove_dir(name).map_err(map_io_error)
        } else {
            parent.remove_file(name).map_err(map_io_error)
        }
    }

    #[cfg(target_os = "linux")]
    fn rename_entry(
        &self,
        source_parent: &Context,
        source_name: &str,
        destination_parent: &Context,
        destination_name: &str,
    ) -> Result<()> {
        self.writable()?;
        let source_name = entry_name(source_name)?;
        let destination_name = entry_name(destination_name)?;
        let source_parent = Self::entry_parent(source_parent)?;
        let destination_parent = Self::entry_parent(destination_parent)?;
        entry_metadata(source_parent, source_name)?;
        source_parent
            .rename(source_name, destination_parent, destination_name)
            .map_err(map_io_error)
    }

    #[cfg(target_os = "linux")]
    fn link_entry(
        &self,
        existing_parent: &Context,
        existing_name: &str,
        new_parent: &Context,
        new_name: &str,
    ) -> Result<()> {
        self.writable()?;
        let existing_name = entry_name(existing_name)?;
        let new_name = entry_name(new_name)?;
        let existing_parent = Self::entry_parent(existing_parent)?;
        let new_parent = Self::entry_parent(new_parent)?;
        let metadata = entry_metadata(existing_parent, existing_name)?;
        if metadata.is_dir() {
            return Err(IoErrorKind::IsADirectory);
        }
        // `linkat` without `AT_SYMLINK_FOLLOW` links the entry itself.
        existing_parent
            .hard_link(existing_name, new_parent, new_name)
            .map_err(map_io_error)
    }

    #[cfg(target_os = "linux")]
    fn create_symlink_entry(&self, target: &Path, parent: &Context, name: &str) -> Result<()> {
        self.writable()?;
        let target = symlink_target(target)?;
        let name = entry_name(name)?;
        Self::entry_parent(parent)?
            .symlink(target, name)
            .map_err(map_io_error)
    }

    #[cfg(target_os = "linux")]
    fn read_symlink_entry(&self, parent: &Context, name: &str, buffer: &mut [u8]) -> Result<usize> {
        let name = entry_name(name)?;
        // The contents are returned as stored: the virtual file system resolves
        // them, so a `..` target is checked against the Xila namespace instead
        // of the host directory that holds the link.
        let target = Self::entry_parent(parent)?
            .read_link_contents(name)
            .map_err(map_io_error)?;
        copy_symlink_target(&target, buffer)
    }

    #[cfg(target_os = "linux")]
    fn get_entry_attributes(
        &self,
        parent: &Context,
        name: &str,
        attributes: &mut Attributes,
    ) -> Result<()> {
        let name = entry_name(name)?;
        if attributes.get_mask().is_empty() {
            return Ok(());
        }
        let metadata = entry_metadata(Self::entry_parent(parent)?, name)?;
        self.populate_attributes(&metadata, attributes);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn set_entry_attributes(
        &self,
        parent: &Context,
        name: &str,
        attributes: &Attributes,
    ) -> Result<()> {
        let name = entry_name(name)?;
        let Some(size) = Self::writable_size(attributes)? else {
            return Ok(());
        };
        self.writable()?;
        let file = self.open_regular_file(Self::entry_parent(parent)?, name, false, true, false)?;
        file.set_len(size).map_err(map_io_error)
    }

    fn create_symlink(&self, target: &Path, link: &Path) -> Result<()> {
        self.writable()?;
        let target = symlink_target(target)?;
        let link = Self::relative_path(link)?;
        self.root()?.symlink(target, &link).map_err(map_io_error)
    }

    fn read_symlink(&self, link: &Path, buffer: &mut [u8]) -> Result<usize> {
        let link = Self::relative_path(link)?;
        let target = self.root()?.read_link(&link).map_err(map_io_error)?;
        copy_symlink_target(&target, buffer)
    }

    fn lookup_directory(&self, context: &mut Context, path: &Path) -> Result<()> {
        let relative = Self::relative_path(path)?;
        let root = self.root()?;
        let directory = root.open_dir(&relative).map_err(map_io_error)?;
        let metadata = directory.dir_metadata().map_err(map_io_error)?;
        if !metadata.is_dir() {
            return Err(IoErrorKind::NotADirectory);
        }
        context.set_private_data(Box::new(DirectoryHandle::new(directory)));
        Ok(())
    }

    fn lookup_file(&self, context: &mut Context, path: &Path, flags: Flags) -> Result<()> {
        let relative = Self::relative_path(path)?;
        let file = self.open_file(&*self.root()?, &relative, flags)?;
        context.set_private_data(Box::new(FileHandle(Arc::new(Mutex::new(file)))));
        Ok(())
    }

    fn create_directory(&self, path: &Path) -> Result<()> {
        self.writable()?;
        let path = Self::relative_path(path)?;
        self.root()?.create_dir(&path).map_err(map_io_error)
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        self.writable()?;
        let path = Self::relative_path(path)?;
        self.root()?
            .open_with(&path, self.open_options().write(true).create_new(true))
            .map(|_| ())
            .map_err(map_io_error)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.writable()?;
        let path = Self::relative_path(path)?;
        let root = self.root()?;
        let metadata = root.symlink_metadata(&path).map_err(map_io_error)?;
        if !metadata.file_type().is_symlink() && !metadata.is_file() && !metadata.is_dir() {
            return Err(IoErrorKind::Unsupported);
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            root.remove_dir(&path).map_err(map_io_error)
        } else {
            root.remove_file(&path).map_err(map_io_error)
        }
    }

    fn rename(&self, source: &Path, destination: &Path) -> Result<()> {
        self.writable()?;
        let source = Self::relative_path(source)?;
        let destination = Self::relative_path(destination)?;
        let root = self.root()?;
        let metadata = root.symlink_metadata(&source).map_err(map_io_error)?;
        if !metadata.file_type().is_symlink() && !metadata.is_file() && !metadata.is_dir() {
            return Err(IoErrorKind::Unsupported);
        }
        root.rename(&source, &root, &destination)
            .map_err(map_io_error)
    }

    fn get_attributes(&self, path: &Path, attributes: &mut Attributes) -> Result<()> {
        self.get_path_attributes(path, attributes)
    }

    fn set_attributes(&self, path: &Path, attributes: &Attributes) -> Result<()> {
        let Some(size) = Self::writable_size(attributes)? else {
            return Ok(());
        };
        self.writable()?;
        let path = Self::relative_path(path)?;
        let file = self.open_regular_file(&*self.root()?, &path, false, true, false)?;
        file.set_len(size).map_err(map_io_error)
    }
}

impl BaseOperations for HostFileSystem {
    fn read(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        absolute_position: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| {
            let handle = context
                .get_private_data_of_type::<FileHandle>()
                .ok_or(IoErrorKind::InvalidState)?;
            let mut file = handle.0.lock().map_err(|_| IoErrorKind::InputOutput)?;
            file.seek(SeekFrom::Start(absolute_position))
                .map_err(map_io_error)?;
            file.read(buffer).map_err(map_io_error)
        })
    }

    fn write(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        absolute_position: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| {
            self.writable()?;
            let handle = context
                .get_private_data_of_type::<FileHandle>()
                .ok_or(IoErrorKind::InvalidState)?;
            let mut file = handle.0.lock().map_err(|_| IoErrorKind::InputOutput)?;
            file.seek(SeekFrom::Start(absolute_position))
                .map_err(map_io_error)?;
            file.write(buffer).map_err(map_io_error)
        })
    }

    fn set_position(
        &self,
        context: &mut Context,
        current: Size,
        position: &Position,
    ) -> Result<Size> {
        let file = context
            .get_private_data_of_type::<FileHandle>()
            .ok_or(IoErrorKind::InvalidState)?;
        let mut file = file.0.lock().map_err(|_| IoErrorKind::InputOutput)?;
        let target = match position {
            Position::Start(value) => *value,
            Position::Current(offset) => current
                .checked_add_signed(*offset)
                .ok_or(IoErrorKind::InvalidArgument)?,
            Position::End(offset) => file.seek(SeekFrom::End(*offset)).map_err(map_io_error)?,
        };
        if !matches!(position, Position::End(_)) {
            file.seek(SeekFrom::Start(target)).map_err(map_io_error)?;
        }
        Ok(target)
    }

    fn flush(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Result<()>> {
        file_system::ready(|| {
            let file = context
                .get_private_data_of_type::<FileHandle>()
                .ok_or(IoErrorKind::InvalidState)?;
            file.0
                .lock()
                .map_err(|_| IoErrorKind::InputOutput)?
                .sync_all()
                .map_err(map_io_error)
        })
    }

    fn clone_context(&self, context: &Context) -> Result<Context> {
        if let Some(handle) = context.get_private_data_of_type::<FileHandle>() {
            return Ok(Context::new(Some(FileHandle(handle.0.clone()))));
        }
        let handle = context
            .get_private_data_of_type::<DirectoryHandle>()
            .ok_or(IoErrorKind::InvalidState)?;
        let directory = handle.directory.try_clone().map_err(map_io_error)?;
        let mut clone = DirectoryHandle::new(directory);
        if handle.position > 0 {
            let entries = clone.entries()?;
            for _ in 0..handle.position {
                if entries.next().transpose().map_err(map_io_error)?.is_none() {
                    break;
                }
            }
            clone.position = handle.position;
        }
        Ok(Context::new(Some(clone)))
    }

    fn close(&self, context: &mut Context) -> Result<()> {
        if context.take_private_data_of_type::<FileHandle>().is_some() {
            Ok(())
        } else {
            Err(IoErrorKind::InvalidState)
        }
    }
}

impl FileOperations for HostFileSystem {}

impl AttributeOperations for HostFileSystem {
    fn get_attributes(&self, context: &mut Context, attributes: &mut Attributes) -> Result<()> {
        if attributes.get_mask().is_empty() {
            return Ok(());
        }
        let metadata = if let Some(file) = context.get_private_data_of_type::<FileHandle>() {
            file.0
                .lock()
                .map_err(|_| IoErrorKind::InputOutput)?
                .metadata()
                .map_err(map_io_error)?
        } else if let Some(directory) = context.get_private_data_of_type::<DirectoryHandle>() {
            directory.directory.dir_metadata().map_err(map_io_error)?
        } else {
            return Err(IoErrorKind::InvalidState);
        };
        self.populate_attributes(&metadata, attributes);
        Ok(())
    }

    fn set_attributes(&self, context: &mut Context, attributes: &Attributes) -> Result<()> {
        let mask = attributes.get_mask();
        if mask.is_empty() {
            return Ok(());
        }
        if mask != AttributeFlags::Size {
            return Err(IoErrorKind::Unsupported);
        }
        self.writable()?;
        let size = *attributes.get_size().ok_or(IoErrorKind::InvalidArgument)?;
        let Some(file) = context.get_private_data_of_type::<FileHandle>() else {
            return if context
                .get_private_data_of_type::<DirectoryHandle>()
                .is_some()
            {
                Err(IoErrorKind::IsADirectory)
            } else {
                Err(IoErrorKind::InvalidState)
            };
        };
        file.0
            .lock()
            .map_err(|_| IoErrorKind::InputOutput)?
            .set_len(size)
            .map_err(map_io_error)
    }
}

impl DirectoryOperations for HostFileSystem {
    fn read(&self, context: &mut Context) -> Result<Option<Entry>> {
        loop {
            let (entry, position, symlink_metadata) = {
                let handle = context
                    .get_private_data_mutable_of_type::<DirectoryHandle>()
                    .ok_or(IoErrorKind::InvalidState)?;
                let entry = handle.entries()?.next().transpose().map_err(map_io_error)?;
                let mut symlink_metadata = None;
                if let Some(entry) = entry.as_ref() {
                    handle.position += 1;
                    if entry.file_type().map_err(map_io_error)?.is_symlink() {
                        symlink_metadata = Some(
                            handle
                                .directory
                                .symlink_metadata(entry.file_name())
                                .map_err(map_io_error)?,
                        );
                    }
                }
                (entry, handle.position, symlink_metadata)
            };
            let Some(entry) = entry else { return Ok(None) };
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            let kind = entry.file_type().map_err(map_io_error)?;
            let file_type = if kind.is_symlink() {
                Kind::SymbolicLink
            } else if kind.is_file() {
                Kind::File
            } else if kind.is_dir() {
                Kind::Directory
            } else {
                continue;
            };
            let (inode, size, modification) = if kind.is_symlink() {
                let metadata = symlink_metadata.ok_or(IoErrorKind::InputOutput)?;
                (position, metadata.len(), modification_time(&metadata))
            } else {
                let metadata = entry.metadata().map_err(map_io_error)?;
                (
                    stable_inode(&metadata).unwrap_or(position),
                    metadata.len(),
                    modification_time(&metadata),
                )
            };
            return Ok(Some(Entry::new(inode, name, file_type, size, modification)));
        }
    }

    fn get_position(&self, context: &mut Context) -> Result<Size> {
        context
            .get_private_data_of_type::<DirectoryHandle>()
            .map(|handle| handle.position)
            .ok_or(IoErrorKind::InvalidState)
    }

    fn set_position(&self, context: &mut Context, position: Size) -> Result<()> {
        let handle = context
            .get_private_data_mutable_of_type::<DirectoryHandle>()
            .ok_or(IoErrorKind::InvalidState)?;
        handle.entries = None;
        handle.position = 0;
        if position == 0 {
            return Ok(());
        }
        let entries = handle.entries()?;
        let mut skipped = 0;
        while skipped < position {
            if entries.next().transpose().map_err(map_io_error)?.is_none() {
                break;
            }
            skipped += 1;
        }
        handle.position = skipped;
        Ok(())
    }

    fn rewind(&self, context: &mut Context) -> Result<()> {
        DirectoryOperations::set_position(self, context, 0)
    }

    fn close(&self, context: &mut Context) -> Result<()> {
        if context
            .take_private_data_of_type::<DirectoryHandle>()
            .is_some()
        {
            Ok(())
        } else {
            Err(IoErrorKind::InvalidState)
        }
    }
}

impl MountOperations for HostFileSystem {
    fn mount(&self) -> Result<()> {
        if self
            .root
            .lock()
            .map_err(|_| IoErrorKind::InputOutput)?
            .is_some()
        {
            Ok(())
        } else {
            Err(IoErrorKind::DeviceUnavailable)
        }
    }

    fn unmount(&self) -> Result<()> {
        // Like a Linux mount, detaching the namespace root does not revoke
        // already-open file or directory descriptors. New path lookups fail,
        // while existing contexts remain usable until they are closed.
        self.root
            .lock()
            .map_err(|_| IoErrorKind::InputOutput)?
            .take()
            .map(drop)
            .ok_or(IoErrorKind::DeviceUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{AccessFlags, AttributeFlags, CreateFlags, Flags};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(unix)]
    #[test]
    fn maps_host_errno_to_io_error_kind() {
        use libc::*;

        for (errno, expected) in [
            (ENOENT, IoErrorKind::NotFound),
            (EACCES, IoErrorKind::PermissionDenied),
            (EPERM, IoErrorKind::OperationNotPermitted),
            (EROFS, IoErrorKind::ReadOnlyFileSystem),
            (EEXIST, IoErrorKind::AlreadyExists),
            (ENOTDIR, IoErrorKind::NotADirectory),
            (EISDIR, IoErrorKind::IsADirectory),
            (ENOTEMPTY, IoErrorKind::DirectoryNotEmpty),
            (ELOOP, IoErrorKind::SymlinkLoop),
            (EXDEV, IoErrorKind::CrossDevice),
            (ENOSYS, IoErrorKind::NotImplemented),
            (ENOTSUP, IoErrorKind::Unsupported),
            (EOPNOTSUPP, IoErrorKind::Unsupported),
            (ESPIPE, IoErrorKind::InvalidSeek),
            (EMFILE, IoErrorKind::ProcessFileLimit),
            (ENFILE, IoErrorKind::SystemFileLimit),
            (ENAMETOOLONG, IoErrorKind::NameTooLong),
            (ENOSPC, IoErrorKind::NoSpace),
            (EDQUOT, IoErrorKind::NoSpace),
            (EBUSY, IoErrorKind::ResourceBusy),
            (ETXTBSY, IoErrorKind::ResourceBusy),
            (EAGAIN, IoErrorKind::WouldBlock),
            (EWOULDBLOCK, IoErrorKind::WouldBlock),
            (EINTR, IoErrorKind::Interrupted),
            (EIO, IoErrorKind::InputOutput),
            (ESTALE, IoErrorKind::StaleHandle),
            (EINVAL, IoErrorKind::InvalidArgument),
            (EBADF, IoErrorKind::BadDescriptor),
            (ENOMEM, IoErrorKind::OutOfMemory),
            (EFBIG, IoErrorKind::FileTooLarge),
            (ENODEV, IoErrorKind::DeviceUnavailable),
            (ETIMEDOUT, IoErrorKind::TimedOut),
            (i32::MAX, IoErrorKind::InputOutput),
        ] {
            assert_eq!(
                map_io_error(std::io::Error::from_raw_os_error(errno)),
                expected,
                "errno {errno}"
            );
        }
    }

    #[test]
    fn maps_a_cap_std_escape_to_path_escapes_root() {
        let escape = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "a path led outside of the filesystem",
        );
        assert_eq!(map_io_error(escape), IoErrorKind::PathEscapesRoot);
        #[cfg(unix)]
        assert_eq!(
            map_io_error(std::io::Error::from_raw_os_error(libc::EACCES)),
            IoErrorKind::PermissionDenied
        );
    }

    #[test]
    fn maps_portable_io_error_kinds() {
        use std::io::ErrorKind as Io;

        for (kind, expected) in [
            (Io::InvalidInput, IoErrorKind::InvalidPath),
            (Io::InvalidData, IoErrorKind::InvalidData),
            (Io::ReadOnlyFilesystem, IoErrorKind::ReadOnlyFileSystem),
            (Io::StorageFull, IoErrorKind::NoSpace),
            (Io::FileTooLarge, IoErrorKind::FileTooLarge),
            (Io::Unsupported, IoErrorKind::Unsupported),
            (Io::CrossesDevices, IoErrorKind::CrossDevice),
            (Io::WouldBlock, IoErrorKind::WouldBlock),
            (Io::ResourceBusy, IoErrorKind::ResourceBusy),
            (Io::ExecutableFileBusy, IoErrorKind::ResourceBusy),
            (Io::TimedOut, IoErrorKind::TimedOut),
            (Io::Interrupted, IoErrorKind::Interrupted),
            (Io::Other, IoErrorKind::InputOutput),
        ] {
            assert_eq!(map_io_error(kind.into()), expected, "{kind:?}");
        }
    }

    fn temp_root() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "xila-hostfs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn confines_parent_absolute_and_symlink_paths() {
        let root = temp_root();
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        assert_eq!(
            HostFileSystem::relative_path(Path::new("/../outside/secret")),
            Err(IoErrorKind::InvalidPath)
        );
        let mut context = Context::new_empty();
        assert_eq!(
            filesystem.lookup_file(
                &mut context,
                Path::new("/../secret"),
                AccessFlags::Read.into()
            ),
            Err(IoErrorKind::InvalidPath)
        );
        #[cfg(unix)]
        assert!(
            filesystem
                .lookup_file(
                    &mut context,
                    Path::new("/escape/secret"),
                    AccessFlags::Read.into()
                )
                .is_err()
        );
        assert_eq!(
            filesystem.rename(Path::new("/../secret"), Path::new("/inside")),
            Err(IoErrorKind::InvalidPath)
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn hostfs_round_trips_file_io_and_enforces_read_only() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        filesystem.create_file(Path::new("/file")).unwrap();
        let mut context = Context::new_empty();
        filesystem
            .lookup_file(
                &mut context,
                Path::new("/file"),
                Flags::from(AccessFlags::READ_WRITE),
            )
            .unwrap();
        assert_eq!(
            BaseOperations::write_sync(&filesystem, &mut context, b"data", 0),
            Ok(4)
        );
        fs::write(root.join("file"), b"host").unwrap();
        let mut data = [0; 4];
        assert_eq!(
            BaseOperations::read_sync(&filesystem, &mut context, &mut data, 0),
            Ok(4)
        );
        assert_eq!(&data, b"host");
        BaseOperations::close(&filesystem, &mut context).unwrap();
        let readonly = HostFileSystem::open(
            &root,
            HostFileSystemOptions {
                read_only: true,
                ..HostFileSystemOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            readonly.create_file(Path::new("/blocked")),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        let mut attrs = Attributes::new().set_mask(AttributeFlags::Size);
        FileSystemOperations::get_attributes(&readonly, Path::new("/file"), &mut attrs).unwrap();
        assert_eq!(attrs.get_size(), Some(&4));
        assert!(root.join("file").exists());
        assert_eq!(
            readonly.create_directory(Path::new("/blocked-dir")),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        assert_eq!(
            readonly.remove(Path::new("/file")),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        assert_eq!(
            readonly.rename(Path::new("/file"), Path::new("/renamed")),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        assert_eq!(
            FileSystemOperations::set_attributes(
                &readonly,
                Path::new("/file"),
                &Attributes::new().set_size(1),
            ),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        let mut read_write_context = Context::new_empty();
        filesystem
            .lookup_file(
                &mut read_write_context,
                Path::new("/file"),
                Flags::from(AccessFlags::READ_WRITE),
            )
            .unwrap();
        assert_eq!(
            BaseOperations::write_sync(&readonly, &mut read_write_context, b"x", 0),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        assert_eq!(
            AttributeOperations::set_attributes(
                &readonly,
                &mut read_write_context,
                &Attributes::new().set_size(1),
            ),
            Err(IoErrorKind::ReadOnlyFileSystem)
        );
        BaseOperations::close(&filesystem, &mut read_write_context).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reports_stable_inode_and_synthetic_identity() {
        let root = temp_root();
        fs::write(root.join("file"), b"x").unwrap();
        let filesystem = HostFileSystem::open(
            &root,
            HostFileSystemOptions {
                user: users::UserIdentifier::new(42),
                group: users::GroupIdentifier::new(43),
                ..HostFileSystemOptions::default()
            },
        )
        .unwrap();
        let mut attrs = Attributes::new().set_mask(
            AttributeFlags::Kind
                | AttributeFlags::Inode
                | AttributeFlags::Links
                | AttributeFlags::User
                | AttributeFlags::Group
                | AttributeFlags::Permissions,
        );
        FileSystemOperations::get_attributes(&filesystem, Path::new("/file"), &mut attrs).unwrap();
        assert_eq!(attrs.get_kind(), Some(&Kind::File));
        assert!(attrs.get_inode().is_some());
        assert!(attrs.get_links().is_some());
        assert_eq!(attrs.get_user(), Some(&users::UserIdentifier::new(42)));
        assert_eq!(attrs.get_group(), Some(&users::GroupIdentifier::new(43)));
        assert_eq!(attrs.get_permissions(), Some(&Permissions::FILE_DEFAULT));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlink_metadata_payload_and_directory_entry_are_no_follow() {
        let root = temp_root();
        fs::write(root.join("target"), b"payload").unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        filesystem
            .create_symlink(Path::new("target"), Path::new("/link"))
            .unwrap();

        let mut attributes =
            Attributes::new().set_mask(AttributeFlags::Kind | AttributeFlags::Size);
        FileSystemOperations::get_attributes(&filesystem, Path::new("/link"), &mut attributes)
            .unwrap();
        assert_eq!(attributes.get_kind(), Some(&Kind::SymbolicLink));
        assert_eq!(attributes.get_size(), Some(&("target".len() as Size)));

        let mut target = [0; 16];
        let length = filesystem
            .read_symlink(Path::new("/link"), &mut target)
            .unwrap();
        assert_eq!(&target[..length], b"target");

        let mut directory = Context::new_empty();
        filesystem
            .lookup_directory(&mut directory, Path::ROOT)
            .unwrap();
        let entry = loop {
            let Some(entry) = DirectoryOperations::read(&filesystem, &mut directory).unwrap()
            else {
                panic!("link missing from host directory listing");
            };
            if entry.name == "link" {
                break entry;
            }
        };
        assert_eq!(entry.kind, Kind::SymbolicLink);
        assert_eq!(entry.size, "target".len() as Size);
        DirectoryOperations::close(&filesystem, &mut directory).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_host_absolute_symlink_targets() {
        let root = temp_root();
        std::os::unix::fs::symlink("/etc/passwd", root.join("host-absolute")).unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let mut target = [0; 64];
        assert!(
            filesystem
                .read_symlink(Path::new("/host-absolute"), &mut target)
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn satisfies_generic_filesystem_metadata_contract() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        file_system::tests::test_attribute_operations_with_capabilities(
            &filesystem,
            file_system::tests::FileSystemTestCapabilities {
                readable: AttributeFlags::Inode
                    | AttributeFlags::Kind
                    | AttributeFlags::Size
                    | AttributeFlags::Links
                    | AttributeFlags::ModificationTime
                    | AttributeFlags::AccessTime
                    | AttributeFlags::Permissions
                    | AttributeFlags::User
                    | AttributeFlags::Group,
                writable: AttributeFlags::Size,
            },
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mount_lifecycle_releases_the_root_capability() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        MountOperations::mount(&filesystem).unwrap();
        MountOperations::unmount(&filesystem).unwrap();
        assert_eq!(
            filesystem.create_file(Path::new("/after-unmount")),
            Err(IoErrorKind::DeviceUnavailable)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn open_file_context_survives_unmount_like_a_linux_descriptor() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        filesystem.create_file(Path::new("/file")).unwrap();
        filesystem
            .create_directory(Path::new("/directory"))
            .unwrap();
        let mut context = Context::new_empty();
        let mut directory_context = Context::new_empty();
        filesystem
            .lookup_file(
                &mut context,
                Path::new("/file"),
                Flags::from(AccessFlags::READ_WRITE),
            )
            .unwrap();
        filesystem
            .lookup_directory(&mut directory_context, Path::new("/directory"))
            .unwrap();

        MountOperations::unmount(&filesystem).unwrap();
        assert_eq!(
            BaseOperations::write_sync(&filesystem, &mut context, b"still open", 0),
            Ok(10)
        );
        assert_eq!(
            filesystem.create_file(Path::new("/new-path")),
            Err(IoErrorKind::DeviceUnavailable)
        );
        DirectoryOperations::set_position(&filesystem, &mut directory_context, 0).unwrap();
        let mut attributes = Attributes::new().set_mask(AttributeFlags::Kind);
        AttributeOperations::get_attributes(&filesystem, &mut directory_context, &mut attributes)
            .unwrap();
        assert_eq!(attributes.get_kind(), Some(&Kind::Directory));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_host_special_files_without_blocking() {
        let root = temp_root();
        let fifo = root.join("pipe");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let mut context = Context::new_empty();
        assert_eq!(
            filesystem.lookup_file(&mut context, Path::new("/pipe"), AccessFlags::Read.into()),
            Err(IoErrorKind::Unsupported)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    fn open_root_entry(filesystem: &HostFileSystem) -> Context {
        let mut context = Context::new_empty();
        filesystem
            .lookup_directory(&mut context, Path::ROOT)
            .unwrap();
        context
    }

    #[cfg(target_os = "linux")]
    fn entry_kind(filesystem: &HostFileSystem, parent: &Context, name: &str) -> Result<Kind> {
        let mut attributes = Attributes::new().set_mask(AttributeFlags::Kind);
        filesystem.get_entry_attributes(parent, name, &mut attributes)?;
        attributes
            .get_kind()
            .copied()
            .ok_or(IoErrorKind::InputOutput)
    }

    #[test]
    fn reports_confined_namespace_with_directory_entry_operations() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        assert_eq!(
            filesystem.supports_directory_entry_operations(),
            cfg!(target_os = "linux")
        );
        assert_eq!(
            filesystem.supports_confined_namespace(),
            filesystem.supports_directory_entry_operations()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directory_entry_lookup_and_creation() {
        let root = temp_root();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let mut root_context = open_root_entry(&filesystem);

        filesystem
            .create_directory_entry(&root_context, "directory")
            .unwrap();
        assert_eq!(
            filesystem.create_directory_entry(&root_context, "directory"),
            Err(IoErrorKind::AlreadyExists)
        );
        assert!(root.join("directory").is_dir());

        let mut directory = Context::new_empty();
        filesystem
            .lookup_directory_entry(&root_context, "directory", &mut directory)
            .unwrap();
        filesystem.create_file_entry(&directory, "file").unwrap();
        assert_eq!(
            filesystem.create_file_entry(&directory, "file"),
            Err(IoErrorKind::AlreadyExists)
        );
        assert!(root.join("directory/file").is_file());

        // A context opened by entry lookup is a complete directory handle.
        let entry = DirectoryOperations::read(&filesystem, &mut directory)
            .unwrap()
            .unwrap();
        assert_eq!(entry.name, "file");
        assert_eq!(entry.kind, Kind::File);
        assert_eq!(
            DirectoryOperations::read(&filesystem, &mut directory).unwrap(),
            None
        );
        DirectoryOperations::rewind(&filesystem, &mut directory).unwrap();
        let mut clone = filesystem.clone_context(&directory).unwrap();
        assert_eq!(
            DirectoryOperations::read(&filesystem, &mut clone)
                .unwrap()
                .map(|entry| entry.name),
            Some("file".into())
        );
        DirectoryOperations::close(&filesystem, &mut clone).unwrap();

        let mut missing = Context::new_empty();
        assert_eq!(
            filesystem.lookup_directory_entry(&root_context, "missing", &mut missing),
            Err(IoErrorKind::NotFound)
        );
        assert_eq!(
            filesystem.lookup_directory_entry(&directory, "file", &mut missing),
            Err(IoErrorKind::NotADirectory)
        );
        DirectoryOperations::close(&filesystem, &mut directory).unwrap();
        DirectoryOperations::close(&filesystem, &mut root_context).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_entry_lookup_and_attributes() {
        let root = temp_root();
        fs::write(root.join("file"), b"content").unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let root_context = open_root_entry(&filesystem);

        let mut file = Context::new_empty();
        filesystem
            .lookup_file_entry(
                &root_context,
                "file",
                &mut file,
                Flags::from(AccessFlags::READ_WRITE),
            )
            .unwrap();
        let mut data = [0; 7];
        assert_eq!(
            BaseOperations::read_sync(&filesystem, &mut file, &mut data, 0),
            Ok(7)
        );
        assert_eq!(&data, b"content");
        BaseOperations::close(&filesystem, &mut file).unwrap();

        let mut created = Context::new_empty();
        filesystem
            .lookup_file_entry(
                &root_context,
                "created",
                &mut created,
                Flags::new(
                    AccessFlags::Write,
                    Some(CreateFlags::Create | CreateFlags::Exclusive),
                    None,
                ),
            )
            .unwrap();
        BaseOperations::close(&filesystem, &mut created).unwrap();
        assert!(root.join("created").is_file());

        let mut directory = Context::new_empty();
        fs::create_dir(root.join("directory")).unwrap();
        assert_eq!(
            filesystem.lookup_file_entry(
                &root_context,
                "directory",
                &mut directory,
                AccessFlags::Read.into()
            ),
            Err(IoErrorKind::IsADirectory)
        );

        let mut attributes = Attributes::new().set_mask(
            AttributeFlags::Kind
                | AttributeFlags::Size
                | AttributeFlags::Inode
                | AttributeFlags::User
                | AttributeFlags::Permissions,
        );
        filesystem
            .get_entry_attributes(&root_context, "file", &mut attributes)
            .unwrap();
        assert_eq!(attributes.get_kind(), Some(&Kind::File));
        assert_eq!(attributes.get_size(), Some(&7));
        assert!(attributes.get_inode().is_some());
        assert_eq!(attributes.get_user(), Some(&users::UserIdentifier::ROOT));
        assert_eq!(
            attributes.get_permissions(),
            Some(&Permissions::FILE_DEFAULT)
        );
        assert_eq!(
            entry_kind(&filesystem, &root_context, "directory"),
            Ok(Kind::Directory)
        );
        assert_eq!(
            entry_kind(&filesystem, &root_context, "missing"),
            Err(IoErrorKind::NotFound)
        );

        filesystem
            .set_entry_attributes(&root_context, "file", &Attributes::new().set_size(3))
            .unwrap();
        assert_eq!(fs::read(root.join("file")).unwrap(), b"con");
        filesystem
            .set_entry_attributes(&root_context, "file", &Attributes::new())
            .unwrap();
        assert_eq!(
            filesystem.set_entry_attributes(
                &root_context,
                "file",
                &Attributes::new().set_user(users::UserIdentifier::new(1))
            ),
            Err(IoErrorKind::Unsupported)
        );
        assert_eq!(
            filesystem.set_entry_attributes(
                &root_context,
                "directory",
                &Attributes::new().set_size(0)
            ),
            Err(IoErrorKind::IsADirectory)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn removes_renames_and_links_entries() {
        let root = temp_root();
        fs::create_dir(root.join("source")).unwrap();
        fs::create_dir(root.join("destination")).unwrap();
        fs::write(root.join("source/file"), b"data").unwrap();
        fs::create_dir(root.join("source/directory")).unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let root_context = open_root_entry(&filesystem);
        let mut source = Context::new_empty();
        let mut destination = Context::new_empty();
        filesystem
            .lookup_directory_entry(&root_context, "source", &mut source)
            .unwrap();
        filesystem
            .lookup_directory_entry(&root_context, "destination", &mut destination)
            .unwrap();

        filesystem
            .rename_entry(&source, "file", &destination, "moved")
            .unwrap();
        assert!(!root.join("source/file").exists());
        assert_eq!(fs::read(root.join("destination/moved")).unwrap(), b"data");
        assert_eq!(
            filesystem.rename_entry(&source, "file", &destination, "again"),
            Err(IoErrorKind::NotFound)
        );

        filesystem
            .link_entry(&destination, "moved", &source, "hard")
            .unwrap();
        let mut attributes = Attributes::new().set_mask(AttributeFlags::Links);
        filesystem
            .get_entry_attributes(&source, "hard", &mut attributes)
            .unwrap();
        assert_eq!(attributes.get_links(), Some(&2));
        assert_eq!(
            filesystem.link_entry(&source, "directory", &destination, "linked"),
            Err(IoErrorKind::IsADirectory)
        );

        filesystem.remove_entry(&source, "hard").unwrap();
        filesystem.remove_entry(&source, "directory").unwrap();
        assert!(!root.join("source/hard").exists());
        assert!(!root.join("source/directory").exists());
        fs::write(root.join("destination/inner"), b"").unwrap();
        assert_eq!(
            filesystem.remove_entry(&root_context, "destination"),
            Err(IoErrorKind::DirectoryNotEmpty)
        );
        assert_eq!(
            filesystem.remove_entry(&source, "missing"),
            Err(IoErrorKind::NotFound)
        );
        DirectoryOperations::close(&filesystem, &mut source).unwrap();
        DirectoryOperations::close(&filesystem, &mut destination).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn symbolic_link_entries_are_never_followed() {
        let root = temp_root();
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"secret").unwrap();
        fs::create_dir(root.join("directory")).unwrap();
        fs::write(root.join("file"), b"inside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("absolute")).unwrap();
        std::os::unix::fs::symlink("../outside-relative", root.join("relative")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("secret")).unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let root_context = open_root_entry(&filesystem);

        filesystem
            .create_symlink_entry(Path::new("directory"), &root_context, "directory-link")
            .unwrap();
        filesystem
            .create_symlink_entry(Path::new("file"), &root_context, "file-link")
            .unwrap();
        assert_eq!(
            fs::read_link(root.join("file-link")).unwrap(),
            PathBuf::from("file")
        );
        assert_eq!(
            filesystem.create_symlink_entry(Path::new("/file"), &root_context, "absolute-link"),
            Err(IoErrorKind::Unsupported)
        );
        assert_eq!(
            filesystem.create_symlink_entry(Path::new("file"), &root_context, "file-link"),
            Err(IoErrorKind::AlreadyExists)
        );

        let mut buffer = [0; 64];
        let length = filesystem
            .read_symlink_entry(&root_context, "file-link", &mut buffer)
            .unwrap();
        assert_eq!(&buffer[..length], b"file");
        // A relative target is returned as stored, for the virtual file system
        // to resolve within its own namespace.
        let length = filesystem
            .read_symlink_entry(&root_context, "relative", &mut buffer)
            .unwrap();
        assert_eq!(&buffer[..length], b"../outside-relative");
        assert_eq!(
            filesystem.read_symlink_entry(&root_context, "absolute", &mut buffer),
            Err(IoErrorKind::CorruptData)
        );
        assert_eq!(
            filesystem.read_symlink_entry(&root_context, "file-link", &mut [0; 2]),
            Err(IoErrorKind::NameTooLong)
        );
        assert_eq!(
            filesystem.read_symlink_entry(&root_context, "file", &mut buffer),
            Err(IoErrorKind::InvalidArgument)
        );

        let mut attributes =
            Attributes::new().set_mask(AttributeFlags::Kind | AttributeFlags::Size);
        filesystem
            .get_entry_attributes(&root_context, "file-link", &mut attributes)
            .unwrap();
        assert_eq!(attributes.get_kind(), Some(&Kind::SymbolicLink));
        assert_eq!(attributes.get_size(), Some(&("file".len() as Size)));

        for link in ["directory-link", "absolute", "relative"] {
            let mut context = Context::new_empty();
            assert_eq!(
                filesystem.lookup_directory_entry(&root_context, link, &mut context),
                Err(IoErrorKind::NotADirectory),
                "{link}"
            );
        }
        for link in ["file-link", "secret"] {
            let mut context = Context::new_empty();
            assert_eq!(
                filesystem.lookup_file_entry(
                    &root_context,
                    link,
                    &mut context,
                    AccessFlags::Read.into()
                ),
                Err(IoErrorKind::SymlinkLoop),
                "{link}"
            );
            assert_eq!(
                filesystem.set_entry_attributes(
                    &root_context,
                    link,
                    &Attributes::new().set_size(0)
                ),
                Err(IoErrorKind::SymlinkLoop),
                "{link}"
            );
        }
        let mut context = Context::new_empty();
        assert_eq!(
            filesystem.lookup_file_entry(
                &root_context,
                "secret",
                &mut context,
                Flags::new(
                    AccessFlags::Write,
                    Some(CreateFlags::Create | CreateFlags::Exclusive),
                    None,
                ),
            ),
            Err(IoErrorKind::AlreadyExists)
        );
        assert_eq!(
            filesystem.create_file_entry(&root_context, "secret"),
            Err(IoErrorKind::AlreadyExists)
        );
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"secret");

        // Renaming, linking and removing act on the link, not its target.
        filesystem
            .rename_entry(&root_context, "secret", &root_context, "renamed")
            .unwrap();
        assert!(
            fs::symlink_metadata(root.join("renamed"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        filesystem
            .link_entry(&root_context, "renamed", &root_context, "linked")
            .unwrap();
        assert!(
            fs::symlink_metadata(root.join("linked"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        for link in ["renamed", "linked", "directory-link", "absolute"] {
            filesystem.remove_entry(&root_context, link).unwrap();
        }
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"secret");
        assert!(root.join("directory").is_dir());
        assert!(outside.is_dir());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn entry_operations_reject_paths_and_dot_names() {
        let root = temp_root();
        fs::create_dir(root.join("directory")).unwrap();
        fs::write(root.join("directory/file"), b"").unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let root_context = open_root_entry(&filesystem);
        for name in [
            "",
            ".",
            "..",
            "directory/file",
            "/directory",
            "../outside",
            "a\0b",
        ] {
            let mut context = Context::new_empty();
            let mut attributes = Attributes::new().set_mask(AttributeFlags::Kind);
            let mut buffer = [0; 16];
            assert_eq!(
                filesystem.lookup_directory_entry(&root_context, name, &mut context),
                Err(IoErrorKind::InvalidPath),
                "{name:?}"
            );
            assert_eq!(
                filesystem.lookup_file_entry(
                    &root_context,
                    name,
                    &mut context,
                    AccessFlags::Read.into()
                ),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.create_directory_entry(&root_context, name),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.create_file_entry(&root_context, name),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.remove_entry(&root_context, name),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.rename_entry(&root_context, name, &root_context, "new"),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.rename_entry(&root_context, "directory", &root_context, name),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.link_entry(&root_context, name, &root_context, "new"),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.create_symlink_entry(Path::new("target"), &root_context, name),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.read_symlink_entry(&root_context, name, &mut buffer),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.get_entry_attributes(&root_context, name, &mut attributes),
                Err(IoErrorKind::InvalidPath)
            );
            assert_eq!(
                filesystem.set_entry_attributes(
                    &root_context,
                    name,
                    &Attributes::new().set_size(0)
                ),
                Err(IoErrorKind::InvalidPath)
            );
        }
        // A file context is not a parent directory.
        let mut file = Context::new_empty();
        filesystem
            .lookup_file(
                &mut file,
                Path::new("/directory/file"),
                AccessFlags::Read.into(),
            )
            .unwrap();
        assert_eq!(
            filesystem.create_file_entry(&file, "child"),
            Err(IoErrorKind::InvalidState)
        );
        BaseOperations::close(&filesystem, &mut file).unwrap();
        assert!(root.join("directory").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_only_rejects_entry_mutations() {
        let root = temp_root();
        fs::write(root.join("file"), b"data").unwrap();
        let filesystem = HostFileSystem::open(
            &root,
            HostFileSystemOptions {
                read_only: true,
                ..HostFileSystemOptions::default()
            },
        )
        .unwrap();
        let root_context = open_root_entry(&filesystem);
        let read_only = Err(IoErrorKind::ReadOnlyFileSystem);
        assert_eq!(
            filesystem.create_directory_entry(&root_context, "new"),
            read_only
        );
        assert_eq!(
            filesystem.create_file_entry(&root_context, "new"),
            read_only
        );
        assert_eq!(filesystem.remove_entry(&root_context, "file"), read_only);
        assert_eq!(
            filesystem.rename_entry(&root_context, "file", &root_context, "new"),
            read_only
        );
        assert_eq!(
            filesystem.link_entry(&root_context, "file", &root_context, "new"),
            read_only
        );
        assert_eq!(
            filesystem.create_symlink_entry(Path::new("file"), &root_context, "new"),
            read_only
        );
        assert_eq!(
            filesystem.set_entry_attributes(&root_context, "file", &Attributes::new().set_size(0)),
            read_only
        );
        let mut context = Context::new_empty();
        assert_eq!(
            filesystem.lookup_file_entry(
                &root_context,
                "file",
                &mut context,
                Flags::from(AccessFlags::READ_WRITE)
            ),
            read_only
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"data");
        let _ = fs::remove_dir_all(root);
    }

    /// The virtual file system holds the parent descriptor while a racing host
    /// process renames that directory away and puts a link to an outside
    /// directory in its place: the pinned descriptor keeps every entry
    /// operation inside the moved directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_parent_survives_replacement_by_an_escaping_link() {
        use std::thread;

        let root = temp_root();
        let outside = root.with_extension("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(root.join("parent")).unwrap();
        let filesystem = HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap();
        let root_context = open_root_entry(&filesystem);

        // Deterministic replacement after the parent was opened.
        let mut parent = Context::new_empty();
        filesystem
            .lookup_directory_entry(&root_context, "parent", &mut parent)
            .unwrap();
        fs::rename(root.join("parent"), root.join("parent-moved")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("parent")).unwrap();
        filesystem.create_file_entry(&parent, "pinned").unwrap();
        filesystem
            .create_directory_entry(&parent, "directory")
            .unwrap();
        filesystem
            .create_symlink_entry(Path::new("pinned"), &parent, "link")
            .unwrap();
        filesystem
            .rename_entry(&parent, "pinned", &parent, "renamed")
            .unwrap();
        assert!(root.join("parent-moved/renamed").is_file());
        assert!(root.join("parent-moved/directory").is_dir());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        // A new lookup through the replaced name refuses the link.
        let mut replaced = Context::new_empty();
        assert_eq!(
            filesystem.lookup_directory_entry(&root_context, "parent", &mut replaced),
            Err(IoErrorKind::NotADirectory)
        );
        DirectoryOperations::close(&filesystem, &mut parent).unwrap();
        fs::remove_file(root.join("parent")).unwrap();
        fs::remove_dir_all(root.join("parent-moved")).unwrap();
        fs::create_dir(root.join("parent")).unwrap();

        // Concurrent replacement while lookups and creations run.
        let replacing_root = root.clone();
        let replacing_outside = outside.clone();
        let replacer = thread::spawn(move || {
            for _ in 0..300 {
                let parent = replacing_root.join("parent");
                let moved = replacing_root.join("parent-moved");
                let _ = fs::rename(&parent, &moved);
                let _ = std::os::unix::fs::symlink(&replacing_outside, &parent);
                let _ = fs::remove_file(&parent);
                let _ = fs::rename(&moved, &parent);
            }
        });
        for _ in 0..300 {
            let mut parent = Context::new_empty();
            if filesystem
                .lookup_directory_entry(&root_context, "parent", &mut parent)
                .is_ok()
            {
                let _ = filesystem.create_file_entry(&parent, "pwned");
                let _ = filesystem.create_directory_entry(&parent, "pwned-directory");
                let _ = filesystem.remove_entry(&parent, "pwned");
                let _ = filesystem.remove_entry(&parent, "pwned-directory");
                DirectoryOperations::close(&filesystem, &mut parent).unwrap();
            }
        }
        replacer.join().unwrap();
        assert!(!outside.join("pwned").exists());
        assert!(!outside.join("pwned-directory").exists());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_replacement_cannot_redirect_creates_outside_root() {
        use std::thread;

        let root = temp_root();
        let outside = root.with_extension("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(root.join("parent")).unwrap();
        let filesystem =
            Arc::new(HostFileSystem::open(&root, HostFileSystemOptions::default()).unwrap());
        let replacing_root = root.clone();
        let replacing_outside = outside.clone();
        let replacer = thread::spawn(move || {
            for _ in 0..300 {
                let parent = replacing_root.join("parent");
                let moved = replacing_root.join("parent-moved");
                let _ = fs::rename(&parent, &moved);
                let _ = std::os::unix::fs::symlink(&replacing_outside, &parent);
                let _ = fs::remove_file(&parent);
                let _ = fs::rename(&moved, &parent);
            }
        });
        for _ in 0..300 {
            let _ = filesystem.create_file(Path::new("/parent/pwned"));
            let _ = fs::remove_file(root.join("parent/pwned"));
        }
        replacer.join().unwrap();
        assert!(!outside.join("pwned").exists());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}
