//! Atomic user configuration writes, with additional ownership checks for the wizard.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

use nix::fcntl::{Flock, FlockArg};
use nix::unistd::{fchown, Gid, Uid};

const WRITE_BITS: u32 = 0o022;

pub(super) fn lock(path: &Path, guard: bool) -> Result<(PathBuf, Vec<Flock<File>>), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("config path has no directory: {}", path.display()))?;
    let (target, directories) = if guard {
        verify_directory(directory)?;
        verify_document(path)?;
        (path.to_path_buf(), vec![directory.to_path_buf()])
    } else {
        fs::create_dir_all(directory)
            .map_err(|error| format!("create config directory failed: {error}"))?;
        resolve_target(path)?
    };

    // Core can replace any link in the chain under that link's directory lock.
    let mut files = directories
        .iter()
        .map(|directory| open_lock(directory, guard))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_unstable_by_key(|(identity, _)| *identity);
    files.dedup_by_key(|(identity, _)| *identity);
    let locks = files
        .into_iter()
        .map(|(_, file)| {
            Flock::lock(file, FlockArg::LockExclusive)
                .map_err(|(_, error)| format!("lock config file failed: {error}"))
        })
        .collect::<Result<_, _>>()?;
    if !guard {
        let (current, current_directories) = resolve_target(path)?;
        if current != target || current_directories != directories {
            return Err("config path changed while acquiring its lock".into());
        }
    }
    Ok((target, locks))
}

fn open_lock(directory: &Path, guard: bool) -> Result<((u64, u64), File), String> {
    // Core uses the same lock file, whose inode survives config.toml replacements.
    let lock_path = directory.join(".config.toml.lock");
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    let file = match options.open(&lock_path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            create_lock(directory, &lock_path, &mut options)
        }
        result => result,
    }
    .map_err(|error| format!("open config lock failed: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect config lock failed: {error}"))?;
    if !metadata.is_file() {
        return Err(format!(
            "config lock is not a regular file: {}",
            lock_path.display()
        ));
    }
    if guard {
        verify_owner_and_mode(&metadata, &lock_path)?;
    }
    Ok(((metadata.dev(), metadata.ino()), file))
}

fn create_lock(directory: &Path, path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    if Uid::effective().is_root() {
        let owner = Uid::from_raw(fs::metadata(directory)?.uid());
        if !owner.is_root() {
            // Set ownership before publishing so the directory owner can open a 0600 lock.
            let temporary = tempfile::NamedTempFile::new_in(directory)?;
            fchown(temporary.as_file().as_raw_fd(), Some(owner), None)?;
            return match temporary.persist_noclobber(path) {
                Ok(file) => Ok(file),
                Err(error) if error.error.kind() == ErrorKind::AlreadyExists => options.open(path),
                Err(error) => Err(error.error),
            };
        }
    }
    options.create(true).open(path)
}

fn resolve_target(path: &Path) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let mut candidate = path.to_path_buf();
    let mut directories = Vec::new();
    for _ in 0..40 {
        let spelling = candidate.as_os_str().as_encoded_bytes();
        if spelling.ends_with(b"/") || spelling.ends_with(b"/.") {
            return Err(format!(
                "config path requires a directory: {}",
                candidate.display()
            ));
        }
        let name = candidate
            .file_name()
            .ok_or_else(|| format!("config path has no filename: {}", candidate.display()))?;
        let directory = candidate
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = fs::canonicalize(directory)
            .map_err(|error| format!("resolve config directory failed: {error}"))?;
        candidate = directory.join(name);
        let resolved = fs::read_link(&candidate).map(|link| directory.join(link));
        if !directories.contains(&directory) {
            directories.push(directory);
        }
        match resolved {
            Ok(link) => candidate = link,
            Err(error) if matches!(error.kind(), ErrorKind::InvalidInput | ErrorKind::NotFound) => {
                return Ok((candidate, directories));
            }
            Err(error) => return Err(format!("resolve config file failed: {error}")),
        }
    }
    Err("too many config symlinks".into())
}

pub(super) fn read(path: &Path, guard: bool) -> Result<String, String> {
    if guard {
        verify_document(path)?;
    }
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("read config file failed: {error}")),
    };
    config_metadata(&file, path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|error| format!("read config file failed: {error}"))?;
    Ok(content)
}

