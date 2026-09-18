use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use asc_daemon_service::{
    BoundUnixSocket, DispatchError, DispatchRequest, RejectedRequest, RejectionEncoder,
    RejectionReason, RequestDispatcher, ResponseDisposition, ServeReport, ServiceConfig,
    ShutdownToken, UnixService,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct FakeDispatcher {
    requests: Mutex<Vec<DispatchRequest>>,
    hold_started: AtomicBool,
    hold_gate: (Mutex<bool>, Condvar),
}

impl FakeDispatcher {
    fn requests(&self) -> Vec<DispatchRequest> {
        self.requests.lock().unwrap().clone()
    }

    async fn wait_until_held(&self) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !self.hold_started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("hold request should reach the dispatcher");
    }

    fn release_hold(&self) {
        let (lock, ready) = &self.hold_gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
}

impl RequestDispatcher for FakeDispatcher {
    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn std::io::Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.requests.lock().unwrap().push(request.clone());
        match request.payload.as_slice() {
            b"close" => return Ok(ResponseDisposition::Close),
            b"fail" => return Err(DispatchError),
            b"large" => {
                response
                    .write_all(&[b'x'; 128])
                    .map_err(|_| DispatchError)?;
            }
            b"newline" => {
                response
                    .write_all(b"invalid\nresponse")
                    .map_err(|_| DispatchError)?;
            }
            b"hold" => {
                self.hold_started.store(true, Ordering::Release);
                let (lock, ready) = &self.hold_gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
                response.write_all(b"ok:hold").map_err(|_| DispatchError)?;
            }
            payload => {
                response.write_all(b"ok:").map_err(|_| DispatchError)?;
                response.write_all(payload).map_err(|_| DispatchError)?;
            }
        }
        Ok(ResponseDisposition::Send)
    }
}

struct FakeRejectionEncoder;

impl RejectionEncoder for FakeRejectionEncoder {
    fn encode_rejection(
        &self,
        request: RejectedRequest,
        response: &mut dyn std::io::Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        let label = match request.reason {
            RejectionReason::Busy => "reject:busy",
            RejectionReason::ShuttingDown => "reject:shutdown",
            RejectionReason::RequestReadTimeout => "reject:timeout",
            RejectionReason::RequestFrameTooLarge => "reject:request-too-large",
            RejectionReason::EmptyRequest => "reject:empty",
            RejectionReason::DispatchFailed => "reject:dispatch-failed",
            RejectionReason::DispatchTimedOut => "reject:dispatch-timeout",
            RejectionReason::ResponseFrameTooLarge => "reject:response-too-large",
            RejectionReason::InvalidResponseFrame => "reject:invalid-response",
        };
        response
            .write_all(label.as_bytes())
            .map_err(|_| DispatchError)?;
        Ok(ResponseDisposition::Send)
    }
}

#[derive(Default)]
struct BlockingRejectionEncoder {
    started: AtomicBool,
    gate: (Mutex<bool>, Condvar),
}

