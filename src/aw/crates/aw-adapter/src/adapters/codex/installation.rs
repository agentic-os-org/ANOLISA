//! Install stable, reviewable hook definitions without granting native trust.

use super::{hook_command, native_event};
use crate::Result;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

struct InstallationLock(fs::File);

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // An unrelated concurrent fork may retain the descriptor until exec.
        // SAFETY: the owned file keeps the locked descriptor valid through Drop.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn installation_lock(directory: &Path) -> Result<InstallationLock> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(".aw-install.lock"))?;
    let stat = lock.metadata()?;
    // SAFETY: geteuid reads identity; flock applies only to the owned descriptor.
    if !stat.is_file()
        || stat.uid() != unsafe { libc::geteuid() }
        || stat.mode() & 0o777 != 0o600
        || stat.nlink() != 1
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err("Codex AW installation lock is unsafe or already held".into());
    }
    Ok(InstallationLock(lock))
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(aw_exec::Error::Cancelled.into());
    }
    Ok(())
}

fn publish(candidate: &Path, live: &Path, original: &[u8], cancelled: &AtomicBool) -> Result<()> {
    check_cancelled(cancelled)?;
    let directory = live
        .parent()
        .ok_or("Codex hooks have no parent directory")?;
    let backup = directory.join("hooks.json.aw-backup");
    // Retain the displaced inode: native editors do not participate in our lock.
    fs::hard_link(candidate, &backup).map_err(|error| {
        format!("cannot reserve Codex hooks backup {}: {error}; review any retained backup before reinstalling", backup.display())
    })?;
    publish_reserved(&backup, live, original, cancelled)
}

fn publish_reserved(
    backup: &Path,
    live: &Path,
    original: &[u8],
    cancelled: &AtomicBool,
) -> Result<()> {
    let exchange = || -> Result<()> {
        let source = std::ffi::CString::new(backup.as_os_str().as_bytes())?;
        let destination = std::ffi::CString::new(live.as_os_str().as_bytes())?;
        check_cancelled(cancelled)?;
        // SAFETY: both terminated paths remain valid for the entire renameat2 call.
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    };
    if let Err(error) = exchange() {
        fs::remove_file(backup)?;
        return Err(error);
    }
    // Exchange commits publication; cancellation must not skip durable recovery.
    let verify = || -> Result<()> {
        let directory = live
            .parent()
            .ok_or("Codex hooks have no parent directory")?;
        fs::File::open(directory)?.sync_all()?;
        if read_owned(backup)? != original {
            return Err("hooks changed at publication".into());
        }
        Ok(())
    };
    verify().map_err(|error| format!(
        "Codex hooks were published, but {error}; displaced hooks retained at {}; review and reconcile before retrying",
        backup.display()
    ).into())
}

pub(super) fn read_owned(path: &Path) -> Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let stat = file.metadata()?;
    // SAFETY: geteuid only reads the current process identity.
    if !stat.is_file() || stat.uid() != unsafe { libc::geteuid() } || stat.mode() & 0o022 != 0 {
        return Err(
            "Codex integration files must be owned regular files, not writable by others".into(),
        );
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("Codex integration file exceeds one MiB".into());
    }
    Ok(bytes)
}

pub(super) fn install(cancelled: &AtomicBool) -> Result<()> {
    let directory = match std::env::var_os("CODEX_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".codex"),
    };
    install_in(&directory, cancelled)
}

