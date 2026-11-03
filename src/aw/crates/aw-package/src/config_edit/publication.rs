//! Locked snapshot comparison, private staging and atomic file replacement.

use crate::{filesystem as paths, require, Result};
use std::{
    ffi::CStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{fchown, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

pub(super) struct Snapshot {
    pub(super) bytes: Vec<u8>,
    identity: Identity,
    parent: File,
}

#[derive(PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
}

impl From<&Metadata> for Identity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        }
    }
}

pub(super) fn read(path: &Path) -> Result<Snapshot> {
    let parent = pin_parent(path)?;
    let anchored = descriptor_path(&parent).join(
        path.file_name()
            .ok_or_else(|| crate::Error::Invalid("configuration path has no file name".into()))?,
    );
    let mut file = open(&anchored, false)?;
    snapshot(&anchored, &mut file, &parent)
}

fn descriptor_path(directory: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

fn pin_parent(path: &Path) -> Result<File> {
    paths::parent(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::Error::Invalid("configuration path has no parent".into()))?;
    let expected = parent.metadata()?;
    let mut held = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")?;
    for component in parent.components() {
        if let Component::Normal(name) = component {
            held = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(descriptor_path(&held).join(name))?;
        }
    }
    let current = held.metadata()?;
    require(
        current.dev() == expected.dev() && current.ino() == expected.ino(),
        "configuration parent changed while opening",
    )?;
    check_parent(&held)?;
    Ok(held)
}

fn check_parent(parent: &File) -> Result<()> {
    let metadata = parent.metadata()?;
    // SAFETY: geteuid reads the effective identity without side effects.
    require(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0,
        "configuration parent must be owned and not group/world writable",
    )?;
    reject_native_acls(parent)
}

fn open(path: &Path, writable: bool) -> Result<File> {
    // O_NONBLOCK prevents a substituted FIFO from blocking before metadata checks.
    let file = OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    checked_metadata(&file)?;
    Ok(file)
}

fn checked_metadata(file: &File) -> Result<Metadata> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid reads the effective identity without side effects.
    require(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o7000 == 0
            && metadata.nlink() == 1,
        "configuration must be an owned regular file without writable peers or hard links",
    )?;
    Ok(metadata)
}

fn snapshot(path: &Path, file: &mut File, parent: &File) -> Result<Snapshot> {
    check_parent(parent)?;
    let identity = Identity::from(&checked_metadata(file)?);
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() <= aw_config::MAX_DOCUMENT_BYTES,
        "configuration exceeds the document byte limit",
    )?;
    reject_acl(file, c"system.posix_acl_access")?;
    reject_native_acls(file)?;
    require(
        identity == Identity::from(&checked_metadata(file)?)
            && identity == Identity::from(&fs::symlink_metadata(path)?),
        "configuration changed while reading; reopen the current file",
    )?;
    Ok(Snapshot {
        bytes,
        identity,
        parent: parent.try_clone()?,
    })
}

fn reject_native_acls(file: &File) -> Result<()> {
    // Native ACLs cannot be reduced to the mode/GID contract of this editor.
    for attribute in [
        c"system.nfs4_acl",
        c"system.nfs4_dacl",
        c"system.richacl",
        c"system.cifs_acl",
        c"system.cifs_ntsd",
        c"system.cifs_ntsd_full",
        c"system.smb3_acl",
        c"system.smb3_ntsd",
        c"system.smb3_ntsd_full",
    ] {
        reject_acl(file, attribute)?;
    }
    Ok(())
}

