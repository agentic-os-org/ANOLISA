use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncWriteExt as _;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout, timeout_at};

use crate::config::{ConfigError, ServiceConfig};
use crate::dispatcher::{
    DispatchControl, DispatchRequest, PeerCredentials, RejectedRequest, RejectionEncoder,
    RejectionReason, RequestDispatcher, ResponseDisposition,
};
use crate::frame::{BoundedResponseBuffer, FrameReadError, ResponseFrameError, read_request_frame};
use crate::shutdown::ShutdownToken;

/// A bound listener plus filesystem identity proving socket-path ownership.
pub struct BoundUnixSocket {
    listener: Option<UnixListener>,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl BoundUnixSocket {
    /// Binds an absolute, previously absent Unix socket path.
    ///
    /// The caller owns runtime-directory validation and stale-socket policy.
    /// This function never removes or replaces a pre-existing path.
    ///
    /// # Errors
    /// Returns a stable bind error for an unsafe mode, relative path, existing
    /// path, or operating-system failure.
    pub fn bind(path: impl AsRef<Path>, mode: u32) -> Result<Self, BindError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(BindError::RelativePath);
        }
        if mode & !0o777 != 0 || mode & 0o600 != 0o600 || mode & 0o111 != 0 {
            return Err(BindError::UnsafeMode);
        }
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(BindError::PathExists),
            Err(problem) if problem.kind() == io::ErrorKind::NotFound => {}
            Err(problem) => return Err(BindError::Io(problem)),
        }

        let listener = UnixListener::bind(path).map_err(BindError::Io)?;
        let metadata = fs::symlink_metadata(path).map_err(BindError::Io)?;
        let mut socket = Self {
            listener: Some(listener),
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Err(problem) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
            socket.close_ignoring_errors();
            return Err(BindError::Io(problem));
        }
        Ok(socket)
    }

    /// Returns the bound filesystem path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn listener(&self) -> Result<&UnixListener, ServeError> {
        self.listener.as_ref().ok_or(ServeError::ListenerClosed)
    }

    fn close(&mut self) -> Result<(), io::Error> {
        self.listener.take();
        self.remove_owned_path()
    }

    fn remove_owned_path(&self) -> Result<(), io::Error> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(problem) if problem.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(problem) => return Err(problem),
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    fn close_ignoring_errors(&mut self) {
        let _ = self.close();
    }
}

impl Drop for BoundUnixSocket {
    fn drop(&mut self) {
        self.close_ignoring_errors();
    }
}

/// Failure to bind the UDS service endpoint.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// System-owned daemon socket paths must be absolute.
    #[error("daemon socket path must be absolute")]
    RelativePath,
    /// Socket mode must grant owner read/write without execute or special bits.
    #[error("daemon socket mode must grant owner read/write without execute or special bits")]
    UnsafeMode,
    /// Stale/live path classification belongs to the process bootstrap.
    #[error("daemon socket path already exists")]
    PathExists,
    /// Operating-system bind, metadata, permission, or cleanup failure.
    #[error("daemon socket could not be bound")]
    Io(#[source] io::Error),
}

/// Protocol-independent UDS server composed with one request dispatcher.
pub struct UnixService {
    socket: BoundUnixSocket,
    config: ServiceConfig,
    dispatcher: Arc<dyn RequestDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
}

