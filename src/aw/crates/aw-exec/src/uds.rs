//! Bounded non-blocking Unix domain socket request/response exchange.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Transport failure during a bounded Unix domain socket exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UdsError {
    #[error("uds_unavailable")]
    Unavailable,
    #[error("uds_peer_rejected")]
    PeerRejected,
    #[error("uds_timeout")]
    Timeout,
    #[error("uds_response_too_large")]
    ResponseTooLarge,
    #[error("uds_invalid_frame")]
    InvalidFrame,
    #[error("uds_invalid_timeout")]
    InvalidTimeout,
}

/// Executes a single-shot, bounded newline-framed request/response exchange
/// over a local Unix domain stream socket with zero retries on mid-stream failure.
pub fn exchange(
    socket: &Path,
    request_line: &[u8],
    timeout: Duration,
    expected_uid: Option<u32>,
    max_response_bytes: usize,
) -> Result<Vec<u8>, UdsError> {
    if timeout.is_zero() {
        return Err(UdsError::Timeout);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(UdsError::InvalidTimeout)?;
    let mut stream = connect_bounded(socket, deadline)?;
    if let Some(uid) = expected_uid {
        verify_peer_uid(&stream, uid)?;
    }
    write_all_bounded(&stream, request_line, deadline)?;
    if !request_line.ends_with(b"\n") {
        write_all_bounded(&stream, b"\n", deadline)?;
    }
    read_line_bounded(&mut stream, deadline, max_response_bytes)
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, UdsError> {
    let now = Instant::now();
    if now >= deadline {
        Err(UdsError::Timeout)
    } else {
        Ok(deadline - now)
    }
}

fn connect_bounded(socket: &Path, deadline: Instant) -> Result<UnixStream, UdsError> {
    let path_bytes = socket.as_os_str().as_bytes();
    if path_bytes.is_empty() || path_bytes.contains(&0) {
        return Err(UdsError::Unavailable);
    }

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if path_bytes.len() >= addr.sun_path.len() {
        return Err(UdsError::Unavailable);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr().cast::<libc::c_char>(),
            addr.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
    }
    let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
        as libc::socklen_t;

    loop {
        let _ = remaining_timeout(deadline)?;
        let fd = open_nonblocking_unix_socket()?;
        let rc = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
                addr_len,
            )
        };
        if rc == 0 {
            return Ok(unsafe { UnixStream::from_raw_fd(fd.into_raw_fd()) });
        }

        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINPROGRESS) => {
                poll_fd(fd.as_raw_fd(), libc::POLLOUT, remaining_timeout(deadline)?)?;
                check_connected(fd.as_raw_fd())?;
                return Ok(unsafe { UnixStream::from_raw_fd(fd.into_raw_fd()) });
            }
            // Linux AF_UNIX returns EAGAIN when the listening backlog is momentarily full.
            Some(libc::EAGAIN) | Some(libc::EINTR) => {
                drop(fd);
                let rem = remaining_timeout(deadline)?;
                thread::sleep(Duration::from_millis(1).min(rem));
            }
            _ => return Err(UdsError::Unavailable),
        }
    }
}

fn open_nonblocking_unix_socket() -> Result<OwnedFd, UdsError> {
    #[cfg(target_os = "linux")]
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };

    if raw_fd < 0 {
        return Err(UdsError::Unavailable);
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

    #[cfg(not(target_os = "linux"))]
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(UdsError::Unavailable);
        }
        let fd_flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        if fd_flags < 0
            || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) < 0
        {
            return Err(UdsError::Unavailable);
        }
    }

    #[cfg(target_os = "macos")]
    unsafe {
        let one: libc::c_int = 1;
        let _ = libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&one as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    Ok(fd)
}

