//! Regression coverage for daemon availability under peer-driven fd
//! exhaustion.
//!
//! Every accepted connection holds a descriptor until its handler thread
//! finishes, so a peer that opens enough sockets drives `accept(2)` into
//! `EMFILE`. Aborting the daemon on that transient error would tear down
//! every active binding, so this test runs the real binary under a low
//! `RLIMIT_NOFILE`, exhausts its descriptors, and asserts the daemon keeps
//! serving once the descriptors are released.

#![cfg(feature = "mock-backend")]

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use agentsight_enforcement_protocol::{
    Command as ProtocolCommand, Request, Response, read_frame, write_frame,
};
use uuid::Uuid;

/// Descriptor budget for the daemon under test. Startup needs a handful of
/// descriptors (stdio, the listening socket), so this leaves room for a
/// bounded number of accepted clients before `accept(2)` fails with `EMFILE`.
const DAEMON_FD_LIMIT: u64 = 24;

fn spawn_daemon(socket_path: &Path) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentsight-enforcer"));
    command
        .env("AGENTSIGHT_ENFORCER_SOCKET", socket_path)
        .stderr(Stdio::piped());
    // SAFETY: the closure runs between `fork` and `exec` and only calls
    // `setrlimit`, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: DAEMON_FD_LIMIT,
                rlim_max: DAEMON_FD_LIMIT,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .expect("enforcer daemon should spawn under a low fd limit")
}

fn wait_for_socket(socket_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "enforcer did not bind {socket_path:?} within 10s"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn health_round_trip(socket_path: &Path) {
    let stream = UnixStream::connect(socket_path).expect("daemon should accept fresh clients");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout should apply");
    let mut writer = stream.try_clone().expect("socket should clone");
    write_frame(&mut writer, &Request::new(ProtocolCommand::Health))
        .expect("health request should be written");
    let response = read_frame::<_, Response>(&mut BufReader::new(&stream))
        .expect("daemon should answer after fd recovery")
        .expect("response frame should exist");
    assert!(
        response.result.is_ok(),
        "health should succeed after recovery: {response:?}"
    );
}

#[test]
fn daemon_survives_fd_exhaustion_and_keeps_serving() {
    let socket_path = PathBuf::from("/tmp").join(format!(
        "enforcer-accept-resilience-{}.sock",
        Uuid::new_v4()
    ));
    let mut child = spawn_daemon(&socket_path);
    wait_for_socket(&socket_path);

    // Hold more connections than the daemon's descriptor budget. Each
    // accepted connection parks in its handler thread until this side
    // closes, so `accept(2)` eventually fails with `EMFILE`.
    let exhaustors: Vec<_> = (0..64)
        .map(|index| {
            UnixStream::connect(&socket_path).unwrap_or_else(|error| {
                panic!(
                    "client {index} could not connect; the daemon likely exited \
                     under fd exhaustion: {error}"
                )
            })
        })
        .collect();

    // Before the fix the daemon exits as soon as accept fails; give it time
    // to reach the exhausted state and assert it is still alive.
    thread::sleep(Duration::from_millis(500));
    assert!(
        child
            .try_wait()
            .expect("daemon state should be queryable")
            .is_none(),
        "daemon must survive fd exhaustion instead of exiting"
    );

    // Releasing the clients frees the daemon's descriptors; a fresh request
    // must be served, proving the accept loop recovered.
    drop(exhaustors);
    health_round_trip(&socket_path);

    // Graceful stop still works after the recovery.
    // SAFETY: `kill` only delivers a signal to the spawned child pid; no
    // memory is dereferenced.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let output = child
        .wait_with_output()
        .expect("daemon should exit after SIGINT");
    assert!(
        output.status.success(),
        "daemon should stop cleanly after surviving fd exhaustion"
    );
    let _ = std::fs::remove_file(&socket_path);
}