impl UnixService {
    /// Validates and creates a service around an already-owned listener.
    ///
    /// # Errors
    /// Returns an error when any configured resource bound is unusable.
    pub fn new(
        socket: BoundUnixSocket,
        config: ServiceConfig,
        dispatcher: Arc<dyn RequestDispatcher>,
        rejection_encoder: Arc<dyn RejectionEncoder>,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            socket,
            config,
            dispatcher,
            rejection_encoder,
        })
    }

    /// Accepts bounded connections until shutdown, then drains admitted tasks.
    ///
    /// Per-connection read, dispatch, encoding, and write failures are isolated
    /// and recorded in the returned report rather than terminating the listener.
    ///
    /// # Errors
    /// Returns only for listener ownership or socket cleanup failures.
    pub async fn serve(mut self, shutdown: ShutdownToken) -> Result<ServeReport, ServeError> {
        let normal_admission = Arc::new(Semaphore::new(self.config.max_connections));
        let rejection_admission = Arc::new(Semaphore::new(self.config.max_rejection_connections));
        let runtime = ConnectionRuntime {
            config: self.config.clone(),
            dispatcher: Arc::clone(&self.dispatcher),
            rejection_encoder: Arc::clone(&self.rejection_encoder),
            shutdown: shutdown.clone(),
            normal_admission,
            rejection_admission,
        };
        let mut tasks = JoinSet::new();
        let mut report = ServeReport::default();

        loop {
            if shutdown.is_requested() {
                break;
            }
            tokio::select! {
                biased;
                () = shutdown.wait() => break,
                completed = tasks.join_next(), if !tasks.is_empty() => {
                    record_task_result(&mut report, completed.as_ref());
                }
                accepted = self.socket.listener()?.accept() => {
                    match accepted {
                        Ok((stream, _address)) => {
                            report.accepted_connections += 1;
                            admit_connection(
                                stream,
                                &runtime,
                                &mut tasks,
                                &mut report,
                            );
                        }
                        Err(problem) if problem.kind() == io::ErrorKind::Interrupted => {}
                        Err(_problem) => {
                            report.accept_errors += 1;
                            tokio::select! {
                                biased;
                                () = shutdown.wait() => break,
                                () = sleep(self.config.accept_error_backoff) => {}
                            }
                        }
                    }
                }
            }
        }

        let cleanup_result = self.socket.close();
        drain_tasks(&mut tasks, self.config.drain_timeout, &mut report).await;
        cleanup_result.map_err(ServeError::Cleanup)?;
        Ok(report)
    }
}

struct ConnectionRuntime {
    config: ServiceConfig,
    dispatcher: Arc<dyn RequestDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
    shutdown: ShutdownToken,
    normal_admission: Arc<Semaphore>,
    rejection_admission: Arc<Semaphore>,
}

/// Listener-level service failure.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The owned listener was unexpectedly closed before serving started.
    #[error("daemon listener is closed")]
    ListenerClosed,
    /// The service could not remove its own socket inode.
    #[error("owned daemon socket could not be cleaned up")]
    Cleanup(#[source] io::Error),
}

/// Bounded service activity observed before controlled shutdown completed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeReport {
    /// Connections returned by the kernel listener.
    pub accepted_connections: u64,
    /// Complete request frames sent to normal dispatch.
    pub dispatched_requests: u64,
    /// Typed transport rejections rendered by the rejection encoder.
    pub rejected_requests: u64,
    /// Connections intentionally closed without a response.
    pub silently_closed_connections: u64,
    /// Isolated peer/read/dispatch/write/task failures.
    pub connection_failures: u64,
    /// Listener accept errors retried with backoff.
    pub accept_errors: u64,
    /// Admitted tasks aborted after the drain deadline.
    pub aborted_connections: u64,
}

fn admit_connection(
    stream: UnixStream,
    runtime: &ConnectionRuntime,
    tasks: &mut JoinSet<ConnectionOutcome>,
    report: &mut ServeReport,
) {
    let Ok(peer) = peer_credentials(&stream) else {
        report.silently_closed_connections += 1;
        report.connection_failures += 1;
        return;
    };

    if runtime.shutdown.is_requested() {
        spawn_rejection(
            stream,
            peer,
            RejectionReason::ShuttingDown,
            runtime,
            tasks,
            report,
        );
        return;
    }

    // TODO: isolate host-wide admission across peer UIDs and reserve administrator
    // capacity. Public UDS access currently lets one UID exhaust the global budget;
    // method authorization does not prevent this availability failure. Add a
    // cross-UID saturation regression with the separate ingress-control work.
    match Arc::clone(&runtime.normal_admission).try_acquire_owned() {
        Ok(permit) => {
            let config = runtime.config.clone();
            let dispatcher = Arc::clone(&runtime.dispatcher);
            let rejection_encoder = Arc::clone(&runtime.rejection_encoder);
            tasks.spawn(async move {
                process_connection(stream, peer, &config, dispatcher, rejection_encoder, permit)
                    .await
            });
        }
        Err(_) => spawn_rejection(stream, peer, RejectionReason::Busy, runtime, tasks, report),
    }
}

