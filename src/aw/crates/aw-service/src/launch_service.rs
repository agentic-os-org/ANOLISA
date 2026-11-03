//! Resolve or start one same-user AW service without coupling its life to an Agent.

mod files;
mod process;
#[cfg(test)]
mod tests;

use crate::{rejected, Client, Operation, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Component, Path, PathBuf},
    process::Child,
    time::Instant,
};

/// Validated service locations and the exact configuration revision they select.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServicePaths {
    /// Private directory containing the service socket, journal and startup files.
    pub state_dir: PathBuf,
    /// Fixed `aw.sock` endpoint supported by the local service contract.
    pub socket: PathBuf,
    /// SHA-256 of the original bytes, including whitespace.
    pub config_revision: String,
    namespace: Option<PathBuf>,
    on_demand: bool,
}

/// A service whose protocol, generation and full configuration revision were checked.
///
/// Dropping this value never stops the daemon. Retain it until the launcher exits
/// so an already-exited child can be reaped; long-lived callers should explicitly
/// stop and [`Self::reap_started`] services they choose to retire.
pub struct EnsuredService {
    /// Reusable client pinned to the verified service generation.
    pub client: Client,
    /// Locations used by status, stop and audit tools.
    pub paths: ServicePaths,
    /// Whether this invocation created the daemon process.
    pub started: bool,
    /// PID confirmed by both the live service and, when started, the owned child.
    pub pid: u32,
    child: Option<Child>,
}

impl EnsuredService {
    /// Reap this invocation's child after separately requesting service shutdown.
    ///
    /// Does not signal a process. Reused external services have no child to reap.
    ///
    /// # Errors
    /// Reports a wait failure or a child still running at the supplied deadline.
    pub fn reap_started(&mut self, deadline: Instant) -> Result<()> {
        if let Some(child) = &mut self.child {
            process::wait(child, deadline)?;
            self.child = None;
        }
        Ok(())
    }
}

impl Drop for EnsuredService {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.try_wait();
        }
    }
}

/// Resolve desired service locations without creating files or starting processes.
///
/// `runtime_dir` is the caller's `XDG_RUNTIME_DIR`; `None` selects `/tmp/aw-UID`.
/// An explicitly supplied runtime directory must already be private and owned by
/// the current user. Auto names use 24 hexadecimal revision characters to fit a
/// Unix socket path; adoption still compares all 64 revision characters.
/// An explicit endpoint must be `aw.sock`; when state is `auto`, its parent selects
/// the state directory. Two explicit paths must refer to that same endpoint.
///
/// # Errors
/// Rejects invalid configuration, nonabsolute or overlong paths and unsafe runtime
/// directories. An existing state directory is checked later, before connection.
pub fn resolve(bytes: &[u8], runtime_dir: Option<&Path>) -> Result<ServicePaths> {
    let configuration = aw_config::Validator::new()?.parse(bytes)?;
    let daemon = &configuration.as_value()["spec"]["daemon"];
    let field = |name: &str| {
        daemon[name]
            .as_str()
            .ok_or_else(|| rejected("daemon_path_invalid"))
    };
    let state = field("state_dir")?;
    let endpoint = field("endpoint")?;
    let revision = format!("{:x}", Sha256::digest(bytes));
    let (state_dir, namespace) = match (state, endpoint) {
        ("auto", "auto") => {
            let namespace = match runtime_dir {
                Some(directory) => {
                    absolute(directory)?;
                    files::check_private_directory(directory)?;
                    directory.join("aw")
                }
                None => PathBuf::from(format!("/tmp/aw-{}", files::uid())),
            };
            (namespace.join(&revision[..24]), Some(namespace))
        }
        ("auto", endpoint) => {
            let socket = Path::new(endpoint);
            absolute(socket)?;
            if socket.file_name() != Some(std::ffi::OsStr::new("aw.sock")) {
                return Err(rejected("daemon_endpoint_must_be_aw_sock"));
            }
            (
                socket
                    .parent()
                    .ok_or_else(|| rejected("daemon_path_invalid"))?
                    .to_owned(),
                None,
            )
        }
        (state, _) => (PathBuf::from(state), None),
    };
    absolute(&state_dir)?;
    let socket = state_dir.join("aw.sock");
    if endpoint != "auto" && Path::new(endpoint) != socket {
        return Err(rejected("daemon_path_mismatch"));
    }
    // sockaddr_un reserves its final byte for the pathname terminator.
    if socket.as_os_str().as_bytes().len() >= 108 {
        return Err(rejected("service_socket_path_too_long"));
    }
    Ok(ServicePaths {
        state_dir,
        socket,
        config_revision: revision,
        namespace,
        on_demand: field("startup")? == "on_demand",
    })
}

