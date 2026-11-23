use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    Io(io::ErrorKind),
    UnsafePath,
    LockBusy,
    TooLarge,
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

pub(crate) fn ensure_directory(path: &Path) -> Result<(), Error> {
    directory(path, true).map(|_| ())
}

pub(crate) fn validate_directory(path: &Path) -> Result<(), Error> {
    open_private_directory(path).map(|_| ())
}

pub(crate) fn open_private_directory(path: &Path) -> Result<File, Error> {
    directory(path, false)
}

fn directory(path: &Path, create: bool) -> Result<File, Error> {
    let directory = walk_directory(path, create)?;
    validate_metadata(&directory.metadata()?, true)?;
    Ok(directory)
}

pub(crate) fn tighten_legacy_directory(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let directory = walk_directory(path, false)?;
    let metadata = directory.metadata()?;
    if metadata.uid() == unsafe { nix::libc::geteuid() } && metadata.mode() & 0o777 == 0o755 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    validate_metadata(&directory.metadata()?, true)
}

fn walk_directory(path: &Path, create: bool) -> Result<File, Error> {
    if !path.is_absolute() {
        return Err(Error::UnsafePath);
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(Error::UnsafePath);
        };
        validate_parent(&current.metadata()?)?;
        let name = c_name(name.as_bytes())?;
        current = open_directory_at(&current, &name, create)?;
    }
    Ok(current)
}

fn open_directory_at(parent: &File, name: &CString, create: bool) -> Result<File, Error> {
    let mut fd = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC,
        )
    };
    if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
        let result = unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io::Error::last_os_error().into());
        }
        fd = unsafe {
            nix::libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
            )
        };
    }
    if fd < 0 {
        return Err(open_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(crate) fn ensure_directory_at(parent: &File, name: &str) -> Result<File, Error> {
    let name = owner_name(parent, name)?;
    let directory = open_directory_at(parent, &name, true)?;
    validate_metadata(&directory.metadata()?, true)?;
    Ok(directory)
}

fn validate_parent(metadata: &Metadata) -> Result<(), Error> {
    let uid = unsafe { nix::libc::geteuid() };
    let sticky = metadata.mode() & 0o1000 != 0;
    if !metadata.is_dir()
        || (metadata.uid() != uid && metadata.uid() != 0)
        || (metadata.mode() & 0o022 != 0 && !sticky)
    {
        return Err(Error::UnsafePath);
    }
    Ok(())
}

pub(crate) fn validate_metadata(metadata: &Metadata, directory: bool) -> Result<(), Error> {
    if metadata.uid() != unsafe { nix::libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(Error::UnsafePath);
    }
    Ok(())
}

fn parent_and_name(path: &Path) -> Result<(File, CString), Error> {
    let parent = path.parent().ok_or(Error::UnsafePath)?;
    let name = path.file_name().ok_or(Error::UnsafePath)?;
    Ok((open_private_directory(parent)?, c_name(name.as_bytes())?))
}

fn c_name(bytes: &[u8]) -> Result<CString, Error> {
    CString::new(bytes).map_err(|_| Error::UnsafePath)
}

fn open_error() -> Error {
    classify_open_error(io::Error::last_os_error())
}

fn classify_open_error(error: io::Error) -> Error {
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::ELOOP | nix::libc::ENOTDIR)
    ) {
        Error::UnsafePath
    } else {
        error.into()
    }
}

fn owner_open_flags(create: bool) -> i32 {
    if create {
        nix::libc::O_RDWR | nix::libc::O_CREAT
    } else {
        nix::libc::O_RDONLY
    }
}

