//! Idle listener waits still observe an embedder's external cancellation flag.

use super::*;
use crate::Client;
use std::{os::unix::fs::PermissionsExt, sync::mpsc};

struct Stop {
    cancelled: Arc<AtomicBool>,
    socket: PathBuf,
}

impl Drop for Stop {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Also release an incorrectly unbounded wait when an assertion fails.
        let _ = UnixStream::connect(&self.socket);
    }
}

#[test]
fn idle_listener_observes_external_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = Server::bind(
        include_bytes!("../../../aw-config/examples/aw.minimal.yaml").to_vec(),
        directory.path().join("state"),
    )
    .unwrap();
    let socket = server.socket_path();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (finished, completion) = mpsc::channel();
    thread::scope(|scope| {
        let stop = Arc::clone(&cancelled);
        let worker = scope.spawn(move || {
            let result = server.run(&stop);
            let _ = finished.send(());
            result
        });
        let cleanup = Stop {
            cancelled: Arc::clone(&cancelled),
            socket: socket.clone(),
        };
        Client::connect(&socket, Instant::now() + Duration::from_secs(5)).unwrap();
        thread::sleep(Duration::from_millis(20));
        cancelled.store(true, Ordering::Release);
        let result = completion.recv_timeout(Duration::from_secs(2));
        drop(cleanup);
        worker.join().unwrap().unwrap();
        result.expect("idle service must observe cancellation without a new connection");
    });
    assert!(!socket.exists());
}

#[test]
fn preparation_returns_success_and_failure_with_an_idle_peer() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = Server::bind(
        include_bytes!("../../../aw-config/examples/aw.minimal.yaml").to_vec(),
        directory.path().join("state"),
    )
    .unwrap();
    let (stream, _peer) = UnixStream::pair().unwrap();
    let peer = endpoint::peer_credentials(&stream).unwrap();
    let process = Process::read(peer.pid as u32).unwrap();
    for target in ["qoder", "missing"] {
        let expires = Instant::now() + Duration::from_secs(2);
        let request = Request {
            api_version: VERSION.into(),
            identity: Some(server.runtime.identity.clone()),
            deadline_ns: deadline::encode(expires).unwrap(),
            operation: Operation::Bind {
                target: target.into(),
                capabilities: crate::Capabilities {
                    adapter: "qoder".into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    events: std::collections::BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                    failure_actions: Default::default(),
                },
                cwd: directory.path().to_str().unwrap().into(),
                environment: Default::default(),
            },
        };
        let result = prepare_connected(
            &stream,
            &server.runtime,
            request,
            expires,
            peer,
            Some(&process),
        );
        assert_eq!(result.is_ok(), target == "qoder", "{result:?}");
    }
}
