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

/// Peer credential obtained via `SO_PEERCRED` (Linux) or equivalent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCredential {
    pub uid: u32,
    pub gid: u32,
    pub pid: i32,
}

/// Retrieve the peer credential from a connected Unix stream.
///
/// On Linux this calls `getsockopt(SO_PEERCRED)` via the `nix` crate.
#[cfg(target_os = "linux")]
pub fn get_peer_credential(stream: &UnixStream) -> io::Result<PeerCredential> {
    use std::os::unix::io::AsFd;

    let fd = stream.as_fd();
    let cred = nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials)
        .map_err(io::Error::other)?;

    Ok(PeerCredential {
        uid: cred.uid(),
        gid: cred.gid(),
        pid: cred.pid(),
    })
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
}