impl BlockingRejectionEncoder {
    fn release(&self) {
        let (lock, ready) = &self.gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
}

impl RejectionEncoder for BlockingRejectionEncoder {
    fn encode_rejection(
        &self,
        _request: RejectedRequest,
        _response: &mut dyn std::io::Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.started.store(true, Ordering::Release);
        let (lock, ready) = &self.gate;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
        Ok(ResponseDisposition::Close)
    }
}

struct RunningService {
    directory: PathBuf,
    socket_path: PathBuf,
    shutdown: ShutdownToken,
    task: tokio::task::JoinHandle<Result<ServeReport, asc_daemon_service::ServeError>>,
}

impl RunningService {
    async fn stop(self) -> ServeReport {
        self.shutdown.request();
        let report = self.task.await.unwrap().unwrap();
        assert!(!self.socket_path.exists());
        std::fs::remove_dir(self.directory).unwrap();
        report
    }
}

fn config() -> ServiceConfig {
    ServiceConfig {
        max_request_frame_bytes: 64,
        max_response_frame_bytes: 64,
        max_connections: 4,
        max_rejection_connections: 2,
        rejection_encode_timeout: Duration::from_millis(100),
        request_read_timeout: Duration::from_millis(250),
        dispatch_timeout: Duration::from_millis(250),
        response_write_timeout: Duration::from_millis(250),
        drain_timeout: Duration::from_millis(500),
        accept_error_backoff: Duration::from_millis(10),
    }
}

fn unique_directory(label: &str) -> PathBuf {
    // Root at /tmp rather than TMPDIR: a socket bound inside this directory must
    // fit sun_path, which caps the address at 104 bytes on macOS. TMPDIR there
    // is a ~49-byte /var/folders/... path that overruns the cap once this
    // labelled directory name and /daemon.sock are appended.
    let base = Path::new("/tmp");
    let base = if base.is_dir() {
        base.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    base.join(format!(
        "asc-daemon-service-e2e-{label}-{}-{}",
        std::process::id(),
        DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn start_service(
    label: &str,
    config: ServiceConfig,
    dispatcher: Arc<FakeDispatcher>,
) -> RunningService {
    start_service_with_rejection_encoder(label, config, dispatcher, Arc::new(FakeRejectionEncoder))
}

fn start_service_with_rejection_encoder(
    label: &str,
    config: ServiceConfig,
    dispatcher: Arc<FakeDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
) -> RunningService {
    let directory = unique_directory(label);
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("daemon.sock");
    let socket = BoundUnixSocket::bind(&socket_path, 0o660).unwrap();
    let request_dispatcher: Arc<dyn RequestDispatcher> = dispatcher;
    let service = UnixService::new(socket, config, request_dispatcher, rejection_encoder).unwrap();
    let shutdown = ShutdownToken::new();
    let task = tokio::spawn(service.serve(shutdown.clone()));
    RunningService {
        directory,
        socket_path,
        shutdown,
        task,
    }
}

async fn request(path: &PathBuf, payload: &[u8], eof_terminated: bool) -> Vec<u8> {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(payload).await.unwrap();
    if eof_terminated {
        stream.shutdown().await.unwrap();
    }
    let mut response = Vec::new();
    let mut reader = BufReader::new(stream);
    tokio::time::timeout(
        Duration::from_secs(1),
        reader.read_until(b'\n', &mut response),
    )
    .await
    .expect("service response should be bounded")
    .unwrap();
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lf_eof_and_coalesced_requests_reach_the_fake_dispatcher_with_kernel_peer() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let service = start_service("frames", config(), dispatcher.clone());

    assert_eq!(
        request(&service.socket_path, b"first\n", false).await,
        b"ok:first\n"
    );
    assert_eq!(
        request(&service.socket_path, b"second", true).await,
        b"ok:second\n"
    );
    assert_eq!(
        request(&service.socket_path, b"third\nignored\n", false).await,
        b"ok:third\n"
    );

    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 3);
    let requests = dispatcher.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.peer.pid() == std::process::id())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_client_does_not_block_a_healthy_request_and_receives_one_deadline() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.max_connections = 2;
    limits.request_read_timeout = Duration::from_millis(150);
    let service = start_service("idle", limits, dispatcher);
    let mut idle = UnixStream::connect(&service.socket_path).await.unwrap();

    assert_eq!(
        request(&service.socket_path, b"healthy\n", false).await,
        b"ok:healthy\n"
    );
    let mut idle_response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), idle.read_to_end(&mut idle_response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(idle_response, b"reject:timeout\n");

    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 1);
    assert_eq!(report.rejected_requests, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_admission_returns_busy_without_disrupting_later_requests() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.max_connections = 1;
    let service = start_service("busy", limits, dispatcher.clone());
    let held_path = service.socket_path.clone();
    let held = tokio::spawn(async move { request(&held_path, b"hold\n", false).await });
    dispatcher.wait_until_held().await;

    let busy = tokio::time::timeout(
        Duration::from_secs(1),
        request(&service.socket_path, b"busy\n", false),
    )
    .await;
    dispatcher.release_hold();
    assert_eq!(held.await.unwrap(), b"ok:hold\n");
    assert_eq!(
        busy.expect("busy response should not wait for normal dispatch"),
        b"reject:busy\n"
    );
    assert_eq!(
        request(&service.socket_path, b"after\n", false).await,
        b"ok:after\n"
    );

    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 2);
    assert_eq!(report.rejected_requests, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_blocked_dispatch_does_not_block_an_independent_healthy_request() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.max_connections = 2;
    limits.dispatch_timeout = Duration::from_secs(1);
    let service = start_service("dispatch-isolation", limits, dispatcher.clone());
    let held_path = service.socket_path.clone();
    let held = tokio::spawn(async move { request(&held_path, b"hold\n", false).await });
    dispatcher.wait_until_held().await;

    assert_eq!(
        request(&service.socket_path, b"healthy\n", false).await,
        b"ok:healthy\n"
    );
    dispatcher.release_hold();
    assert_eq!(held.await.unwrap(), b"ok:hold\n");

    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_rejection_encoder_is_bounded_and_does_not_block_dispatch() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let rejection_encoder = Arc::new(BlockingRejectionEncoder::default());
    let mut limits = config();
    limits.request_read_timeout = Duration::from_millis(20);
    limits.rejection_encode_timeout = Duration::from_millis(30);
    let service = start_service_with_rejection_encoder(
        "rejection-timeout",
        limits,
        dispatcher,
        rejection_encoder.clone(),
    );
    let mut idle = UnixStream::connect(&service.socket_path).await.unwrap();

    let mut idle_response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), idle.read_to_end(&mut idle_response))
        .await
        .expect("rejection encoding should have an independent deadline")
        .unwrap();
    assert!(idle_response.is_empty());
    assert!(rejection_encoder.started.load(Ordering::Acquire));
    assert_eq!(
        request(&service.socket_path, b"healthy\n", false).await,
        b"ok:healthy\n"
    );

    rejection_encoder.release();
    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 1);
    assert_eq!(report.rejected_requests, 1);
    assert_eq!(report.silently_closed_connections, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_deadline_cancels_lifetime_and_releases_connection_capacity() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.max_connections = 1;
    limits.dispatch_timeout = Duration::from_millis(50);
    let service = start_service("dispatch-timeout", limits, dispatcher.clone());
    let held_path = service.socket_path.clone();
    let held = tokio::spawn(async move { request(&held_path, b"hold\n", false).await });
    dispatcher.wait_until_held().await;

    assert_eq!(held.await.unwrap(), b"reject:dispatch-timeout\n");
    let held_request = dispatcher
        .requests()
        .into_iter()
        .find(|request| request.payload == b"hold")
        .unwrap();
    assert!(held_request.control.is_cancelled());
    assert_eq!(
        request(&service.socket_path, b"after\n", false).await,
        b"ok:after\n"
    );

    dispatcher.release_hold();
    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 1);
    assert_eq!(report.rejected_requests, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_and_response_frame_failures_are_bounded_and_connection_local() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.max_request_frame_bytes = 8;
    limits.max_response_frame_bytes = 32;
    let service = start_service("bounds", limits, dispatcher);

    assert_eq!(
        request(&service.socket_path, b"1234567\n", false).await,
        b"ok:1234567\n"
    );
    assert_eq!(
        request(&service.socket_path, b"123456789", true).await,
        b"reject:request-too-large\n"
    );
    assert_eq!(
        request(&service.socket_path, b"large\n", false).await,
        b"reject:response-too-large\n"
    );
    assert_eq!(
        request(&service.socket_path, b"newline\n", false).await,
        b"reject:invalid-response\n"
    );
    assert_eq!(
        request(&service.socket_path, b"fail\n", false).await,
        b"reject:dispatch-failed\n"
    );
    assert!(
        request(&service.socket_path, b"close\n", false)
            .await
            .is_empty()
    );
    assert_eq!(
        request(&service.socket_path, b"after\n", false).await,
        b"ok:after\n"
    );

    let report = service.stop().await;
    assert_eq!(report.dispatched_requests, 3);
    assert_eq!(report.rejected_requests, 4);
    assert_eq!(report.silently_closed_connections, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_client_cannot_terminate_the_service() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let service = start_service("disconnect", config(), dispatcher);
    let mut disconnected = UnixStream::connect(&service.socket_path).await.unwrap();
    disconnected.write_all(b"abandoned\n").await.unwrap();
    drop(disconnected);

    assert_eq!(
        request(&service.socket_path, b"survivor\n", false).await,
        b"ok:survivor\n"
    );

    let report = service.stop().await;
    assert!(report.dispatched_requests >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_admission_and_bounds_the_drain_wait() {
    let dispatcher = Arc::new(FakeDispatcher::default());
    let mut limits = config();
    limits.drain_timeout = Duration::from_millis(50);
    let service = start_service("drain", limits, dispatcher.clone());
    let held_path = service.socket_path.clone();
    let held = tokio::spawn(async move { request(&held_path, b"hold\n", false).await });
    dispatcher.wait_until_held().await;

    let report = service.stop().await;
    assert_eq!(report.aborted_connections, 1);
    dispatcher.release_hold();
    assert!(held.await.unwrap().is_empty());
}
