//! Pure local service path rules shared by deployment compilation and launchers.

use std::{
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

/// Invalid or unresolved local service locations, without supplied path values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Both paths need a runtime-specific location supplied by the launcher.
    #[error("daemon_paths_unresolved")]
    Unresolved,
    /// A path is relative, contains NUL or has a parent/current component.
    #[error("service_path_must_be_absolute")]
    Absolute,
    /// An explicit socket must use the service's fixed filename.
    #[error("daemon_endpoint_must_be_aw_sock")]
    EndpointName,
    /// Explicit state and socket paths identify different service locations.
    #[error("daemon_path_mismatch")]
    Mismatch,
    /// A socket pathname exceeds the Linux Unix socket capacity.
    #[error("service_socket_path_too_long")]
    TooLong,
}

/// Resolves `(state_dir, socket)` without inspecting files or the environment.
///
/// A launcher must replace one `auto` path before resolving `auto/auto`. Explicit
/// endpoints select their parent when state is `auto`; otherwise they must match
/// `state_dir/aw.sock`. Path comparison retains Rust's component normalization.
///
/// # Errors
/// Rejects unresolved, nonabsolute, conflicting or overlong service locations.
pub fn resolve(state: &Path, endpoint: &Path) -> Result<(PathBuf, PathBuf), Error> {
    let endpoint_auto = endpoint.as_os_str() == "auto";
    let state_dir = if state.as_os_str() == "auto" {
        if endpoint_auto {
            return Err(Error::Unresolved);
        }
        absolute(endpoint)?;
        if endpoint.file_name() != Some(std::ffi::OsStr::new("aw.sock")) {
            return Err(Error::EndpointName);
        }
        endpoint.parent().ok_or(Error::Absolute)?.to_owned()
    } else {
        state.to_owned()
    };
    absolute(&state_dir)?;
    let socket = state_dir.join("aw.sock");
    if !endpoint_auto && endpoint != socket {
        return Err(Error::Mismatch);
    }
    // sockaddr_un reserves its final byte for the pathname terminator.
    if socket.as_os_str().as_bytes().len() >= 108 {
        return Err(Error::TooLong);
    }
    Ok((state_dir, socket))
}

/// Validates an absolute path using the launcher's existing component semantics.
///
/// # Errors
/// Rejects relative paths, NUL bytes and parent/current directory components.
pub fn absolute(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Absolute);
    }
    Ok(())
}
