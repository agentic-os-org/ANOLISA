use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use asc_daemon_client::{ClientError, MAX_FRAME_BYTES, call};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse};
use serde_json::json;
// Only the Linux-gated connect-queue test builds a socket by hand.
#[cfg(target_os = "linux")]
use socket2::{Domain, SockAddr, Socket, Type};
use uuid::Uuid;

struct Endpoint {
    directory: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

impl Endpoint {
    fn new() -> Self {
        // `sun_path` caps a socket address at 104 bytes on macOS against 108 on
        // Linux, and `TMPDIR` there expands to a ~49-byte `/var/folders/...`
        // path that overruns the cap once a UUID-named directory and file name
        // are appended. Rooting at `/tmp` keeps the address near 60 bytes on
        // both systems instead of depending on the caller's `TMPDIR`.
        let base = std::path::Path::new("/tmp");
        let base = if base.is_dir() {
            base.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        let directory = base.join(format!("asc-client-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            directory,
            path,
            listener,
        }
    }

    fn accept(&self) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Accepted sockets inherit the listener's non-blocking flag on
                    // macOS but not on Linux. Left inherited, the flag overrides
                    // the timeouts set below and every read or write on this
                    // stream fails with `WouldBlock` instead of waiting.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    return stream;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "client did not connect");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
    }

    fn no_connection(&self) {
        assert_eq!(
            self.listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn exchange<T: Send>(
        &self,
        input: &DaemonRequest,
        timeout: Duration,
        server: impl FnOnce(&Self) -> T + Send,
    ) -> Result<DaemonResponse, ClientError> {
        self.timed_exchange(input, timeout, server).0
    }

    fn timed_exchange<T: Send>(
        &self,
        input: &DaemonRequest,
        timeout: Duration,
        server: impl FnOnce(&Self) -> T + Send,
    ) -> (Result<DaemonResponse, ClientError>, Duration) {
        let result = thread::scope(|scope| {
            let server = scope.spawn(|| server(self));
            let started = Instant::now();
            let result = call(&self.path, input, timeout);
            let elapsed = started.elapsed();
            // A returned peer stays open until after call finishes, allowing
            // tests to prove LF completion and deadlines without relying on EOF.
            let _peer = server.join().unwrap();
            (result, elapsed)
        });
        self.no_connection();
        result
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn request() -> DaemonRequest {
    DaemonRequest {
        method: "test.method".to_owned(),
        params: json!({}),
    }
}

fn read_request(stream: UnixStream) -> (BufReader<UnixStream>, Vec<u8>) {
    let mut stream = BufReader::new(stream);
    let mut frame = Vec::new();
    stream.read_until(b'\n', &mut frame).unwrap();
    (stream, frame)
}

const RESPONSE: &[u8] =
    b"{\"requestId\":\"request-1\",\"result\":{\"status\":\"PENDING_APPLY\"}}\n";

#[test]
fn fragmented_lf_response_returns_without_waiting_for_eof() {
    let endpoint = Endpoint::new();
    let response = endpoint
        .exchange(&request(), Duration::from_secs(1), |endpoint| {
            let (mut stream, frame) = read_request(endpoint.accept());
            assert_eq!(
                serde_json::from_slice::<DaemonRequest>(&frame).unwrap(),
                request()
            );
            stream.get_mut().write_all(&RESPONSE[..10]).unwrap();
            thread::sleep(Duration::from_millis(10));
            stream.get_mut().write_all(&RESPONSE[10..]).unwrap();
            stream
        })
        .unwrap();
    assert_eq!(response.request_id().as_str(), "request-1");
    assert!(matches!(response, DaemonResponse::Success(_)));
}

fn exchange(bytes: &[u8]) -> Result<DaemonResponse, ClientError> {
    Endpoint::new().exchange(&request(), Duration::from_secs(2), |endpoint| {
        let (mut stream, _) = read_request(endpoint.accept());
        let _ = stream.get_mut().write_all(bytes);
    })
}

#[test]
fn eof_delimited_response_and_first_frame_only_are_supported() {
    assert!(exchange(&RESPONSE[..RESPONSE.len() - 1]).is_ok());
    let mut bytes = RESPONSE.to_vec();
    bytes.extend_from_slice(b"unexpected trailing bytes");
    assert!(exchange(&bytes).is_ok());
}

#[test]
fn daemon_errors_preserve_code_message_and_request_id() {
    let response = exchange(b"{\"requestId\":\"r1\",\"error\":{\"code\":\"permission_denied\",\"message\":\"denied\"}}\n").unwrap();
    let DaemonResponse::Error(error) = response else {
        panic!("expected daemon error")
    };
    assert_eq!(error.request_id.as_str(), "r1");
    assert_eq!(error.error.code.as_str(), "permission_denied");
    assert_eq!(error.error.message(), "denied");
}

#[test]
fn empty_invalid_and_ambiguous_responses_do_not_trigger_replay() {
    assert!(matches!(exchange(&[]), Err(ClientError::EmptyResponse)));
    for payload in [
        b"not-json\n".as_slice(),
        b"{\"requestId\":\"r\",\"result\":{},\"error\":{\"code\":\"internal\",\"message\":\"bad\"}}\n",
        b"{\"result\":{}}\n",
    ] {
        assert!(matches!(exchange(payload), Err(ClientError::Decode(_))));
    }
}

#[test]
fn response_frame_limit_includes_lf() {
    let mut exact = RESPONSE[..RESPONSE.len() - 1].to_vec();
    exact.resize(MAX_FRAME_BYTES - 1, b' ');
    exact.push(b'\n');
    assert!(exchange(&exact).is_ok());
    exact.insert(exact.len() - 1, b' ');
    assert!(matches!(
        exchange(&exact),
        Err(ClientError::ResponseTooLarge)
    ));
}

#[test]
fn request_frame_limit_is_checked_before_connecting() {
    let endpoint = Endpoint::new();
    let mut input = request();
    input.params = json!({"payload": ""});
    let overhead = serde_json::to_vec(&input).unwrap().len() + 1;
    input.params["payload"] = json!("x".repeat(MAX_FRAME_BYTES - overhead + 1));
    assert!(matches!(
        call(&endpoint.path, &input, Duration::from_secs(1)),
        Err(ClientError::RequestTooLarge)
    ));
    endpoint.no_connection();
    input.params["payload"] = json!("x".repeat(MAX_FRAME_BYTES - overhead));
    assert!(
        endpoint
            .exchange(&input, Duration::from_secs(2), |endpoint| {
                let (mut stream, frame) = read_request(endpoint.accept());
                assert_eq!(frame.len(), MAX_FRAME_BYTES);
                stream.get_mut().write_all(RESPONSE).unwrap();
            })
            .is_ok()
    );
}

#[test]
fn deadline_does_not_reset_for_each_response_chunk() {
    let result = Endpoint::new().exchange(&request(), Duration::from_millis(100), |endpoint| {
        let (mut stream, _) = read_request(endpoint.accept());
        for _ in 0..10 {
            if stream.get_mut().write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        stream
    });
    assert!(matches!(
        result,
        Err(ClientError::Timeout {
            request_may_have_executed: true
        })
    ));
}

#[test]
fn deadline_is_not_extended_after_a_partial_response() {
    let (result, elapsed) =
        Endpoint::new().timed_exchange(&request(), Duration::from_millis(100), |endpoint| {
            let (mut stream, _) = read_request(endpoint.accept());
            thread::sleep(Duration::from_millis(70));
            stream.get_mut().write_all(b"{").unwrap();
            thread::sleep(Duration::from_millis(200));
            stream
        });
    assert!(matches!(
        result,
        Err(ClientError::Timeout {
            request_may_have_executed: true
        })
    ));
    assert!(
        elapsed < Duration::from_millis(150),
        "partial response extended the deadline: {elapsed:?}"
    );
}

#[test]
fn deadline_also_covers_a_blocked_write() {
    let mut input = request();
    input.params = json!({"payload": "x".repeat(MAX_FRAME_BYTES / 2)});
    let result = Endpoint::new().exchange(&input, Duration::from_millis(100), Endpoint::accept);
    assert!(matches!(
        result,
        Err(ClientError::Timeout {
            request_may_have_executed: true
        })
    ));
}

#[test]
fn deadline_covers_a_silent_response() {
    let result = Endpoint::new().exchange(&request(), Duration::from_millis(100), |endpoint| {
        let (stream, _) = read_request(endpoint.accept());
        stream
    });
    assert!(matches!(
        result,
        Err(ClientError::Timeout {
            request_may_have_executed: true
        })
    ));
}

#[test]
fn partial_writes_and_response_wait_share_one_deadline() {
    let mut input = request();
    input.params = json!({"payload": "x".repeat(MAX_FRAME_BYTES / 2)});
    // A slow reader makes multiple writes necessary. Draining the complete
    // request must not give the response wait a fresh timeout budget.
    let result = Endpoint::new().exchange(&input, Duration::from_millis(300), |endpoint| {
        let mut stream = endpoint.accept();
        thread::sleep(Duration::from_millis(180));
        let mut frame = Vec::new();
        let mut chunk = [0; 16384];
        loop {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0);
            frame.extend_from_slice(&chunk[..count]);
            if frame.ends_with(b"\n") {
                break;
            }
        }
        assert_eq!(
            serde_json::from_slice::<DaemonRequest>(&frame).unwrap(),
            input
        );
        thread::sleep(Duration::from_millis(180));
        let _ = stream.write_all(RESPONSE);
    });
    assert!(matches!(
        result,
        Err(ClientError::Timeout {
            request_may_have_executed: true
        })
    ));
}

#[test]
fn absent_daemon_and_invalid_deadline_are_local_failures() {
    let directory = std::env::temp_dir().join(format!("absent-{}", Uuid::new_v4()));
    assert!(matches!(
        call(&directory, &request(), Duration::from_secs(1)),
        Err(ClientError::Connect(_))
    ));
    assert!(matches!(
        call(&directory, &request(), Duration::ZERO),
        Err(ClientError::InvalidTimeout)
    ));
    assert!(matches!(
        call(&directory, &request(), Duration::MAX),
        Err(ClientError::InvalidTimeout)
    ));
    assert!(!directory.exists());
}

#[test]
fn expired_budget_before_connect_sends_nothing() {
    let endpoint = Endpoint::new();
    assert!(matches!(
        call(&endpoint.path, &request(), Duration::from_nanos(1)),
        Err(ClientError::Timeout {
            request_may_have_executed: false
        })
    ));
    endpoint.no_connection();
}

#[cfg(target_os = "linux")]
#[test]
fn full_connect_queue_is_bounded_and_never_marks_request_sent() {
    let endpoint = Endpoint::new();
    let queued_path = endpoint.directory.join("queued.sock");
    let address = SockAddr::unix(&queued_path).unwrap();
    let listener = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
    listener.bind(&address).unwrap();
    listener.listen(0).unwrap();
    listener.set_nonblocking(true).unwrap();
    let _queued = UnixStream::connect(&queued_path).unwrap();
    let started = Instant::now();
    let result = call(&queued_path, &request(), Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        result,
        Err(ClientError::Connect(_)
            | ClientError::Timeout {
                request_may_have_executed: false
            })
    ));
    let (_peer, _) = listener.accept().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
