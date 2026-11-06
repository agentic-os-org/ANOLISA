//! Persist the producer scope independently of daemon generations and directory names.

use crate::{rejected, Result};
use aw_contracts::canonical;
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const NAME: &str = ".aw-audit-source.json";

pub(crate) fn read(directory: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(NAME))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                rejected("audit_source_missing")
            } else {
                rejected("audit_source_invalid")
            }
        })?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > 512
    {
        return Err(rejected("audit_source_invalid"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(513).read_to_end(&mut bytes)?;
    let value = canonical::parse(&bytes).map_err(|_| rejected("audit_source_invalid"))?;
    let id = value["sourceId"]
        .as_str()
        .filter(|id| {
            id.len() == 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| rejected("audit_source_invalid"))?;
    if value != json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditSource","sourceId":id}) {
        return Err(rejected("audit_source_invalid"));
    }
    Ok(id.to_owned())
}

pub(super) fn initialize(directory: &Path) -> Result<String> {
    match read(directory) {
        Ok(id) => return Ok(id),
        Err(crate::Error::Rejected(code)) if code == "audit_source_missing" => {}
        Err(error) => return Err(error),
    }
    let mut entropy = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut entropy)?;
    let id = canonical::digest(&entropy);
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    let value: Value = json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditSource","sourceId":id});
    temporary
        .write_all(&canonical::bytes(&value).map_err(|_| rejected("audit_source_invalid"))?)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(directory.join(NAME)) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error.into()),
    }
    // Persist both the installed descriptor and removal of its temporary link.
    File::open(directory)?.sync_all()?;
    read(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Barrier},
        thread,
    };

    #[test]
    fn concurrent_openers_retain_one_private_identity() {
        for _ in 0..10 {
            let root = tempfile::tempdir().unwrap();
            let gate = Arc::new(Barrier::new(8));
            let ids = thread::scope(|scope| {
                let workers: Vec<_> = (0..8)
                    .map(|_| {
                        let gate = Arc::clone(&gate);
                        let path = root.path();
                        scope.spawn(move || {
                            gate.wait();
                            initialize(path).unwrap()
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert!(ids.iter().all(|id| id == &ids[0]));
            assert_eq!(initialize(root.path()).unwrap(), ids[0]);
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn missing_corrupt_and_linked_descriptors_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            matches!(read(root.path()),Err(crate::Error::Rejected(code)) if code=="audit_source_missing")
        );
        initialize(root.path()).unwrap();
        let path = root.path().join(NAME);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(root.path()).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, root.path().join("alias")).unwrap();
        assert!(read(root.path()).is_err());
        fs::remove_file(root.path().join("alias")).unwrap();
        fs::write(&path, b"{}").unwrap();
        assert!(initialize(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(initialize(root.path()).is_err());
    }
}