fn install_in(directory: &Path, cancelled: &AtomicBool) -> Result<()> {
    check_cancelled(cancelled)?;
    if !directory.is_absolute() {
        return Err("Codex home must be absolute".into());
    }
    match fs::symlink_metadata(directory) {
        Ok(stat) => {
            // SAFETY: geteuid only reads the current process identity.
            if !stat.is_dir()
                || stat.uid() != unsafe { libc::geteuid() }
                || stat.mode() & 0o022 != 0
            {
                return Err("Codex home must be an owned directory, not writable by others".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    let _lock = installation_lock(directory)?;
    let backup = directory.join("hooks.json.aw-backup");
    match fs::symlink_metadata(&backup) {
        // Retained inodes may receive late editor writes even after verification.
        Ok(_) => return Err(format!(
            "Codex hooks need recovery review at {}; stop native Hook editors, reconcile with hooks.json, then move the backup away before reinstalling",
            backup.display()
        ).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    let path = directory.join("hooks.json");
    let original = match fs::symlink_metadata(&path) {
        Ok(_) => Some(read_owned(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut value = match &original {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)?,
        None => json!({"hooks":{}}),
    };
    let executable = std::env::current_exe()?;
    let mut changed = false;
    let hooks = value
        .as_object_mut()
        .ok_or("Codex hooks file must be an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid Codex hooks")?;
    for event in ["input.submit", "tool.before", "tool.after"] {
        let command = hook_command(&executable, event)?;
        let expected = if event == "input.submit" {
            json!({"hooks":[{"type":"command","command":command,"timeout":60}]})
        } else {
            json!({"matcher":".*","hooks":[{"type":"command","command":command,"timeout":60}]})
        };
        let groups = hooks
            .entry(native_event(event)?)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Codex hook groups must be arrays")?;
        let matching: Vec<_> = groups
            .iter()
            .filter(|group| {
                group["hooks"]
                    .as_array()
                    .is_some_and(|handlers| handlers.iter().any(|h| h["command"] == command))
            })
            .collect();
        if matching.is_empty() {
            groups.push(expected);
            changed = true;
        } else if matching.len() != 1 || matching[0] != &expected {
            return Err("existing AW Codex hook differs; review it before reinstalling".into());
        }
    }
    if changed {
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        if let Some(original) = original {
            if read_owned(&path)? != original {
                return Err("Codex hooks changed during installation".into());
            }
            publish(file.path(), &path, &original, cancelled)?;
        } else {
            check_cancelled(cancelled)?;
            file.persist_noclobber(&path).map_err(|error| error.error)?;
        }
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn installation_lock_serializes_aw_writers() {
        let directory = tempfile::tempdir().unwrap();
        let first = installation_lock(directory.path()).unwrap();
        assert!(installation_lock(directory.path()).is_err());
        drop(first);
        assert!(installation_lock(directory.path()).is_ok());
    }

    #[test]
    fn cancelled_installations_do_not_publish_or_leave_reservations() {
        let root = tempfile::tempdir().unwrap();
        let cancelled = AtomicBool::new(true);
        let home = root.path().join("home");
        assert!(install_in(&home, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(!home.exists());
        let live = root.path().join("hooks.json");
        let backup = root.path().join("hooks.json.aw-backup");
        let candidate = root.path().join("candidate");
        fs::write(&live, b"original").unwrap();
        fs::write(&candidate, b"candidate").unwrap();
        assert!(publish(&candidate, &live, b"original", &cancelled).is_err());
        assert!(!backup.exists());
        fs::hard_link(&candidate, &backup).unwrap();
        assert!(publish_reserved(&backup, &live, b"original", &cancelled)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(!backup.exists());
        assert_eq!(fs::read(&live).unwrap(), b"original");
        assert_eq!(fs::read(&candidate).unwrap(), b"candidate");
        cancelled.store(false, Ordering::Release);
        fs::remove_file(&live).unwrap();
        assert!(publish(&candidate, &live, b"original", &cancelled).is_err());
        assert!(!backup.exists());
        assert_eq!(fs::read(&candidate).unwrap(), b"candidate");
    }

    #[test]
    fn failed_publication_cannot_be_cleared_by_a_noop_install() {
        let home = tempfile::tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let cancelled = AtomicBool::new(false);
        install_in(home.path(), &cancelled).unwrap();
        let live = home.path().join("hooks.json");
        let candidate = home.path().join("candidate");
        let installed = fs::read(&live).unwrap();
        fs::copy(&live, &candidate).unwrap();
        fs::write(&live, b"concurrent user edit").unwrap();
        assert!(publish(&candidate, &live, b"original", &cancelled).is_err());
        let backup = home.path().join("hooks.json.aw-backup");
        for _ in 0..2 {
            let error = install_in(home.path(), &cancelled).unwrap_err().to_string();
            assert!(error.contains("recovery review"), "{error}");
            assert_eq!(fs::read(&live).unwrap(), installed);
            assert_eq!(fs::read(&backup).unwrap(), b"concurrent user edit");
        }
        fs::rename(&backup, home.path().join("reviewed-hooks.json")).unwrap();
        install_in(home.path(), &cancelled).unwrap();
        assert_eq!(fs::read(&live).unwrap(), installed);
    }

    #[test]
    fn publication_preserves_concurrent_edits_and_open_descriptors() {
        for concurrent in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let live = directory.path().join("hooks.json");
            fs::write(&live, b"original").unwrap();
            fs::set_permissions(&live, fs::Permissions::from_mode(0o600)).unwrap();
            let mut writer = fs::OpenOptions::new().write(true).open(&live).unwrap();
            let mut candidate = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
            candidate.write_all(b"candidate").unwrap();
            if concurrent {
                fs::write(&live, b"concurrent").unwrap();
            }
            let cancelled = AtomicBool::new(false);
            let result = publish(candidate.path(), &live, b"original", &cancelled);
            assert_eq!(result.is_err(), concurrent);
            let backup = directory.path().join("hooks.json.aw-backup");
            assert_eq!(
                fs::read(&backup).unwrap(),
                if concurrent {
                    b"concurrent".as_slice()
                } else {
                    b"original".as_slice()
                }
            );
            assert_eq!(fs::read(&live).unwrap(), b"candidate");
            writer.set_len(0).unwrap();
            writer.write_all(b"late editor write").unwrap();
            assert_eq!(fs::read(&backup).unwrap(), b"late editor write");
            assert_eq!(fs::read(&live).unwrap(), b"candidate");
            assert!(publish(candidate.path(), &live, b"candidate", &cancelled).is_err());
            assert_eq!(fs::read(&backup).unwrap(), b"late editor write");
        }
    }
}