fn open_owner_file_raw(parent: i32, name: &CString, flags: i32) -> io::Result<File> {
    let fd = unsafe {
        nix::libc::openat(
            parent,
            name.as_ptr(),
            flags | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn validate_owner_file(file: File) -> Result<File, Error> {
    validate_metadata(&file.metadata()?, false)?;
    Ok(file)
}

fn open_at(parent: &File, name: &CString, create: bool) -> Result<File, Error> {
    // O_NONBLOCK prevents a pre-existing FIFO from blocking before type validation.
    let file = open_owner_file_raw(
        parent.as_raw_fd(),
        name,
        owner_open_flags(create) | nix::libc::O_NONBLOCK,
    )
    .map_err(classify_open_error)?;
    validate_owner_file(file)
}

fn open_leaf(path: &Path, flags: i32) -> Result<File, Error> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Io(io::ErrorKind::InvalidInput))?;
    let file = open_owner_file_raw(nix::libc::AT_FDCWD, &name, flags)?;
    validate_owner_file(file)
}

// The caller owns ancestor policy; only the leaf is protected and validated here.
pub(crate) fn open_owner_file_leaf(path: &Path, create: bool) -> Result<File, Error> {
    open_leaf(path, owner_open_flags(create))
}

pub(crate) fn create_owner_file_leaf(path: &Path) -> Result<File, Error> {
    open_leaf(
        path,
        nix::libc::O_RDWR | nix::libc::O_CREAT | nix::libc::O_EXCL,
    )
}

pub(crate) fn open_owner_file(path: &Path, create: bool) -> Result<File, Error> {
    let (parent, name) = parent_and_name(path)?;
    open_at(&parent, &name, create)
}

fn owner_name(parent: &File, name: &str) -> Result<CString, Error> {
    validate_metadata(&parent.metadata()?, true)?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(Error::UnsafePath);
    }
    c_name(name.as_bytes())
}

fn open_owner_file_at(parent: &File, name: &str, create: bool) -> Result<File, Error> {
    let name = owner_name(parent, name)?;
    open_at(parent, &name, create)
}

pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, Error> {
    read_opened_file(open_owner_file(path, false), limit)
}

pub(crate) fn read_bounded_at(
    parent: &File,
    name: &str,
    limit: usize,
) -> Result<Option<Vec<u8>>, Error> {
    read_opened_file(open_owner_file_at(parent, name, false), limit)
}

fn read_opened_file(file: Result<File, Error>, limit: usize) -> Result<Option<Vec<u8>>, Error> {
    let mut file = match file {
        Ok(file) => file,
        Err(Error::Io(io::ErrorKind::NotFound)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > limit as u64 {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::TooLarge);
    }
    Ok(Some(bytes))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], limit: usize) -> Result<(), Error> {
    if bytes.len() > limit {
        return Err(Error::TooLarge);
    }
    let (parent, name) = parent_and_name(path)?;
    atomic_write_inner(&parent, &name, bytes, limit)
}

pub(crate) fn atomic_write_at(
    parent: &File,
    name: &str,
    bytes: &[u8],
    limit: usize,
) -> Result<(), Error> {
    let name = owner_name(parent, name)?;
    atomic_write_inner(parent, &name, bytes, limit)
}