fn spawn_rejection(
    stream: UnixStream,
    peer: PeerCredentials,
    reason: RejectionReason,
    runtime: &ConnectionRuntime,
    tasks: &mut JoinSet<ConnectionOutcome>,
    report: &mut ServeReport,
) {
    let Ok(permit) = Arc::clone(&runtime.rejection_admission).try_acquire_owned() else {
        report.silently_closed_connections += 1;
        return;
    };
    let config = runtime.config.clone();
    let rejection_encoder = Arc::clone(&runtime.rejection_encoder);
    tasks.spawn(async move {
        process_rejection(stream, peer, reason, &config, rejection_encoder, permit).await
    });
}

async fn process_connection(
    stream: UnixStream,
    peer: PeerCredentials,
    config: &ServiceConfig,
    dispatcher: Arc<dyn RequestDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
    permit: OwnedSemaphorePermit,
) -> ConnectionOutcome {
    let outcome =
        process_admitted_connection(stream, peer, config, dispatcher, rejection_encoder).await;
    drop(permit);
    outcome
}

async fn process_admitted_connection(
    mut stream: UnixStream,
    peer: PeerCredentials,
    config: &ServiceConfig,
    dispatcher: Arc<dyn RequestDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
) -> ConnectionOutcome {
    let frame = match timeout(
        config.request_read_timeout,
        read_request_frame(&mut stream, config.max_request_frame_bytes),
    )
    .await
    {
        Err(_) => {
            return reject_on_stream(
                stream,
                peer,
                RejectionReason::RequestReadTimeout,
                config,
                rejection_encoder,
            )
            .await;
        }
        Ok(Err(FrameReadError::TooLarge)) => {
            return reject_on_stream(
                stream,
                peer,
                RejectionReason::RequestFrameTooLarge,
                config,
                rejection_encoder,
            )
            .await;
        }
        Ok(Err(FrameReadError::Empty)) => {
            return reject_on_stream(
                stream,
                peer,
                RejectionReason::EmptyRequest,
                config,
                rejection_encoder,
            )
            .await;
        }
        Ok(Err(FrameReadError::Io(_problem))) => return ConnectionOutcome::IoFailure,
        Ok(Ok(frame)) => frame,
    };

    // The budget is picked per request: an entry in the method-prefix table
    // overrides the interactive default, so a slow method family cannot
    // stretch everyone else's dispatch deadline.
    let budget = dispatch_budget(config, &frame);
    let control = DispatchControl::new(std::time::Instant::now() + budget);
    let request = DispatchRequest {
        peer,
        payload: frame,
        control: control.clone(),
    };
    match invoke_dispatch(
        dispatcher.clone(),
        request,
        control,
        config.max_response_frame_bytes,
        budget,
    )
    .await
    {
        DispatchAttempt::Frame(frame) => {
            write_frame(stream, frame, config.response_write_timeout, true).await
        }
        DispatchAttempt::Close => ConnectionOutcome::DispatchedClose,
        DispatchAttempt::Reject(reason) => {
            reject_on_stream(stream, peer, reason, config, rejection_encoder).await
        }
    }
}

async fn process_rejection(
    stream: UnixStream,
    peer: PeerCredentials,
    reason: RejectionReason,
    config: &ServiceConfig,
    rejection_encoder: Arc<dyn RejectionEncoder>,
    permit: OwnedSemaphorePermit,
) -> ConnectionOutcome {
    let outcome = reject_on_stream(stream, peer, reason, config, rejection_encoder).await;
    drop(permit);
    outcome
}

