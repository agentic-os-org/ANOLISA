//! Ownership-checked writes for wizard-initiated configuration changes.
//!
//! Ordinary slash/CLI writers keep their historical behavior. The wizard asks
//! for the checked path, which verifies the directory and document owner at
//! the write boundary and refuses to follow a substituted link.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const WRITE_BITS: u32 = 0o022;

pub(super) fn write(path: &Path, content: &str) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("config path has no directory: {}", path.display()))?;
    verify_directory(directory)?;
    verify_document(path)?;
    let temporary = directory.join(format!(".config.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|error| format!("create config temporary failed: {error}"))?;
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(format!("write config file failed: {error}"));
    }
    drop(file);
    if let Err(error) = verify_directory(directory).and_then(|()| verify_document(path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("write config file failed: {error}")
    })
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
            fs::create_dir(directory)
                .map_err(|error| format!("create config directory failed: {error}"))?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("create config directory failed: {error}"))
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

    #[test]
    fn writes_a_new_document_without_leaving_a_temporary_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        write(&path, "[ui]\nlanguage = \"en-US\"\n").expect("write");
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
        assert!(write(&link, "[ui]\n").is_err());
        assert_eq!(fs::read_to_string(&real).expect("read"), "[ui]\n");
    }

    #[test]
    fn a_group_writable_document_is_refused_but_0644_is_allowed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        fs::write(&path, "[ui]\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(write(&path, "[ui]\nlanguage = \"zh-CN\"\n").is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("chmod");
        assert!(write(&path, "[ui]\n").is_err());
    }

    #[test]
    fn a_missing_directory_is_created_private() {
        let directory = tempfile::tempdir().expect("temp dir");
        let nested = directory.path().join(".copilot-shell");
        let path = nested.join("config.toml");
        write(&path, "[ui]\n").expect("write");
        let mode = fs::metadata(&nested)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}
}