fn reject_acl(file: &File, attribute: &CStr) -> Result<()> {
    // O_PATH directory descriptors do not support fgetxattr.
    let path = format!("/proc/self/fd/{}\0", file.as_raw_fd());
    // SAFETY: file owns a live descriptor; the generated ASCII path is NUL-terminated,
    // and a null buffer with zero size queries length through the trusted procfs link.
    let acl = unsafe {
        libc::getxattr(
            path.as_ptr().cast(),
            attribute.as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    let error = std::io::Error::last_os_error();
    require(
        acl < 0,
        format!("configuration must not have an extended access ACL ({attribute:?})"),
    )?;
    if !matches!(error.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP)) {
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn publish(
    path: &Path,
    original: &Snapshot,
    bytes: Option<&[u8]>,
) -> Result<Option<Snapshot>> {
    publish_using(path, original, bytes, |file, bytes| {
        file.write_all(bytes)?;
        file.sync_all()
    })
}

fn publish_using(
    path: &Path,
    original: &Snapshot,
    bytes: Option<&[u8]>,
    write: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<Option<Snapshot>> {
    let parent = descriptor_path(&original.parent);
    let anchored = parent.join(
        path.file_name()
            .ok_or_else(|| crate::Error::Invalid("configuration path has no file name".into()))?,
    );
    let path = anchored.as_path();
    let changed = bytes.is_some();
    let mut held = open(path, changed)?;
    // NFS emulates exclusive flock with fcntl, which needs a writable descriptor.
    let lock = if changed {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    // SAFETY: held owns a live descriptor; its lock lasts through replacement.
    if unsafe { libc::flock(held.as_raw_fd(), lock | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    unchanged(path, &mut held, original)?;
    let Some(bytes) = bytes else { return Ok(None) };
    // Keep even group-readable contents inaccessible until the atomic rename.
    let staging = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&parent)?;
    let staging_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(staging.path())?;
    check_parent(&staging_directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(staging.path())?;
    // Inherited named ACL entries must not become readable when the mode is restored.
    // SAFETY: temporary owns a live descriptor and the attribute name is NUL-terminated.
    if unsafe {
        libc::fremovexattr(
            temporary.as_file().as_raw_fd(),
            c"system.posix_acl_access".as_ptr(),
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        // An absent ACL or a filesystem without ACL support has no entries to remove.
        if !matches!(error.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP)) {
            return Err(error.into());
        }
    }
    reject_native_acls(temporary.as_file())?;
    if temporary.as_file().metadata()?.gid() != original.identity.group {
        fchown(temporary.as_file(), None, Some(original.identity.group))?;
    }
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(original.identity.mode & 0o777))?;
    write(temporary.as_file_mut(), bytes)?;
    unchanged(path, &mut held, original)?;
    // Prepare the return state before the commit point: no error may follow rename.
    let identity = Identity::from(&temporary.as_file().metadata()?);
    let parent = original.parent.try_clone()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(Some(Snapshot {
        bytes: bytes.into(),
        identity,
        parent,
    }))
}

fn unchanged(path: &Path, held: &mut File, original: &Snapshot) -> Result<()> {
    let current = snapshot(path, held, &original.parent)?;
    require(
        current.identity == original.identity && current.bytes == original.bytes,
        "configuration changed since opening; reopen the current file",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_probes_support_pinned_directory_descriptors() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        let writable = File::open(directory.path()).unwrap();
        let pinned = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(directory.path())
            .unwrap();
        let attribute = c"user.aw-test-directory-acl";
        reject_acl(&pinned, attribute).unwrap();
        let bytes = b"directory-only ACE";
        // SAFETY: writable owns a live descriptor; both buffers remain live for the call.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    writable.as_raw_fd(),
                    attribute.as_ptr(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    0,
                )
            },
            0
        );
        assert!(reject_acl(&pinned, attribute)
            .unwrap_err()
            .to_string()
            .contains("access ACL"));
    }

    #[test]
    fn acl_presence_is_rejected_even_for_empty_attributes() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        let file = File::open(&path).unwrap();
        let attribute = c"user.aw-test-acl";
        reject_acl(&file, attribute).unwrap();
        for bytes in [b"".as_slice(), b"named ACE"] {
            // SAFETY: file owns a live descriptor; both buffers remain live for the call.
            assert_eq!(
                unsafe {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        attribute.as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        0,
                    )
                },
                0
            );
            assert!(reject_acl(&file, attribute)
                .unwrap_err()
                .to_string()
                .contains("access ACL"));
            assert_eq!(fs::read(&path).unwrap(), b"original");
        }
    }

    #[test]
    fn new_hard_links_during_staging_abort_publication() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        let alias = directory.path().join("backup.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = read(&path).unwrap();
        let mut held = open(&path, false).unwrap();
        fs::hard_link(&path, &alias).unwrap();
        assert!(snapshot(&path, &mut held, &original.parent).is_err());
        fs::remove_file(&alias).unwrap();
        let result = publish_using(&path, &original, Some(b"our edits"), |file, bytes| {
            file.write_all(bytes)?;
            fs::hard_link(&path, &alias)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read(&alias).unwrap(), b"original");
        assert_eq!(fs::metadata(&path).unwrap().ino(), original.identity.inode);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn mutations_use_write_capable_locks_and_noops_share_read_locks() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = read(&path).unwrap();
        let published = publish_using(&path, &original, Some(b"new document"), |file, bytes| {
            let held = fs::read_dir("/proc/self/fd")?
                .filter_map(|entry| entry.ok())
                .find(|entry| fs::read_link(entry.path()).ok().as_deref() == Some(path.as_path()))
                .unwrap();
            let descriptor = held.file_name().to_str().unwrap().parse::<i32>().unwrap();
            // SAFETY: the publication descriptor remains open throughout this callback.
            let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
            assert_eq!(flags & libc::O_ACCMODE, libc::O_RDWR);
            file.write_all(bytes)
        })
        .unwrap()
        .unwrap();
        let reader = File::open(&path).unwrap();
        // SAFETY: reader owns a live descriptor; its shared lock lasts until drop.
        assert_eq!(
            unsafe { libc::flock(reader.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        assert!(publish(&path, &published, None).unwrap().is_none());
        assert!(publish(&path, &published, Some(b"blocked edit")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new document");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_staging_preserves_original_and_removes_temporary_file() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let original = read(&path).unwrap();
        let result = publish_using(&path, &original, Some(b"new document"), |file, _| {
            file.write_all(b"partial")?;
            for entry in fs::read_dir(directory.path())? {
                let entry = entry?;
                if entry.path() != path {
                    assert_eq!(entry.metadata()?.mode() & 0o077, 0);
                }
            }
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
        });
        assert!(
            matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(libc::ENOSPC))
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(read(&path).unwrap().identity == original.identity);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn noncooperating_replacement_during_staging_is_detected() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = read(&path).unwrap();
        let result = publish_using(&path, &original, Some(b"our edits"), |file, bytes| {
            file.write_all(bytes)?;
            let replacement = directory.path().join("other.yaml");
            fs::write(&replacement, b"another writer").unwrap();
            fs::rename(replacement, &path).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"another writer");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