async fn reject_on_stream(
    stream: UnixStream,
    peer: PeerCredentials,
    reason: RejectionReason,
    config: &ServiceConfig,
    rejection_encoder: Arc<dyn RejectionEncoder>,
) -> ConnectionOutcome {
    let request = RejectedRequest { peer, reason };
    match invoke_rejection(
        rejection_encoder,
        request,
        config.max_response_frame_bytes,
        config.rejection_encode_timeout,
    )
    .await
    {
        DispatchAttempt::Frame(frame) => {
            write_frame(stream, frame, config.response_write_timeout, false).await
        }
        DispatchAttempt::Close | DispatchAttempt::Reject(_) => ConnectionOutcome::RejectedClose,
    }
}

/// Selects the dispatch budget for one decoded frame: the first entry in
/// [`ServiceConfig::method_dispatch_timeouts`] whose prefix matches the
/// request's method name, else the transport default. A frame without a
/// usable method name always takes the default.
fn dispatch_budget(config: &ServiceConfig, frame: &[u8]) -> std::time::Duration {
    let Some(method) = frame_method(frame) else {
        return config.dispatch_timeout;
    };
    config
        .method_dispatch_timeouts
        .iter()
        .find(|(prefix, _)| method.starts_with(prefix.as_str()))
        .map_or(config.dispatch_timeout, |(_, timeout)| *timeout)
}

/// Extracts the request's method name without decoding the whole payload:
/// serde skips the values of unknown fields, so this stays a linear scan.
/// Returns `None` for a malformed frame or a non-string method; the caller
/// then applies the default budget.
fn frame_method(frame: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct MethodField {
        method: String,
    }
    serde_json::from_slice::<MethodField>(frame)
        .ok()
        .map(|request| request.method)
}

async fn invoke_dispatch(
    dispatcher: Arc<dyn RequestDispatcher>,
    request: DispatchRequest,
    control: DispatchControl,
    maximum_wire_bytes: usize,
    dispatch_timeout: std::time::Duration,
) -> DispatchAttempt {
    let _cancellation = CancelDispatchOnDrop(control);
    let dispatch = tokio::task::spawn_blocking(move || {
        let mut response = BoundedResponseBuffer::new(maximum_wire_bytes);
        let result = dispatcher.dispatch(request, &mut response);
        finish_dispatch(result, response)
    });
    match timeout(dispatch_timeout, dispatch).await {
        Err(_) => DispatchAttempt::Reject(RejectionReason::DispatchTimedOut),
        Ok(Ok(attempt)) => attempt,
        Ok(Err(_)) => DispatchAttempt::Reject(RejectionReason::DispatchFailed),
    }
}

struct CancelDispatchOnDrop(DispatchControl);

impl Drop for CancelDispatchOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn invoke_rejection(
    rejection_encoder: Arc<dyn RejectionEncoder>,
    request: RejectedRequest,
    maximum_wire_bytes: usize,
    rejection_timeout: std::time::Duration,
) -> DispatchAttempt {
    let encoding = tokio::task::spawn_blocking(move || {
        let mut response = BoundedResponseBuffer::new(maximum_wire_bytes);
        let result = rejection_encoder.encode_rejection(request, &mut response);
        finish_dispatch(result, response)
    });
    match timeout(rejection_timeout, encoding).await {
        Ok(Ok(attempt)) => attempt,
        Ok(Err(_)) | Err(_) => DispatchAttempt::Close,
    }
}

fn finish_dispatch(
    result: Result<ResponseDisposition, crate::DispatchError>,
    response: BoundedResponseBuffer,
) -> DispatchAttempt {
    match response.failure() {
        Some(ResponseFrameError::TooLarge) => {
            return DispatchAttempt::Reject(RejectionReason::ResponseFrameTooLarge);
        }
        Some(
            ResponseFrameError::Empty
            | ResponseFrameError::ContainsDelimiter
            | ResponseFrameError::BytesBeforeClose,
        ) => return DispatchAttempt::Reject(RejectionReason::InvalidResponseFrame),
        None => {}
    }
    match result {
        Err(_) => DispatchAttempt::Reject(RejectionReason::DispatchFailed),
        Ok(ResponseDisposition::Close) => match response.finish_close() {
            Ok(()) => DispatchAttempt::Close,
            Err(_) => DispatchAttempt::Reject(RejectionReason::InvalidResponseFrame),
        },
        Ok(ResponseDisposition::Send) => match response.finish_send() {
            Ok(frame) => DispatchAttempt::Frame(frame),
            Err(ResponseFrameError::TooLarge) => {
                DispatchAttempt::Reject(RejectionReason::ResponseFrameTooLarge)
            }
            Err(
                ResponseFrameError::Empty
                | ResponseFrameError::ContainsDelimiter
                | ResponseFrameError::BytesBeforeClose,
            ) => DispatchAttempt::Reject(RejectionReason::InvalidResponseFrame),
        },
    }
}

