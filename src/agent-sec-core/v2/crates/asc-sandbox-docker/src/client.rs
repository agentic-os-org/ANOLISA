//! Isolate Docker client configuration and pin a trusted local endpoint.

use crate::{ContainerPlan, DockerConfig, DockerError};
use std::fs;
use std::io::{Read as _, Seek as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct Client {
    pub(crate) directory: PathBuf,
    temporary: Option<tempfile::TempDir>,
    preserve: std::cell::Cell<bool>,
    guard: Option<PathBuf>,
    pub(crate) host: String,
    cli: PathBuf,
    peer: Option<UnixStream>,
    daemon_root: Option<PathBuf>,
}

impl Client {
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn new(_config: &DockerConfig) -> Result<Self, DockerError> {
        Err(DockerError::Unsupported(
            "Docker sandbox requires Linux".into(),
        ))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn new(config: &DockerConfig) -> Result<Self, DockerError> {
        config.validate()?;
        let endpoint = fs::canonicalize(config.host.trim_start_matches("unix://"))?;
        trusted_path(&endpoint, true)?;
        let peer = UnixStream::connect(&endpoint)?;
        let credentials =
            rustix::net::sockopt::socket_peercred(&peer).map_err(std::io::Error::from)?;
        if !credentials.uid.is_root() {
            return Err(DockerError::Unsupported(
                "Docker daemon peer must be host root".into(),
            ));
        }
        let proc = daemon_process(credentials.pid.as_raw_nonzero().get(), &endpoint)?;
        let executable = fs::read_link(proc.join("exe"))?;
        if executable.file_name() != Some(std::ffi::OsStr::new("dockerd")) {
            return Err(DockerError::Unsupported(
                "endpoint must connect directly to local dockerd".into(),
            ));
        }
        trusted_path(&executable, false)?;
        if fs::read_link(proc.join("ns/user"))? != fs::read_link("/proc/self/ns/user")? {
            return Err(DockerError::Unsupported(
                "Docker daemon user namespace differs".into(),
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("client-")
            .tempdir_in(crate::mounts::trusted_root()?)?;
        fs::write(directory.path().join("config.json"), b"{}")?;
        let client = Self {
            directory: directory.path().to_owned(),
            temporary: Some(directory),
            preserve: std::cell::Cell::new(false),
            guard: config.guard.clone(),
            host: format!("unix://{}", endpoint.display()),
            cli: config.cli.clone(),
            peer: Some(peer),
            daemon_root: Some(proc.join("root")),
        };
        client.verify_visible(client.directory.as_path())?;
        Ok(client)
    }

    pub(crate) fn verify_mounts(&self, plan: &ContainerPlan) -> Result<(), DockerError> {
        for mount in &plan.mounts {
            let destination = Path::new(&mount.destination);
            let endpoint = Path::new(self.host.trim_start_matches("unix://"));
            if endpoint.starts_with(destination)
                || self.directory.starts_with(destination)
                || self
                    .guard
                    .as_ref()
                    .is_some_and(|path| path.starts_with(destination))
            {
                return Err(DockerError::Unsupported(
                    "mount exposes Docker control paths".into(),
                ));
            }
            self.verify_visible(Path::new(&mount.source))?;
        }
        Ok(())
    }

    fn verify_visible(&self, path: &Path) -> Result<(), DockerError> {
        if let Some(root) = &self.daemon_root {
            let local = fs::metadata(path)?;
            let remote = fs::metadata(
                root.join(
                    path.strip_prefix("/")
                        .map_err(|error| DockerError::Protocol(error.to_string()))?,
                ),
            )?;
            if local.dev() != remote.dev() || local.ino() != remote.ino() {
                return Err(DockerError::Unsupported(
                    "daemon cannot see identical staging objects".into(),
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.cli);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("--config")
            .arg(self.directory.as_path())
            .arg("--host")
            .arg(&self.host)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    pub(crate) fn preserve(&self) {
        self.preserve.set(true);
    }

    pub(crate) fn output(
        &self,
        mut command: Command,
        timeout: Duration,
    ) -> Result<std::process::Output, DockerError> {
        let mut stdout = tempfile::tempfile_in(&self.directory)?;
        let mut stderr = tempfile::tempfile_in(&self.directory)?;
        let mut child = command
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DockerError::Protocol(
                    "Docker CLI timed out; operation state unknown".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        stdout.rewind()?;
        stderr.rewind()?;
        let mut output = std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        stdout
            .take(8 * 1024 * 1024)
            .read_to_end(&mut output.stdout)?;
        stderr.take(4096).read_to_end(&mut output.stderr)?;
        Ok(output)
    }

    #[cfg(test)]
    pub(crate) fn scripted(config: &DockerConfig) -> Self {
        let directory = tempfile::tempdir().unwrap();
        Self {
            directory: directory.path().to_owned(),
            temporary: Some(directory),
            preserve: std::cell::Cell::new(false),
            guard: None,
            host: config.host.clone(),
            cli: config.cli.clone(),
            peer: None,
            daemon_root: None,
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Retain the connection for the lifecycle; a single socket stat cannot establish daemon identity.
        let _ = self.peer.take();
        if self.preserve.get()
            && let Some(directory) = self.temporary.take()
        {
            let _ = directory.keep();
        }
    }
}

#[cfg(target_os = "linux")]
fn daemon_process(peer_pid: i32, endpoint: &Path) -> Result<PathBuf, DockerError> {
    let mut proc = PathBuf::from(format!("/proc/{peer_pid}"));
    // With socket activation, SO_PEERCRED identifies PID 1, which created the listener.
    if peer_pid == 1
        && fs::read_link(proc.join("exe"))?.file_name() == Some(std::ffi::OsStr::new("systemd"))
    {
        let pidfile = Path::new("/run/docker.pid");
        trusted_path(pidfile, false)?;
        let pid: u32 = fs::read_to_string(pidfile)?
            .trim()
            .parse()
            .map_err(|_| DockerError::Unsupported("invalid local dockerd PID".into()))?;
        proc = PathBuf::from(format!("/proc/{pid}"));
        let sockets = fs::read_to_string(proc.join("net/unix"))?;
        let mut listener = false;
        for line in sockets.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() == 8
                && fields[3] == "00010000"
                && fs::canonicalize(fields[7]).is_ok_and(|path| path == endpoint)
            {
                let target = PathBuf::from(format!("socket:[{}]", fields[6]));
                listener = fs::read_dir(proc.join("fd"))?
                    .filter_map(Result::ok)
                    .any(|entry| fs::read_link(entry.path()).is_ok_and(|path| path == target));
                if listener {
                    break;
                }
            }
        }
        if !listener {
            return Err(DockerError::Unsupported(
                "cannot bind activated endpoint to dockerd".into(),
            ));
        }
    }
    Ok(proc)
}

#[cfg(target_os = "linux")]
pub(crate) fn trusted_path(path: &Path, socket: bool) -> Result<(), DockerError> {
    for (index, ancestor) in path.ancestors().enumerate() {
        let metadata = fs::symlink_metadata(ancestor)?;
        let forbidden = if socket && index == 0 { 0o002 } else { 0o022 };
        if metadata.uid() != 0
            || metadata.mode() & forbidden != 0
            || metadata.file_type().is_symlink()
            || (index > 0 && !metadata.is_dir())
        {
            return Err(DockerError::Unsupported(format!(
                "untrusted Docker control path: {}",
                ancestor.display()
            )));
        }
        if socket && index == 0 && !metadata.file_type().is_socket() {
            return Err(DockerError::Unsupported(
                "Docker endpoint is not a Unix socket".into(),
            ));
        }
    }
    Ok(())
}
