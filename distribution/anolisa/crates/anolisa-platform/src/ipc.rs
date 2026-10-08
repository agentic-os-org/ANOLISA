//! Low-level IPC primitives for the ANOLISA system-helper socket protocol.
//!
//! Wire format: length-prefixed JSON — each message is sent as a 4-byte
//! big-endian length header followed by the UTF-8 JSON payload.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

/// Default socket path for the system-helper daemon.
pub const SYSTEM_HELPER_SOCKET: &str = "/run/anolisa/system-helper.sock";

/// Maximum allowed message size (8 MiB) — prevents OOM from malformed frames.
const MAX_MESSAGE_SIZE: u32 = 8 * 1024 * 1024;

// ─── Peer credential ────────────────────────────────────────────────────────

/// Peer credential obtained via `SO_PEERCRED` + `SO_PEERGROUPS` (Linux).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCredential {
    pub uid: u32,
    pub gid: u32,
    pub pid: i32,
    /// Supplementary group IDs of the peer as snapshotted by the kernel at
    /// `connect()` time. The snapshot — not a later `/proc/<pid>` lookup —
    /// is the authority: the numeric PID can be recycled while the
    /// connection stays open, and a lookup could then read an unrelated
    /// process's groups.
    pub groups: Vec<u32>,
}

/// Retrieve the peer credential from a connected Unix stream.
///
/// On Linux this calls `getsockopt(SO_PEERCRED)` via the `nix` crate; the
/// supplementary groups come from `SO_PEERGROUPS`, which the kernel derives
/// from the same connect-time credential snapshot.
#[cfg(target_os = "linux")]
pub fn get_peer_credential(stream: &UnixStream) -> io::Result<PeerCredential> {
    use std::os::unix::io::AsFd;

    let fd = stream.as_fd();
    let cred = nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials)
        .map_err(io::Error::other)?;

    // An unreadable group list (pre-4.13 kernel, allocation failure) fails
    // closed to empty: a supplementary-group member is then refused instead
    // of authorized on evidence that was never captured.
    let groups = peer_groups(&fd).unwrap_or_default();

    Ok(PeerCredential {
        uid: cred.uid(),
        gid: cred.gid(),
        pid: cred.pid(),
        groups,
    })
}

/// Reads the peer's supplementary groups via `getsockopt(SO_PEERGROUPS)`.
///
/// `nix` 0.29 has no wrapper for this option, so this is the raw `libc`
/// call — through nix's `libc` re-export, the same route the codebase
/// already uses for `O_NOFOLLOW`. The buffer starts small and doubles while
/// the kernel answers `ERANGE`.
///
/// # Errors
///
/// Returns the OS error when the kernel cannot serve the snapshot; the
/// caller is expected to treat that as "no groups known".
#[cfg(target_os = "linux")]
fn peer_groups(fd: &std::os::unix::io::BorrowedFd<'_>) -> io::Result<Vec<u32>> {
    use std::os::unix::io::AsRawFd;

    // 64 gids covers every ordinary login session; NGROUPS_MAX (65536)
    // bounds the growth for hostile credentials.
    let mut cap = 64usize;
    loop {
        let mut buf = vec![0u32; cap];
        let mut len = (cap * std::mem::size_of::<u32>()) as nix::libc::socklen_t;
        let rc = unsafe {
            nix::libc::getsockopt(
                fd.as_raw_fd(),
                nix::libc::SOL_SOCKET,
                nix::libc::SO_PEERGROUPS,
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc == 0 {
            buf.truncate(len as usize / std::mem::size_of::<u32>());
            return Ok(buf);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(nix::libc::ERANGE) && cap < 65_536 {
            cap *= 2;
            continue;
        }
        return Err(err);
    }
}

/// Stub for non-Linux platforms (development on macOS, etc.).
#[cfg(not(target_os = "linux"))]
pub fn get_peer_credential(_stream: &UnixStream) -> io::Result<PeerCredential> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SO_PEERCRED is only available on Linux",
    ))
}

// ─── Wire protocol ──────────────────────────────────────────────────────────