async fn write_frame(
    mut stream: UnixStream,
    frame: Vec<u8>,
    write_timeout: std::time::Duration,
    dispatched: bool,
) -> ConnectionOutcome {
    let write = async {
        stream.write_all(&frame).await?;
        stream.shutdown().await
    };
    match timeout(write_timeout, write).await {
        Ok(Ok(())) if dispatched => ConnectionOutcome::DispatchedResponse,
        Ok(Ok(())) => ConnectionOutcome::RejectedResponse,
        Ok(Err(_)) | Err(_) => ConnectionOutcome::IoFailure,
    }
}

fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, ()> {
    let credentials = stream.peer_cred().map_err(|_| ())?;
    let pid = credentials.pid().ok_or(())?;
    let pid = u32::try_from(pid).map_err(|_| ())?;
    Ok(PeerCredentials::new(
        credentials.uid(),
        credentials.gid(),
        pid,
    ))
}

async fn drain_tasks(
    tasks: &mut JoinSet<ConnectionOutcome>,
    drain_timeout: std::time::Duration,
    report: &mut ServeReport,
) {
    let deadline = Instant::now() + drain_timeout;
    while !tasks.is_empty() {
        if let Ok(completed) = timeout_at(deadline, tasks.join_next()).await {
            record_task_result(report, completed.as_ref());
        } else {
            report.aborted_connections += u64::try_from(tasks.len()).unwrap_or(u64::MAX);
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            break;
        }
    }
}

fn record_task_result(
    report: &mut ServeReport,
    completed: Option<&Result<ConnectionOutcome, tokio::task::JoinError>>,
) {
    match completed {
        Some(Ok(ConnectionOutcome::DispatchedResponse)) => report.dispatched_requests += 1,
        Some(Ok(ConnectionOutcome::DispatchedClose)) => {
            report.dispatched_requests += 1;
            report.silently_closed_connections += 1;
        }
        Some(Ok(ConnectionOutcome::RejectedResponse)) => report.rejected_requests += 1,
        Some(Ok(ConnectionOutcome::RejectedClose)) => {
            report.rejected_requests += 1;
            report.silently_closed_connections += 1;
        }
        Some(Ok(ConnectionOutcome::IoFailure) | Err(_)) => {
            report.connection_failures += 1;
        }
        None => {}
    }
}

enum DispatchAttempt {
    Frame(Vec<u8>),
    Close,
    Reject(RejectionReason),
}

#[derive(Clone, Copy)]
enum ConnectionOutcome {
    DispatchedResponse,
    DispatchedClose,
    RejectedResponse,
    RejectedClose,
    IoFailure,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;

    static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    fn budget_config(overrides: &[(&str, Duration)]) -> ServiceConfig {
        ServiceConfig {
            dispatch_timeout: Duration::from_secs(5),
            method_dispatch_timeouts: overrides
                .iter()
                .map(|(prefix, timeout)| ((*prefix).to_owned(), *timeout))
                .collect(),
            max_request_frame_bytes: 1024,
            max_response_frame_bytes: 1024,
            max_connections: 4,
            max_rejection_connections: 2,
            rejection_encode_timeout: Duration::from_millis(250),
            request_read_timeout: Duration::from_secs(1),
            response_write_timeout: Duration::from_secs(1),
            drain_timeout: Duration::from_secs(1),
            accept_error_backoff: Duration::from_millis(10),
        }
    }

