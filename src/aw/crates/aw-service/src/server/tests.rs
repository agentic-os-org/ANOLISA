//! Service listener cancellation, preparation, and connection accounting tests.

use super::*;
use crate::{Binding, Client, EventHandle};
use serde_json::json;
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
fn sequential_rpcs_release_the_only_connection_slot_before_reply() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap();
    let mut document = config.as_value().clone();
    document["spec"]["daemon"]["limits"] = serde_json::json!({"connections": 1});
    let server = Server::bind(
        serde_json::to_vec(&document).unwrap(),
        directory.path().join("state"),
    )
    .unwrap();
    let socket = server.socket_path();
    let cancelled = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let stop = Arc::clone(&cancelled);
        let worker = scope.spawn(move || server.run(&stop));
        let cleanup = Stop {
            cancelled,
            socket: socket.clone(),
        };
        let expires = || Instant::now() + Duration::from_secs(5);
        let client = Client::connect(&socket, expires()).unwrap();
        for index in 0..200 {
            let response = client.call(Operation::RecentExecutions, expires());
            assert!(response.is_ok(), "sequential request {index}: {response:?}");
        }
        let status = client.call(Operation::Status, expires()).unwrap();
        assert!(status["resources"]["rejection_reasons"]
            .get("connection_limit")
            .is_none_or(|count| count == 0));
        drop(cleanup);
        worker.join().unwrap().unwrap();
    });
}

#[test]
fn backpressured_ordinary_reply_preserves_control_capacity() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap();
    let mut document = config.as_value().clone();
    let spec = &mut document["spec"];
    spec["daemon"]["limits"] = json!({"connections": 1});
    spec["execution"]["default_event_budget_ms"] = json!(15000);
    spec["providers"]["raw"] = json!({
        "protocol": "native-hook/v1alpha1", "config": {},
        "timeout_ms": 5000, "max_output_bytes": 524288,
        "transport": {"type": "stdio", "location": "agent",
            "argv": ["/usr/bin/head", "-c", "400000", "/dev/zero"]}
    });
    spec["events"]["tool.before"]["steps"] =
        json!([{"id": "raw", "provider": "raw", "native": {}, "on_error": "block"}]);
    let server = Server::bind(
        serde_json::to_vec(&document).unwrap(),
        directory.path().join("state"),
    )
    .unwrap();
    let socket = server.socket_path();
    let cancelled = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let stop = Arc::clone(&cancelled);
        let worker = scope.spawn(move || server.run(&stop));
        let cleanup = Stop {
            cancelled,
            socket: socket.clone(),
        };
        let expires = || Instant::now() + Duration::from_secs(10);
        let client = Client::connect(&socket, expires()).unwrap();
        let binding: Binding = serde_json::from_value(
            client
                .call(
                    Operation::Bind {
                        target: "qoder".into(),
                        capabilities: crate::Capabilities {
                            post_permission: false,
                            adapter: "qoder".into(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            events: std::collections::BTreeMap::from([
                                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                                ("tool.after".into(), vec!["observe".into()]),
                            ]),
                            failure_actions: Default::default(),
                        },
                        cwd: directory.path().to_str().unwrap().into(),
                        environment: Default::default(),
                    },
                    expires(),
                )
                .unwrap(),
        )
        .unwrap();
        let burst = CONTROL_RESERVE + 2;
        let events: Vec<EventHandle> = (0..=burst)
            .map(|index| {
                serde_json::from_value(
                    client
                        .call(
                            Operation::OpenHookEvent {
                                instance_id: binding.instance_id.clone(),
                                event: json!({
                                    "name": "tool.before",
                                    "agent": {"adapter": "qoder", "binding_id": "qoder",
                                        "instance_id": binding.instance_id},
                                    "session_id": "session",
                                    "tool": {"name": "test", "native_name": "test",
                                        "call_id": format!("call-{index}"), "input": {}, "result": null},
                                    "native": {},
                                }),
                                native_input: Vec::new(),
                            },
                            expires(),
                        )
                        .unwrap(),
                )
                .unwrap()
            })
            .collect();
        let send_unread = |event: &EventHandle| {
            let mut stream = UnixStream::connect(&socket).unwrap();
            let deadline = expires();
            ipc::write(
                &mut stream,
                &Request {
                    api_version: VERSION.into(),
                    identity: Some(client.identity().clone()),
                    deadline_ns: deadline::encode(deadline).unwrap(),
                    operation: Operation::InvokeStep {
                        instance_id: event.instance_id.clone(),
                        event_id: event.event_id.clone(),
                        step_id: "raw".into(),
                        native_environment: None,
                    },
                },
                deadline,
            )
            .unwrap();
            let mut fd = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // Wait for the reply to begin while leaving all its bytes queued.
            assert_eq!(unsafe { libc::poll(&mut fd, 1, 5000) }, 1);
            assert_ne!(fd.revents & libc::POLLIN, 0);
            stream
        };
        let assert_large_frame = |stream: &mut UnixStream| {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut header = [0u8; 4];
            let size = unsafe {
                libc::recv(
                    stream.as_raw_fd(),
                    header.as_mut_ptr().cast(),
                    header.len(),
                    libc::MSG_PEEK | libc::MSG_WAITALL,
                )
            };
            assert_eq!(size, header.len() as isize);
            let length = u32::from_be_bytes(header);
            if length < 800_000 {
                let response: Response = ipc::read(stream, expires()).unwrap();
                panic!("expected a large reply, got {length} bytes: {response:?}");
            }
        };
        let mut slow = send_unread(&events[0]);
        assert_large_frame(&mut slow);
        let rejected: Vec<_> = events[1..].iter().map(send_unread).collect();
        let status = client.call(Operation::Status, expires()).unwrap();
        assert_eq!(
            status["resources"]["rejection_reasons"]["connection_limit"],
            burst
        );
        let response: Response = ipc::read(&mut slow, expires()).unwrap();
        assert!(response.error.is_none(), "{response:?}");
        let result = response.result.unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(
            result["native"]["stdout"].as_array().unwrap().len(),
            400_000
        );
        for mut stream in rejected {
            let response: Response = ipc::read(&mut stream, expires()).unwrap();
            assert_eq!(response.error.as_deref(), Some("connection_limit"));
        }
        let mut slow = send_unread(&events[1]);
        assert_large_frame(&mut slow);
        client.call(Operation::Stop, expires()).unwrap();
        drop(slow);
        worker.join().unwrap().unwrap();
        drop(cleanup);
    });
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
                    post_permission: false,
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
