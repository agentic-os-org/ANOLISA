//! Resolves absolute filesystem paths for the local Gateway daemon.

use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use super::CliError;

pub(super) fn daemon_socket_path(explicit: Option<&PathBuf>) -> Result<PathBuf, CliError> {
    resolve_daemon_socket_path(
        explicit,
        std::env::var_os("COSH_GATEWAY_SOCKET"),
        live_packaged_daemon_socket(),
        std::env::var_os("XDG_RUNTIME_DIR"),
        nix::unistd::Uid::effective().as_raw(),
    )
}

fn resolve_daemon_socket_path(
    explicit: Option<&PathBuf>,
    configured: Option<OsString>,
    packaged: Option<PathBuf>,
    runtime: Option<OsString>,
    effective_uid: u32,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return require_absolute(path, "daemon socket");
    }
    if let Some(path) = configured {
        return require_absolute(&PathBuf::from(path), "COSH_GATEWAY_SOCKET");
    }
    if let Some(path) = packaged {
        return Ok(path);
    }
    if let Some(runtime) = runtime {
        return require_absolute(&PathBuf::from(runtime), "XDG_RUNTIME_DIR")
            .map(|path| path.join("cosh/gateway.sock"));
    }
    Ok(PathBuf::from(format!(
        "/run/user/{effective_uid}/cosh/gateway.sock"
    )))
}

fn live_packaged_daemon_socket() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
        .ok()
        .flatten()?;
    let path = packaged_daemon_socket_path(&user.name)?;
    path.metadata()
        .ok()
        .filter(|metadata| metadata.file_type().is_socket())
        .map(|_| path)
}

fn packaged_daemon_socket_path(user: &str) -> Option<PathBuf> {
    (!user.is_empty()
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then(|| PathBuf::from(format!("/run/cosh-gateway-{user}/gateway.sock")))
}

pub(super) fn daemon_database_path(explicit: Option<&PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return require_absolute(path, "daemon database");
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return require_absolute(&PathBuf::from(state), "XDG_STATE_HOME")
            .map(|path| path.join("cosh/gateway/state.db"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Daemon("absolute HOME is required".to_owned()))?;
    require_absolute(&home, "HOME").map(|path| path.join(".local/state/cosh/gateway/state.db"))
}

pub(super) fn require_absolute(path: &Path, label: &str) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(CliError::Daemon(format!("{label} path must be absolute")))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{packaged_daemon_socket_path, resolve_daemon_socket_path};

    #[test]
    fn packaged_socket_matches_the_systemd_instance_path() {
        assert_eq!(
            packaged_daemon_socket_path("alice"),
            Some(PathBuf::from("/run/cosh-gateway-alice/gateway.sock"))
        );
        assert_eq!(packaged_daemon_socket_path("../alice"), None);
    }

    #[test]
    fn live_packaged_socket_precedes_the_user_runtime_default() {
        let packaged = PathBuf::from("/run/cosh-gateway-alice/gateway.sock");
        let resolved = resolve_daemon_socket_path(
            None,
            None,
            Some(packaged.clone()),
            Some(OsString::from("/run/user/1000")),
            1000,
        )
        .expect("packaged socket");

        assert_eq!(resolved, packaged);
    }

    #[test]
    fn explicit_and_environment_sockets_keep_precedence() {
        let explicit = PathBuf::from("/tmp/explicit.sock");
        let packaged = PathBuf::from("/run/cosh-gateway-alice/gateway.sock");
        let resolved = resolve_daemon_socket_path(
            Some(&explicit),
            Some(OsString::from("/tmp/environment.sock")),
            Some(packaged),
            Some(OsString::from("/run/user/1000")),
            1000,
        )
        .expect("explicit socket");

        assert_eq!(resolved, explicit);
    }
}