    #[test]
    fn a_matching_method_prefix_selects_its_override_budget() {
        let config = budget_config(&[("action.prompt_scan", Duration::from_secs(35))]);
        // The warmup method shares the family prefix, so it inherits the
        // model-backed budget without its own entry.
        for method in ["action.prompt_scan", "action.prompt_scan.warmup"] {
            let frame = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#);
            assert_eq!(
                dispatch_budget(&config, frame.as_bytes()),
                Duration::from_secs(35)
            );
        }
    }

    #[test]
    fn an_unmatched_method_keeps_the_default_budget() {
        let config = budget_config(&[("action.prompt_scan", Duration::from_secs(35))]);
        // A served method outside the family keeps the interactive default.
        let frame = br#"{"method":"action.code_scan"}"#;
        assert_eq!(dispatch_budget(&config, frame), Duration::from_secs(5));
        let frame = br#"{"method":"policy.bindings.create"}"#;
        assert_eq!(dispatch_budget(&config, frame), Duration::from_secs(5));
    }

    #[test]
    fn a_frame_without_a_usable_method_takes_the_default_budget() {
        let config = budget_config(&[("action.prompt_scan", Duration::from_secs(35))]);
        // Malformed frames and non-string method values fall back instead of
        // failing the request here; dispatch reports the decode error.
        for frame in [
            b"not json".as_slice(),
            br#"{"method":42}"#.as_slice(),
            br"{}".as_slice(),
        ] {
            assert_eq!(dispatch_budget(&config, frame), Duration::from_secs(5));
        }
    }

    #[test]
    fn the_first_matching_prefix_wins() {
        let config = budget_config(&[
            ("action.prompt_scan", Duration::from_secs(35)),
            ("action.prompt_scan.warmup", Duration::from_secs(60)),
        ]);
        let frame = br#"{"method":"action.prompt_scan.warmup"}"#;
        assert_eq!(dispatch_budget(&config, frame), Duration::from_secs(35));
    }

    #[test]
    fn frame_method_reads_only_the_method_field() {
        // Unknown fields (including a large text payload) are skipped
        // without being decoded into values.
        let frame = br#"{"method":"action.prompt_scan","params":{"text":"..."}}"#;
        assert_eq!(frame_method(frame).as_deref(), Some("action.prompt_scan"));
    }

    fn unique_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "asc-daemon-service-{label}-{}-{}",
            std::process::id(),
            DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[tokio::test]
    async fn dropping_socket_removes_only_its_owned_inode() {
        let directory = unique_directory("replacement");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("daemon.sock");
        let moved = directory.join("owned.sock");
        let socket = BoundUnixSocket::bind(&path, 0o660).unwrap();
        fs::rename(&path, &moved).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let replacement_inode = fs::symlink_metadata(&path).unwrap().ino();

        drop(socket);

        assert_eq!(
            fs::symlink_metadata(&path).unwrap().ino(),
            replacement_inode
        );
        drop(replacement);
        fs::remove_file(path).unwrap();
        fs::remove_file(moved).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn bind_refuses_existing_paths_and_unsafe_modes() {
        let directory = unique_directory("bind");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("daemon.sock");
        let socket = BoundUnixSocket::bind(&path, 0o660).unwrap();

        assert!(matches!(
            BoundUnixSocket::bind(&path, 0o660),
            Err(BindError::PathExists)
        ));
        drop(socket);
        let public_socket = BoundUnixSocket::bind(&path, 0o666).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
        drop(public_socket);
        assert!(matches!(
            BoundUnixSocket::bind(&path, 0o1666),
            Err(BindError::UnsafeMode)
        ));
        assert!(matches!(
            BoundUnixSocket::bind(&path, 0o466),
            Err(BindError::UnsafeMode)
        ));
        assert!(matches!(
            BoundUnixSocket::bind(&path, 0o760),
            Err(BindError::UnsafeMode)
        ));
        fs::remove_dir(directory).unwrap();
    }
}