fn atomic_write_inner(
    parent: &File,
    name: &CString,
    bytes: &[u8],
    limit: usize,
) -> Result<(), Error> {
    if bytes.len() > limit {
        return Err(Error::TooLarge);
    }
    match open_at(parent, name, false) {
        Ok(_) | Err(Error::Io(io::ErrorKind::NotFound)) => {}
        Err(error) => return Err(error),
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut temporary = None;
    for _ in 0..16 {
        let candidate = c_name(
            format!(
                ".cosh-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
        match open_owner_file_raw(
            parent.as_raw_fd(),
            &candidate,
            nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL,
        ) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (temporary_name, mut file) = temporary.ok_or(Error::Io(io::ErrorKind::AlreadyExists))?;
    let result = (|| {
        validate_metadata(&file.metadata()?, false)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        let result = unsafe {
            nix::libc::renameat(
                parent.as_raw_fd(),
                temporary_name.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        parent.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            nix::libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0);
        }
    }
    result
}

pub(crate) fn remove_owner_file(path: &Path) -> Result<(), Error> {
    let (parent, name) = parent_and_name(path)?;
    remove_owner_file_inner(&parent, &name)
}

pub(crate) fn remove_owner_file_at(parent: &File, name: &str) -> Result<(), Error> {
    let name = owner_name(parent, name)?;
    remove_owner_file_inner(parent, &name)
}

fn remove_owner_file_inner(parent: &File, name: &CString) -> Result<(), Error> {
    match open_at(parent, name, false) {
        Ok(_) => {}
        Err(Error::Io(io::ErrorKind::NotFound)) => return Ok(()),
        Err(error) => return Err(error),
    }
    if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    parent.sync_all()?;
    Ok(())
}

pub(crate) fn try_lock_exclusive(file: &File) -> Result<(), Error> {
    if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(
            if matches!(error.raw_os_error(), Some(code) if code == nix::libc::EWOULDBLOCK || code == nix::libc::EAGAIN)
            {
                Error::LockBusy
            } else {
                error.into()
            },
        );
    }
    Ok(())
}

pub(crate) struct LockGuard {
    _file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        unsafe {
            nix::libc::flock(self._file.as_raw_fd(), nix::libc::LOCK_UN);
        }
    }
}

impl LockGuard {
    pub(crate) fn try_acquire(path: &Path) -> Result<Self, Error> {
        Self::lock_file(open_owner_file(path, true)?)
    }

    pub(crate) fn try_acquire_at(parent: &File, name: &str) -> Result<Self, Error> {
        Self::lock_file(open_owner_file_at(parent, name, true)?)
    }

    fn lock_file(file: File) -> Result<Self, Error> {
        try_lock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn guard_drop_releases_lease_despite_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("state.lock");
        let guard = LockGuard::try_acquire(&path).unwrap();
        let inherited = guard._file.try_clone().unwrap();
        drop(guard);
        assert!(
            LockGuard::try_acquire(&path).is_ok(),
            "the lexical lease must end before unrelated descriptor copies close"
        );
        drop(inherited);
    }

    #[test]
    fn missing_and_invalid_files_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(read_bounded(&root.join("missing"), 64), Ok(None));
        fs::create_dir(root.join("directory")).unwrap();
        assert_eq!(
            read_bounded(&root.join("directory"), 64),
            Err(Error::UnsafePath)
        );
    }

    #[test]
    fn rejects_permissions_links_and_oversized_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("state");
        atomic_write(&path, b"safe", 8).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_bounded(&path, 8), Err(Error::UnsafePath));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, root.join("hard")).unwrap();
        assert_eq!(read_bounded(&path, 8), Err(Error::UnsafePath));
        symlink(&path, root.join("link")).unwrap();
        assert_eq!(read_bounded(&root.join("link"), 8), Err(Error::UnsafePath));
        assert!(matches!(
            LockGuard::try_acquire(&root.join("link")),
            Err(Error::UnsafePath)
        ));
        atomic_write(&root.join("read-large"), b"12345", 8).unwrap();
        assert_eq!(
            read_bounded(&root.join("read-large"), 4),
            Err(Error::TooLarge)
        );
        assert_eq!(
            atomic_write(&root.join("large"), b"12345", 4),
            Err(Error::TooLarge)
        );
    }

    #[test]
    fn atomic_replace_preserves_old_content_on_rejected_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("state");
        atomic_write(&path, b"old", 8).unwrap();
        assert_eq!(atomic_write(&path, b"oversized", 8), Err(Error::TooLarge));
        assert_eq!(read_bounded(&path, 8), Ok(Some(b"old".to_vec())));
        atomic_write(&path, b"new", 8).unwrap();
        assert_eq!(read_bounded(&path, 8), Ok(Some(b"new".to_vec())));
    }

    #[test]
    fn lock_is_nonblocking_until_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let lock = root.join("lock");
        let guard = LockGuard::try_acquire(&lock).unwrap();
        assert!(matches!(
            LockGuard::try_acquire(&lock),
            Err(Error::LockBusy)
        ));
        drop(guard);
        assert!(LockGuard::try_acquire(&lock).is_ok());
    }

    #[test]
    fn refuses_symlinked_directory_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        ensure_directory(&root.join("real/child")).unwrap();
        symlink(root.join("real"), root.join("alias")).unwrap();
        assert_eq!(
            validate_directory(&root.join("alias/child")),
            Err(Error::UnsafePath)
        );
    }
}