/// Reuse an exact matching daemon or start the configured on-demand service.
///
/// Startup is serialized with a private per-state lock. Configuration is copied
/// from these bytes into a private immutable snapshot before spawning `aw serve`.
/// Existing sockets are never unlinked, including stale sockets. The new child
/// has a separate session, null stdin/stdout and a private stderr log; it survives
/// Agent and launcher exit. External mode never creates files or starts a process.
///
/// # Errors
/// Rejects unsafe paths, mismatched revisions, unavailable external or stale
/// services, failed child startup and the caller's shared startup deadline.
/// Failed startup terminates and reaps only the process started by this call.
pub fn ensure_service(
    bytes: &[u8],
    executable: &Path,
    deadline: Instant,
) -> Result<EnsuredService> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty());
    let paths = resolve(bytes, runtime.as_deref().map(Path::new))?;
    process::remaining(deadline)?;
    if !paths.on_demand {
        files::check_parent(&paths.state_dir)?;
        files::check_private_directory(&paths.state_dir)?;
        return existing(paths, deadline);
    }
    if let Some(namespace) = &paths.namespace {
        files::private_directory(namespace)?;
    }
    files::check_parent(&paths.state_dir)?;
    files::private_directory(&paths.state_dir)?;
    let _lock = files::lock(&paths.state_dir, deadline)?;
    match fs::symlink_metadata(&paths.socket) {
        Ok(_) => existing(paths, deadline),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            absolute(executable)?;
            let snapshot = files::snapshot(&paths, bytes)?;
            let (client, pid, child) = process::start(executable, &paths, &snapshot, deadline)?;
            Ok(EnsuredService {
                client,
                paths,
                started: true,
                pid,
                child: Some(child),
            })
        }
        Err(error) => Err(error.into()),
    }
}

fn existing(paths: ServicePaths, deadline: Instant) -> Result<EnsuredService> {
    let (client, pid) = connect(&paths, deadline)?;
    Ok(EnsuredService {
        client,
        paths,
        started: false,
        pid,
        child: None,
    })
}

fn connect(paths: &ServicePaths, deadline: Instant) -> Result<(Client, u32)> {
    let metadata = fs::symlink_metadata(&paths.socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != files::uid()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(rejected("unsafe_service_socket"));
    }
    let client = Client::connect(&paths.socket, deadline)?;
    if client.identity().config_revision != paths.config_revision {
        return Err(rejected("service_revision_mismatch"));
    }
    let status = client.call(Operation::Status, deadline)?;
    let pid = status
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| rejected("invalid_service_pid"))?;
    if status["audit_healthy"] != true {
        return Err(rejected("service_audit_unavailable"));
    }
    Ok((client, pid))
}

fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(rejected("service_path_must_be_absolute"));
    }
    Ok(())
}

/// Literal `aw serve` arguments; no shell expansion or original config path is used.
fn arguments(paths: &ServicePaths, snapshot: &Path) -> [OsString; 5] {
    [
        "serve".into(),
        "--config".into(),
        snapshot.into(),
        "--state-dir".into(),
        paths.state_dir.clone().into(),
    ]
}
