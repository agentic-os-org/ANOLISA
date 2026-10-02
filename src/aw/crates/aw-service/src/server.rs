//! Foreground service lifecycle; embedders own signals and the process supervisor.

use crate::{
    deadline,
    endpoint::{self, Endpoint},
    ipc, rejected,
    runtime::Runtime,
    Operation, Request, Response, Result, VERSION,
};
use std::{
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 64;

/// One fixed configuration served through a private same-user Unix socket.
/// Binding preparation executes trusted local commands with an explicit context.
/// This service does not install native Hooks or implement a sandbox boundary.
pub struct Server {
    endpoint: Endpoint,
    runtime: Arc<Runtime>,
}

impl Server {
    /// Validate configuration and acquire the private endpoint and journal.
    ///
    /// Explicit configured state/socket paths must match this invocation. `auto`
    /// delegates location selection to the launcher. Existing sockets are never
    /// removed automatically, including sockets left after a killed service.
    ///
    /// # Errors
    /// Rejects configuration, unsafe state paths, occupied endpoints or failed audit storage.
    pub fn bind(bytes: Vec<u8>, state_dir: impl AsRef<Path>) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(rejected("linux_required"));
        }
        let configuration = aw_config::Validator::new()?.parse(&bytes)?;
        let state_dir = state_dir.as_ref();
        let daemon = &configuration.as_value()["spec"]["daemon"];
        let socket = state_dir.join("aw.sock");
        for (field, path) in [("state_dir", state_dir), ("endpoint", socket.as_path())] {
            let configured = daemon[field]
                .as_str()
                .ok_or_else(|| rejected("daemon_path_invalid"))?;
            if configured != "auto" && Path::new(configured) != path {
                return Err(rejected("daemon_path_mismatch"));
            }
        }
        let endpoint = Endpoint::bind(state_dir)?;
        let runtime = Runtime::new(
            bytes,
            &state_dir.join("journal"),
            Arc::new(AtomicBool::new(false)),
        )?;
        Ok(Self { endpoint, runtime })
    }

    /// Private socket retained until shutdown and removed only if its inode still matches.
    pub fn socket_path(&self) -> PathBuf {
        self.endpoint.socket_path().to_owned()
    }

    /// Serve until a stop request or the caller's cancellation flag is set.
    ///
    /// Shutdown cancels Provider work, joins every connection/event worker and
    /// retains durable audit data. The caller must not reap this process's children
    /// elsewhere. Filesystem synchronization and executor cleanup are outside hard
    /// real-time guarantees. No signal handlers are installed by the library.
    ///
    /// # Errors
    /// Reports accept, worker or audit failure after attempting owned cleanup.
    pub fn run(self, cancelled: &AtomicBool) -> Result<()> {
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let result = (|| {
            while !cancelled.load(Ordering::Acquire)
                && !self.runtime.stopped.load(Ordering::Acquire)
            {
                if !self.runtime.audit.healthy() {
                    return Err(rejected("audit_unavailable"));
                }
                self.runtime.maintain(false)?;
                let mut active = Vec::new();
                let mut failed = false;
                for worker in workers.drain(..) {
                    if worker.is_finished() {
                        failed |= worker.join().is_err();
                    } else {
                        active.push(worker);
                    }
                }
                workers = active;
                if failed {
                    return Err(rejected("connection_worker_failed"));
                }
                match self.endpoint.listener().accept() {
                    Ok((stream, _)) => {
                        if workers.len() >= MAX_CONNECTIONS || endpoint::same_user(&stream).is_err()
                        {
                            continue;
                        }
                        let runtime = Arc::clone(&self.runtime);
                        workers.push(
                            thread::Builder::new()
                                .name("aw-rpc".into())
                                .spawn(move || connection(stream, runtime))?,
                        );
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })();
        // First cancel existing work, then join RPC handlers that might still be preparing.
        self.runtime.stopped.store(true, Ordering::Release);
        let shutdown = self.runtime.maintain(true);
        let mut failed = false;
        for worker in workers {
            failed |= worker.join().is_err();
        }
        let final_shutdown = self.runtime.maintain(true);
        result?;
        shutdown?;
        final_shutdown?;
        if failed {
            return Err(rejected("connection_worker_failed"));
        }
        if !self.runtime.audit.healthy() {
            return Err(rejected("audit_unavailable"));
        }
        Ok(())
    }
}

fn connection(mut stream: UnixStream, runtime: Arc<Runtime>) {
    let initial = Instant::now() + Duration::from_secs(1);
    let Ok(request) = ipc::read::<Request>(&mut stream, initial) else {
        return;
    };
    let deadline = deadline::decode(request.deadline_ns);
    let expires = deadline.as_ref().copied().unwrap_or(initial);
    let binding = matches!(&request.operation, Operation::Bind { .. });
    let result = (|| {
        if request.api_version != VERSION {
            return Err(rejected("protocol_version"));
        }
        if request.identity.as_ref() != Some(&runtime.identity)
            && !(request.identity.is_none()
                && matches!(request.operation, crate::Operation::Status))
        {
            return Err(rejected("stale_service"));
        }
        let deadline = deadline?;
        if binding {
            prepare_connected(&stream, &runtime, request.operation, deadline)
        } else {
            runtime.handle(request.operation, deadline, &runtime.stopped)
        }
    })();
    let (result, error, audit_key) = match result {
        Ok(value) => (Some(value), None, None),
        Err(crate::Error::Rejected(code)) => (None, Some(code), None),
        Err(crate::Error::Attempt { code, audit_key }) => (None, Some(code), Some(audit_key)),
        Err(_) => (None, Some("service_failure".into()), None),
    };
    let response = Response {
        api_version: VERSION.into(),
        identity: runtime.identity.clone(),
        result,
        error,
        audit_key,
    };
    if ipc::write(&mut stream, &response, expires).is_err() && binding {
        if let Some(instance_id) = response
            .result
            .as_ref()
            .and_then(|value| value["instance_id"].as_str())
        {
            let _ = runtime.handle(
                Operation::Unbind {
                    instance_id: instance_id.into(),
                },
                Instant::now() + Duration::from_secs(1),
                &runtime.stopped,
            );
        }
    }
}

fn prepare_connected(
    stream: &UnixStream,
    runtime: &Arc<Runtime>,
    operation: Operation,
    deadline: Instant,
) -> Result<serde_json::Value> {
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = thread::Builder::new()
            .name("aw-prepare".into())
            .spawn_scoped(scope, || runtime.handle(operation, deadline, &cancelled))?;
        while !worker.is_finished() {
            let mut peer = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLRDHUP,
                revents: 0,
            };
            // SAFETY: poll borrows one live socket descriptor and never consumes its data.
            let result = unsafe { libc::poll(&mut peer, 1, 20) };
            if runtime.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
                || peer.revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                    != 0
                || (result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
            {
                cancelled.store(true, Ordering::Release);
                break;
            }
        }
        worker
            .join()
            .map_err(|_| rejected("binding_worker_failed"))?
    })
}
