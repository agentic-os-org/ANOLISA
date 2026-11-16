//! Core admission for the wizard.
//!
//! The wizard talks to a real cosh-core binary it can identify. A PATH lookup,
//! a script, or an override that points at something else is refused instead of
//! being executed under the `core` name.

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Missing,
    Untrusted,
}

pub(crate) fn admit() -> Result<String, AdmissionError> {
    let program = resolve_program().ok_or(AdmissionError::Missing)?;
    verify_binary(&program)?;
    crate::adapter::admit_session_assets(Some(&program));
    Ok(program)
}

fn resolve_program() -> Option<String> {
    if let Some(configured) = std::env::var_os("COSH_CORE_PATH") {
        let path = PathBuf::from(configured);
        return resolve_path(&path).map(|path| path.to_string_lossy().into_owned());
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            let sibling = directory.join("cosh-core");
            if sibling.is_file() {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    resolve_path(Path::new("cosh-core")).map(|path| path.to_string_lossy().into_owned())
}

fn resolve_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    if path.components().count() > 1 {
        return path
            .is_file()
            .then(|| path.canonicalize().unwrap_or(path.to_path_buf()));
    }
    let search = std::env::var_os("PATH")?;
    std::env::split_paths(&search)
        .map(|directory| directory.join(path))
        .find(|candidate| candidate.is_file())
}

fn verify_binary(program: &str) -> Result<(), AdmissionError> {
    use std::os::unix::fs::MetadataExt;

    let path = Path::new(program);
    let metadata = std::fs::symlink_metadata(path).map_err(|_| AdmissionError::Missing)?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return Err(AdmissionError::Untrusted);
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(AdmissionError::Untrusted);
    }
    let euid = unsafe { nix::libc::geteuid() };
    if metadata.uid() != euid && metadata.uid() != 0 {
        return Err(AdmissionError::Untrusted);
    }
    let mut magic = [0u8; 4];
    {
        use std::io::Read;
        let mut file = std::fs::File::open(path).map_err(|_| AdmissionError::Untrusted)?;
        file.read_exact(&mut magic)
            .map_err(|_| AdmissionError::Untrusted)?;
    }
    if !is_native_executable(&magic) {
        return Err(AdmissionError::Untrusted);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_native_executable(magic: &[u8; 4]) -> bool {
    magic == b"\x7fELF"
}

#[cfg(target_os = "macos")]
fn is_native_executable(magic: &[u8; 4]) -> bool {
    matches!(
        magic,
        b"\xcf\xfa\xed\xfe" | b"\xce\xfa\xed\xfe" | b"\xca\xfe\xba\xbe" | b"\xbe\xba\xfe\xca"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_native_executable(_magic: &[u8; 4]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_program(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        std::fs::write(&path, bytes).expect("write program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod program");
        path
    }

    #[test]
    fn a_shell_script_is_not_admitted_as_core() {
        let directory = tempfile::tempdir().expect("temp dir");
        let script = write_program(directory.path(), "cosh-core", b"#!/bin/sh\nexec cosh\n");
        assert_eq!(
            verify_binary(&script.to_string_lossy()),
            Err(AdmissionError::Untrusted)
        );
    }

    #[test]
    fn a_world_writable_binary_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_program(directory.path(), "cosh-core", b"\x7fELFrest");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert_eq!(
            verify_binary(&path.to_string_lossy()),
            Err(AdmissionError::Untrusted)
        );
    }

    #[test]
    fn a_missing_program_is_reported_as_missing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("absent-core");
        assert_eq!(
            verify_binary(&path.to_string_lossy()),
            Err(AdmissionError::Missing)
        );
    }

    #[test]
    fn a_truncated_binary_is_not_admitted() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write_program(directory.path(), "cosh-core", b"\x7fE");
        assert_eq!(
            verify_binary(&path.to_string_lossy()),
            Err(AdmissionError::Untrusted)
        );
    }
}