/// Send a length-prefixed JSON message over a `UnixStream`.
///
/// Format: `[4 bytes big-endian length][JSON payload]`
pub fn send_message<T: serde::Serialize>(stream: &mut UnixStream, msg: &T) -> io::Result<()> {
    let payload =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

/// Receive a length-prefixed JSON message from a `UnixStream`.
///
/// Returns `UnexpectedEof` when the peer has closed the connection.
pub fn recv_message<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes (max {MAX_MESSAGE_SIZE})"),
        ));
    }

    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    #[test]
    fn roundtrip_simple_string() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let msg = "hello world".to_string();
        send_message(&mut a, &msg).unwrap();
        let received: String = recv_message(&mut b).unwrap();
        assert_eq!(msg, received);
    }

    #[test]
    fn roundtrip_structured() {
        #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
        struct TestMsg {
            action: String,
            code: i32,
        }

        let (mut a, mut b) = UnixStream::pair().unwrap();
        let msg = TestMsg {
            action: "install".into(),
            code: 42,
        };
        send_message(&mut a, &msg).unwrap();
        let received: TestMsg = recv_message(&mut b).unwrap();
        assert_eq!(msg, received);
    }

    #[test]
    fn reject_oversized_message() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        // Write a fake header claiming a huge payload
        let fake_len: u32 = MAX_MESSAGE_SIZE + 1;
        a.write_all(&fake_len.to_be_bytes()).unwrap();
        let err = recv_message::<String>(&mut b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn eof_on_closed_connection() {
        let (a, mut b) = UnixStream::pair().unwrap();
        drop(a); // close sender
        let err = recv_message::<String>(&mut b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn peer_credential_reports_the_connector_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();

        let cred = get_peer_credential(&server).unwrap();
        assert_eq!(
            cred.pid,
            std::process::id() as i32,
            "the connector is this process"
        );
        let own: Vec<u32> = nix::unistd::getgroups()
            .unwrap()
            .into_iter()
            .map(|gid| gid.as_raw())
            .collect();
        assert_eq!(
            cred.groups, own,
            "SO_PEERGROUPS must mirror getgroups() of the connector"
        );
        drop(client);
    }

    /// Gids used by [`peer_groups_snapshot_survives_group_changes`]: a base
    /// group the connector holds from the start and the delegated group it
    /// only joins after connecting.
    #[cfg(target_os = "linux")]
    const CHILD_BASE_GID: u32 = 190_001;
    #[cfg(target_os = "linux")]
    const CHILD_DELEGATED_GID: u32 = 190_002;

    /// Child half of [`peer_groups_snapshot_survives_group_changes`]; runs
    /// only when the parent re-executed this test binary with
    /// [`PEER_CHILD_ENV`] set.
    ///
    /// `outsider`: join only the base group, connect, announce readiness,
    /// then join the delegated group while the connection stays open and
    /// announce that too. `member`: join both before connecting and announce
    /// readiness. Never returns on the success path — the parent kills the
    /// process once it has read the snapshot.
    #[cfg(target_os = "linux")]
    fn peer_child_dance(mode: &str, socket_path: &str) -> ! {
        use std::io::Write;

        let base = nix::unistd::Gid::from_raw(CHILD_BASE_GID);
        let delegated = nix::unistd::Gid::from_raw(CHILD_DELEGATED_GID);
        if mode == "member" {
            nix::unistd::setgroups(&[base, delegated]).unwrap();
        } else {
            nix::unistd::setgroups(&[base]).unwrap();
        }
        let _client = UnixStream::connect(socket_path).unwrap();
        // Marker-prefixed so the parent can skip the libtest progress lines
        // that share this stdout under --nocapture.
        println!("{MARKER_PREFIX}READY");
        std::io::stdout().flush().unwrap();
        if mode == "outsider" {
            nix::unistd::setgroups(&[base, delegated]).unwrap();
            println!("{MARKER_PREFIX}JOINED");
            std::io::stdout().flush().unwrap();
        }
        // Hold the endpoint open until the parent is done with the snapshot.
        loop {
            std::thread::park();
        }
    }

    /// Set when the test process re-executes itself as the connecting child.
    #[cfg(target_os = "linux")]
    const PEER_CHILD_ENV: &str = "ANOLISA_IPC_TEST_PEER_CHILD";

    /// Prefix of the child's stdout markers ([`peer_child_dance`]).
    #[cfg(target_os = "linux")]
    const MARKER_PREFIX: &str = "ANOLISA_PEER_";

    /// Reads child stdout lines until the next marker, skipping the libtest
    /// progress lines that share the stream under `--nocapture`.
    #[cfg(target_os = "linux")]
    fn next_child_marker(
        lines: &mut std::io::Lines<BufReader<std::process::ChildStdout>>,
    ) -> String {
        for line in lines.by_ref() {
            let line = line.unwrap();
            if line.starts_with(MARKER_PREFIX) {
                return line;
            }
        }
        panic!("child exited before signalling the expected marker");
    }

    /// The connect-time group snapshot must survive everything the peer — or
    /// a same-uid successor on a recycled PID — does after connecting.
    ///
    /// An `outsider` child connects holding only the base group and joins
    /// the delegated group while the connection stays open; a later
    /// `/proc/<pid>/status` read (the approach this replaces) would then see
    /// the delegated group and authorize the stale connection, which is the
    /// fd-transfer/PID-reuse shape from the review. A `member` child joins
    /// the delegated group before connecting, pinning the positive control:
    /// membership granted before connect must arrive in the snapshot.
    ///
    /// Needs root for `setgroups`; skipped otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn peer_groups_snapshot_survives_group_changes() {
        if let Ok(spec) = std::env::var(PEER_CHILD_ENV) {
            let mut parts = spec.splitn(2, ' ');
            let mode = parts.next().unwrap_or_default();
            let socket_path = parts.next().unwrap_or_default();
            peer_child_dance(mode, socket_path);
        }
        if !nix::unistd::Uid::effective().is_root() {
            eprintln!("skipping: setgroups requires root");
            return;
        }

        // The child re-executes this same test; --exact needs the full
        // module-qualified test path, not the bare function name, or the
        // harness would match zero tests and exit before connecting.
        let child_test = "ipc::tests::peer_groups_snapshot_survives_group_changes";
        let spawn_child = |mode: &str, path: &std::path::Path| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", child_test, "--nocapture"])
                .env(PEER_CHILD_ENV, format!("{mode} {}", path.display()))
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };

        // Outsider: joins the delegated group only after connecting.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut child = spawn_child("outsider", &path);
        let (server, _) = listener.accept().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert_eq!(
            next_child_marker(&mut lines),
            format!("{MARKER_PREFIX}READY"),
            "child must announce the live connection"
        );
        assert_eq!(
            next_child_marker(&mut lines),
            format!("{MARKER_PREFIX}JOINED"),
            "child must join the delegated group after connecting"
        );
        let cred = get_peer_credential(&server).unwrap();
        assert!(
            cred.groups.contains(&CHILD_BASE_GID),
            "connect-time groups must survive the peer's later group changes: {:?}",
            cred.groups
        );
        assert!(
            !cred.groups.contains(&CHILD_DELEGATED_GID),
            "a group joined after connect must not leak into the snapshot: {:?}",
            cred.groups
        );
        // The endpoint outlives the connector: re-reading after the child is
        // gone must return the same connect-time snapshot, never whatever
        // process the kernel handed the recycled PID.
        child.kill().unwrap();
        child.wait().unwrap();
        let after = get_peer_credential(&server).unwrap();
        assert_eq!(after.groups, cred.groups);
        assert_eq!(after.pid, cred.pid);

        // Member: joins the delegated group before connecting.
        let member_path = dir.path().join("member.sock");
        let member_listener = std::os::unix::net::UnixListener::bind(&member_path).unwrap();
        let mut member = spawn_child("member", &member_path);
        let (member_server, _) = member_listener.accept().unwrap();
        let mut member_lines = BufReader::new(member.stdout.take().unwrap()).lines();
        assert_eq!(
            next_child_marker(&mut member_lines),
            format!("{MARKER_PREFIX}READY"),
            "member child must announce the live connection"
        );
        let member_cred = get_peer_credential(&member_server).unwrap();
        assert!(
            member_cred.groups.contains(&CHILD_DELEGATED_GID),
            "membership held before connect must arrive in the snapshot: {:?}",
            member_cred.groups
        );
        member.kill().unwrap();
        member.wait().unwrap();
    }
}
