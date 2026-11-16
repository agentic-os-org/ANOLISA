//! Ownership-checked configuration writes for wizard-initiated changes.
//!
//! The guide refuses to write through a path it does not own, and a read
//! failure must never be mistaken for an empty document. Ordinary `/auth`
//! keeps the historical writer; only guarded callers use this path.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const WRITE_BITS: u32 = 0o022;

pub(super) fn verify_dir(dir: &Path) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(format!("config dir is not a directory: {}", dir.display()));
            }
            verify_metadata(&metadata, dir)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = dir
                .parent()
                .ok_or_else(|| format!("config dir has no parent: {}", dir.display()))?;
            let metadata = fs::symlink_metadata(parent)
                .map_err(|error| format!("cannot inspect {}: {error}", parent.display()))?;
            if !metadata.is_dir() {
                return Err(format!(
                    "config parent is not a directory: {}",
                    parent.display()
                ));
            }
            verify_metadata(&metadata, parent)?;
            fs::create_dir(dir)
                .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
            #[cfg(unix)]
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("cannot secure {}: {error}", dir.display()))?;
            Ok(())
        }
        Err(error) => Err(format!("cannot inspect {}: {error}", dir.display())),
    }
}

pub(super) fn verify_target(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(format!(
                    "config path is not a regular file: {}",
                    path.display()
                ));
            }
            verify_metadata(&metadata, path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

fn verify_metadata(metadata: &fs::Metadata, path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let euid = nix::unistd::geteuid().as_raw();
        if metadata.uid() != euid {
            return Err(format!(
                "{} is owned by uid {}, not the current user {euid}",
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
    }
    #[cfg(not(unix))]
    let _ = (metadata, path);
    Ok(())
}

/// Reads the current document without treating an unreadable file as empty.
pub(super) fn read_existing(path: &Path) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(content)
}

/// Replaces the document through a private, unpredictably named temporary file
/// after re-checking the directory and target at the replacement boundary.
pub(super) fn replace(dir: &Path, path: &Path, content: &str) -> Result<(), String> {
    let temporary = dir.join(format!("config.toml.{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("cannot create temporary config: {error}"))?;
    let written = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot write config: {error}"));
    }
    drop(file);
    #[cfg(unix)]
    if let Err(error) = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot secure config: {error}"));
    }
    if let Err(error) = verify_dir(dir).and_then(|()| verify_target(path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("cannot replace config: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_document_reads_as_empty_but_a_directory_does_not() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("config.toml");
        assert_eq!(read_existing(&missing).expect("missing read"), "");
        assert!(verify_target(dir.path()).is_err());
    }

    #[test]
    fn unreadable_document_is_not_silently_treated_as_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, [0xff, 0xfe, 0x00]).expect("write bytes");
        assert!(read_existing(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let real = dir.path().join("real.toml");
        fs::write(&real, "[ai]\n").expect("write real");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(verify_target(&link).is_err());
        assert!(read_existing(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn group_writable_document_is_refused_but_0644_is_accepted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "[ai]\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(verify_target(&path).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).expect("chmod");
        assert!(verify_target(&path).is_err());
    }

    #[test]
    fn replacement_keeps_content_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        replace(dir.path(), &path, "[ai]\nactive_provider = \"x\"\n").expect("replace");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "[ai]\nactive_provider = \"x\"\n"
        );
        let leftovers = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "config.toml")
            .count();
        assert_eq!(leftovers, 0);
    }
}