fn check_connected(fd: libc::c_int) -> Result<(), UdsError> {
    let mut so_error: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut so_error as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc != 0 || so_error != 0 {
        return Err(UdsError::Unavailable);
    }
    let mut peer: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let peer_rc = unsafe {
        libc::getpeername(
            fd,
            (&mut peer as *mut libc::sockaddr_un).cast::<libc::sockaddr>(),
            &mut peer_len,
        )
    };
    if peer_rc != 0 {
        return Err(UdsError::Unavailable);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_peer_uid(stream: &UnixStream, expected_uid: u32) -> Result<(), UdsError> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 || (len as usize) != std::mem::size_of::<libc::ucred>() {
        return Err(UdsError::PeerRejected);
    }
    if cred.uid != expected_uid {
        return Err(UdsError::PeerRejected);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_peer_uid(stream: &UnixStream, expected_uid: u32) -> Result<(), UdsError> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 || uid != expected_uid {
        return Err(UdsError::PeerRejected);
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn verify_peer_uid(_stream: &UnixStream, _expected_uid: u32) -> Result<(), UdsError> {
    Err(UdsError::PeerRejected)
}

fn poll_fd(fd: libc::c_int, events: libc::c_short, timeout: Duration) -> Result<(), UdsError> {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Err(UdsError::Timeout);
        }
        let rem = timeout - elapsed;
        let timeout_ms = rem.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc == 0 {
            return Err(UdsError::Timeout);
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(UdsError::Unavailable);
        }
        if (pfd.revents & libc::POLLNVAL) != 0 {
            return Err(UdsError::Unavailable);
        }
        if (pfd.revents & (events | libc::POLLERR | libc::POLLHUP)) != 0 {
            return Ok(());
        }
    }
}

fn write_all_bounded(
    stream: &UnixStream,
    mut buf: &[u8],
    deadline: Instant,
) -> Result<(), UdsError> {
    #[cfg(target_os = "linux")]
    let flags: libc::c_int = libc::MSG_NOSIGNAL;
    #[cfg(not(target_os = "linux"))]
    let flags: libc::c_int = 0;

    while !buf.is_empty() {
        let rem = remaining_timeout(deadline)?;
        let rc = unsafe { libc::send(stream.as_raw_fd(), buf.as_ptr().cast(), buf.len(), flags) };
        if rc > 0 {
            buf = &buf[rc as usize..];
        } else if rc == 0 {
            return Err(UdsError::Unavailable);
        } else {
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::WouldBlock => {
                    poll_fd(stream.as_raw_fd(), libc::POLLOUT, rem)?;
                }
                io::ErrorKind::Interrupted => continue,
                _ => return Err(UdsError::Unavailable),
            }
        }
    }
    Ok(())
}

fn read_line_bounded(
    stream: &mut UnixStream,
    deadline: Instant,
    max_bytes: usize,
) -> Result<Vec<u8>, UdsError> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 4096];
    loop {
        let rem = remaining_timeout(deadline)?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                if buf.is_empty() {
                    return Err(UdsError::Unavailable);
                }
                return Err(UdsError::InvalidFrame);
            }
            Ok(n) => {
                if let Some(pos) = chunk[..n].iter().position(|&b| b == b'\n') {
                    if buf.len() + pos > max_bytes {
                        return Err(UdsError::ResponseTooLarge);
                    }
                    buf.extend_from_slice(&chunk[..pos]);
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    if buf.is_empty() {
                        return Err(UdsError::InvalidFrame);
                    }
                    return Ok(buf);
                }
                if buf.len() + n > max_bytes {
                    return Err(UdsError::ResponseTooLarge);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                poll_fd(stream.as_raw_fd(), libc::POLLIN, rem)?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(UdsError::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Barrier};

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("aw-uds-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("create tempdir");
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn current_uid() -> u32 {
        unsafe { libc::geteuid() as u32 }
    }

    #[test]
    fn exchange_round_trips_newline_frame_and_verifies_peer_uid() {
        let dir = TempDir::new("test");
        let sock = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut line)
                .expect("read");
            assert_eq!(line, "{\"ping\":1}\n");
            stream.write_all(b"{\"pong\":1}\n").expect("write");
        });

        let out = exchange(
            &sock,
            b"{\"ping\":1}",
            Duration::from_secs(2),
            Some(current_uid()),
            4096,
        )
        .expect("exchange");
        assert_eq!(out, b"{\"pong\":1}");
        handle.join().expect("join");
    }

    #[test]
    fn exchange_rejects_mismatched_peer_uid_before_sending_payload() {
        let dir = TempDir::new("test");
        let sock = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 16];
            let n = stream.read(&mut buf).unwrap_or(0);
            assert_eq!(n, 0, "client must not write bytes when peer UID mismatches");
        });

        let wrong_uid = current_uid().wrapping_add(1);
        let err = exchange(
            &sock,
            b"{\"secret\":true}",
            Duration::from_secs(2),
            Some(wrong_uid),
            4096,
        )
        .expect_err("peer uid mismatch must fail");
        assert_eq!(err, UdsError::PeerRejected);
        handle.join().expect("join");
    }

    #[test]
    fn exchange_enforces_deadline_and_rejects_unterminated_eof() {
        let dir = TempDir::new("test");
        let sock = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            let _ = BufReader::new(&mut stream).read_line(&mut line);
            thread::sleep(Duration::from_millis(250));
        });

        let err = exchange(
            &sock,
            b"{\"slow\":true}",
            Duration::from_millis(60),
            Some(current_uid()),
            4096,
        )
        .expect_err("must time out");
        assert_eq!(err, UdsError::Timeout);
        handle.join().expect("join");

        let sock_eof = dir.path().join("eof.sock");
        let listener_eof = UnixListener::bind(&sock_eof).expect("bind eof");
        let handle_eof = thread::spawn(move || {
            let (mut stream, _) = listener_eof.accept().expect("accept");
            let mut line = String::new();
            let _ = BufReader::new(&mut stream).read_line(&mut line);
            stream.write_all(b"{\"partial\":").expect("partial write");
        });
        let err_eof = exchange(
            &sock_eof,
            b"{\"req\":1}",
            Duration::from_secs(2),
            Some(current_uid()),
            4096,
        )
        .expect_err("partial frame must fail");
        assert_eq!(err_eof, UdsError::InvalidFrame);
        handle_eof.join().expect("join eof");
    }

    #[test]
    fn exchange_handles_saturated_backlog_under_concurrency() {
        let dir = TempDir::new("test");
        let sock = dir.path().join("backlog1.sock");
        let path_bytes = sock.as_os_str().as_bytes();
        let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(raw_fd >= 0);
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        unsafe {
            std::ptr::copy_nonoverlapping(
                path_bytes.as_ptr().cast::<libc::c_char>(),
                addr.sun_path.as_mut_ptr(),
                path_bytes.len(),
            );
            let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path)
                + path_bytes.len()
                + 1) as libc::socklen_t;
            assert_eq!(
                libc::bind(
                    raw_fd,
                    (&addr as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
                    addr_len
                ),
                0
            );
            // Force a tiny listen backlog of 1 so concurrent nonblocking connects hit EAGAIN.
            assert_eq!(libc::listen(raw_fd, 1), 0);
        }
        let listener = unsafe { UnixListener::from_raw_fd(raw_fd) };
        let clients = 16usize;
        let server = thread::spawn(move || {
            for _ in 0..clients {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut line = String::new();
                BufReader::new(&mut stream)
                    .read_line(&mut line)
                    .expect("read");
                stream.write_all(b"{\"ok\":true}\n").expect("write");
            }
        });

        let barrier = Arc::new(Barrier::new(clients));
        let mut handles = Vec::with_capacity(clients);
        let uid = current_uid();
        for _ in 0..clients {
            let sock = sock.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                exchange(
                    &sock,
                    b"{\"hello\":1}",
                    Duration::from_secs(3),
                    Some(uid),
                    4096,
                )
            }));
        }
        for h in handles {
            let res = h
                .join()
                .expect("client thread")
                .expect("exchange succeeded");
            assert_eq!(res, b"{\"ok\":true}");
        }
        server.join().expect("server thread");
    }

    #[test]
    fn exchange_rejects_unrepresentable_timeout_without_panic() {
        let sock = Path::new("/tmp/aw-exec-nonexistent-timeout.sock");
        let err = exchange(sock, b"{}", Duration::MAX, None, 4096).unwrap_err();
        assert_eq!(err, UdsError::InvalidTimeout);
    }
}