pub(super) fn write(path: &Path, content: &str, guard: bool) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("config path has no directory: {}", path.display()))?;
    let existing = if guard {
        verify_directory(directory)?;
        verify_document(path)?;
        None
    } else {
        existing_writable_config(path)?
    };
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("create config temporary failed: {error}"))?;
    let file = temporary.as_file_mut();
    if let Some(original) = existing {
        preserve_file_permissions(&original, file)?;
    }
    file.write_all(content.as_bytes())
        .map_err(|error| format!("write config file failed: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync config file failed: {error}"))?;
    if guard {
        verify_directory(directory)?;
        verify_document(path)?;
    }
    temporary
        .persist(path)
        .map_err(|error| format!("write config file failed: {}", error.error))?;
    Ok(())
}

fn preserve_file_permissions(original: &File, temporary: &File) -> Result<(), String> {
    let old = original
        .metadata()
        .map_err(|error| format!("inspect config file failed: {error}"))?;
    let new = temporary
        .metadata()
        .map_err(|error| format!("inspect config temporary failed: {error}"))?;
    let owner = (new.uid() != old.uid()).then(|| Uid::from_raw(old.uid()));
    let group = (new.gid() != old.gid()).then(|| Gid::from_raw(old.gid()));
    if owner.is_some() || group.is_some() {
        fchown(temporary.as_raw_fd(), owner, group)
            .map_err(|error| format!("preserve config ownership failed: {error}"))?;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    preserve_access_acl(original, temporary)
        .map_err(|error| format!("preserve config ACL failed: {error}"))?;
    temporary
        .set_permissions(old.permissions())
        .map_err(|error| format!("preserve config permissions failed: {error}"))
}

#[cfg(target_os = "linux")]
fn preserve_access_acl(original: &File, temporary: &File) -> std::io::Result<()> {
    use nix::libc::{fgetxattr, fremovexattr, fsetxattr, ENODATA, ENOTSUP};

    let name = c"system.posix_acl_access".as_ptr();
    let source = original.as_raw_fd();
    let target = temporary.as_raw_fd();
    let size = unsafe { fgetxattr(source, name, std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(ENODATA | ENOTSUP)) {
            return Err(error);
        }
        // Drop a directory-inherited ACL when the existing file has none.
        if unsafe { fremovexattr(target, name) } < 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(ENODATA | ENOTSUP)) {
                return Err(error);
            }
        }
        return Ok(());
    }

    let mut acl = vec![0u8; size as usize];
    let read = unsafe { fgetxattr(source, name, acl.as_mut_ptr().cast(), acl.len()) };
    if read < 0 || unsafe { fsetxattr(target, name, acl.as_ptr().cast(), read as usize, 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn preserve_access_acl(original: &File, temporary: &File) -> std::io::Result<()> {
    use nix::libc::{c_int, c_void, ENOENT, EOPNOTSUPP};

    unsafe extern "C" {
        fn acl_get_fd(fd: c_int) -> *mut c_void;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_set_fd(fd: c_int, acl: *mut c_void) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }

    let mut acl = unsafe { acl_get_fd(original.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(EOPNOTSUPP) => return Ok(()),
            Some(ENOENT) => acl = unsafe { acl_init(0) },
            _ => return Err(error),
        }
        if acl.is_null() {
            return Err(std::io::Error::last_os_error());
        }
    }
    let result = unsafe { acl_set_fd(temporary.as_raw_fd(), acl) };
    let error = (result < 0).then(std::io::Error::last_os_error);
    unsafe { acl_free(acl) };
    error.map_or(Ok(()), Err)
}

fn existing_writable_config(path: &Path) -> Result<Option<File>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open config file for writing failed: {error}")),
    };
    let metadata = config_metadata(&file, path)?;
    verify_replacement_owner(path, metadata.uid(), Uid::effective())?;
    Ok(Some(file))
}

fn verify_replacement_owner(path: &Path, owner: u32, caller: Uid) -> Result<(), String> {
    if !caller.is_root() && caller.as_raw() != owner {
        return Err(format!(
            "cannot replace config {} owned by uid {owner} as uid {}. Use a user-owned copy.",
            path.display(),
            caller.as_raw()
        ));
    }
    Ok(())
}

fn config_metadata(file: &File, path: &Path) -> Result<fs::Metadata, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect config file failed: {error}"))?;
    if !metadata.is_file() {
        return Err(format!(
            "config path is not a regular file: {}",
            path.display()
        ));
    }
    Ok(metadata)
}

fn verify_directory(directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(format!(
                    "config directory is not a directory: {}",
                    directory.display()
                ));
            }
            verify_owner_and_mode(&metadata, directory)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = directory.parent().ok_or_else(|| {
                format!("config directory has no parent: {}", directory.display())
            })?;
            let metadata = fs::symlink_metadata(parent)
                .map_err(|error| format!("read config directory failed: {error}"))?;
            verify_owner_and_mode(&metadata, parent)?;
            match DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    verify_directory(directory)
                }
                Err(error) => Err(format!("create config directory failed: {error}")),
            }
        }
        Err(error) => Err(format!("read config directory failed: {error}")),
    }
}

fn verify_document(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(format!(
                    "config path is not a regular file: {}",
                    path.display()
                ));
            }
            verify_owner_and_mode(&metadata, path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("read config file failed: {error}")),
    }
}

fn verify_owner_and_mode(metadata: &fs::Metadata, path: &Path) -> Result<(), String> {
    let euid = unsafe { nix::libc::geteuid() };
    if metadata.uid() != euid {
        return Err(format!(
            "{} belongs to uid {}, not the current user {euid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & WRITE_BITS != 0 {
        return Err(format!(
            "{} is writable by group or others; refusing to write",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ordinary_replacement_requires_ownership_for_regular_users() {
        let path = Path::new("config.toml");
        let caller = Uid::from_raw(1000);
        let error = verify_replacement_owner(path, 1001, caller).expect_err("different owner");
        assert!(error.contains("user-owned copy"), "{error}");
        assert!(verify_replacement_owner(path, 1000, caller).is_ok());
        assert!(verify_replacement_owner(path, 1001, Uid::from_raw(0)).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ordinary_lock_created_by_root_belongs_to_config_directory_owner() {
        use nix::unistd::chown;

        if !Uid::effective().is_root() {
            return;
        }
        let directory = tempfile::tempdir().expect("temp dir");
        let owner = Uid::from_raw(65534);
        chown(directory.path(), Some(owner), None).expect("assign test directory owner");
        let _lock = lock(&directory.path().join("config.toml"), false).expect("acquire lock");
        let metadata = fs::metadata(directory.path().join(".config.toml.lock")).expect("lock info");
        assert_eq!(metadata.uid(), owner.as_raw());
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }

    #[test]
    fn writes_a_new_document_without_leaving_a_temporary_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        write(&path, "[ui]\nlanguage = \"en-US\"\n", true).expect("write");
        assert!(path.is_file());
        let extras = fs::read_dir(directory.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "config.toml")
            .count();
        assert_eq!(extras, 0);
    }

    #[test]
    fn a_symlinked_document_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let real = directory.path().join("real.toml");
        fs::write(&real, "[ui]\n").expect("write real");
        let link = directory.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(write(&link, "[ui]\n", true).is_err());
        assert_eq!(fs::read_to_string(&real).expect("read"), "[ui]\n");
    }

    #[test]
    fn a_group_writable_document_is_refused_but_0644_is_allowed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[ui]\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(write(&path, "[ui]\nlanguage = \"zh-CN\"\n", true).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("chmod");
        assert!(write(&path, "[ui]\n", true).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn ordinary_write_preserves_existing_access_acl() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[ai]\n").expect("seed config");
        let original = File::open(&path).expect("open config");
        let name = c"system.posix_acl_access".as_ptr();
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (1u16, 6u16, u32::MAX),
            (2, 4, 1001),
            (4, 0, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&perm.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        assert_eq!(unsafe {
            nix::libc::fsetxattr(original.as_raw_fd(), name, acl.as_ptr().cast(), acl.len(), 0)
        }, 0);

        write(&path, "[ui]\nlanguage = \"en-US\"\n", false).expect("atomic save");

        let current = File::open(path).expect("open replacement");
        let mut copied = vec![0u8; acl.len()];
        let size = unsafe {
            nix::libc::fgetxattr(current.as_raw_fd(), name, copied.as_mut_ptr().cast(), copied.len())
        };
        assert_eq!(size, acl.len() as isize);
        assert_eq!(copied, acl);
    }

    #[test]
    fn a_missing_directory_is_created_private() {
        let directory = tempfile::tempdir().expect("temp dir");
        let nested = directory.path().join(".copilot-shell");
        let path = nested.join("config.toml");
        write(&path, "[ui]\n", true).expect("write");
        let mode = fs::metadata(&nested)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}
}
