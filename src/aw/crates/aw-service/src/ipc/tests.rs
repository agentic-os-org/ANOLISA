//! IPC framing, parsing, deadlines and connection diagnostics tests.

use super::*;
use serde_json::json;

fn receive(bytes: &[u8]) -> io::Result<Value> {
    let (mut sender, mut receiver) = UnixStream::pair()?;
    sender.write_all(&(bytes.len() as u32).to_be_bytes())?;
    sender.write_all(bytes)?;
    read(&mut receiver, Instant::now() + Duration::from_secs(1))
}

#[test]
fn unavailable_connections_remain_distinct_from_invalid_paths_and_permissions() {
    use std::os::unix::{
        fs::{symlink, PermissionsExt},
        net::UnixListener,
    };

    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let socket = directory.path().join("aw.sock");
    let connect_error =
        |path: &Path| connect(path, Instant::now() + Duration::from_secs(1)).unwrap_err();
    let missing = connect_error(&socket);
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert_eq!(missing.to_string(), "service_unavailable");
    drop(UnixListener::bind(&socket).unwrap());
    let stale = connect_error(&socket);
    assert_eq!(stale.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(stale.to_string(), "service_unavailable");
    assert!(socket.exists());
    std::fs::remove_file(&socket).unwrap();

    std::fs::write(&socket, b"ordinary file").unwrap();
    assert_ne!(connect_error(&socket).to_string(), "service_unavailable");
    std::fs::remove_file(&socket).unwrap();
    let missing_target = directory.path().join("missing");
    symlink(&missing_target, &socket).unwrap();
    assert_eq!(connect_error(&socket).raw_os_error(), Some(libc::ENOENT));
    assert_eq!(
        connect_error(&socket.join("child.sock")).raw_os_error(),
        Some(libc::ENOENT)
    );
    std::fs::remove_file(&socket).unwrap();

    let listener = UnixListener::bind(&socket).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = connect_error(&socket);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(denied.raw_os_error(), Some(libc::EACCES));
    }
    assert!(connect(&socket, Instant::now() + Duration::from_secs(1)).is_ok());
    drop(listener);
}

#[test]
fn accepts_native_numbers_unicode_and_escaped_keys() {
    let value = receive(br#"{"a":1.25,"b":1e-2,"c":"\u4e2d","d":"18446744073709551616"}"#).unwrap();
    assert_eq!(value["a"], json!(1.25));
    assert_eq!(value["b"], json!(0.01));
    assert_eq!(value["c"], json!("中"));
}

#[test]
fn rejects_duplicates_trailing_and_overflowing_integers() {
    for bytes in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"n":{"a":1,"\u0061":2}}"#,
        b"{} []",
        b"18446744073709551616",
        b"-9223372036854775809",
        b"",
    ] {
        assert_eq!(
            receive(bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn malformed_numbers_report_json_errors_without_weakening_integer_bounds() {
    for bytes in [b"not-json".as_slice(), b"-", b"a-b", b"--1", br#"{"n":-}"#] {
        assert_eq!(
            receive(bytes).unwrap_err().to_string(),
            "expected bounded JSON without duplicate fields"
        );
    }
    for bytes in [b"18446744073709551616".as_slice(), b"-9223372036854775809"] {
        assert_eq!(
            receive(bytes).unwrap_err().to_string(),
            "integer is outside the supported range"
        );
    }
    for bytes in [
        b"18446744073709551615".as_slice(),
        b"-9223372036854775808",
        b"-0",
        b"-1.25",
        b"1e-2",
    ] {
        assert!(receive(bytes).is_ok());
    }
}

#[test]
fn enforces_exact_container_depth_limit() {
    let valid = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert!(receive(valid.as_bytes()).is_ok());
    let invalid = format!("[{valid}]");
    assert_eq!(
        receive(invalid.as_bytes()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_oversized_prefix_without_waiting_for_payload() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    sender
        .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
        .unwrap();
    let error = read::<Value>(&mut receiver, Instant::now() + Duration::from_secs(1)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn rejects_truncated_prefix_and_payload() {
    for bytes in [&[0, 0][..], &[0, 0, 0, 5, b'{', b'}'][..]] {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(bytes).unwrap();
        drop(sender);
        let error =
            read::<Value>(&mut receiver, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}

#[test]
fn shares_deadline_between_prefix_and_payload() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    sender.write_all(&2_u32.to_be_bytes()).unwrap();
    let start = Instant::now();
    let error = read::<Value>(&mut receiver, start + Duration::from_millis(25)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn initial_reads_reset_the_stall_budget_on_progress() {
    let payload = br#"{"a":1}"#;
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        sender
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        for chunk in payload.chunks(2) {
            thread::sleep(Duration::from_millis(300));
            sender.write_all(chunk).unwrap();
        }
    });
    let value = read_initial::<Value>(
        &mut receiver,
        Duration::from_secs(1),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(value, json!({"a": 1}));
    writer.join().unwrap();
}

#[test]
fn initial_reads_shed_idle_senders_and_reject_out_of_range_budgets() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    sender.write_all(&4_u32.to_be_bytes()).unwrap();
    let start = Instant::now();
    let error = read_initial::<Value>(
        &mut receiver,
        Duration::from_millis(100),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(1));
    for stall in [Duration::ZERO, Duration::from_secs(61)] {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(&4_u32.to_be_bytes()).unwrap();
        assert_eq!(
            read_initial::<Value>(
                &mut receiver,
                stall,
                Instant::now() + Duration::from_secs(60)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn initial_reads_bound_never_idle_senders_at_the_absolute_limit() {
    // However active a sender is, the whole window stays bounded by the
    // absolute limit: the protocol's maximum remaining call deadline.
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        sender.write_all(&1000_u32.to_be_bytes()).unwrap();
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            sender.write_all(b"x").unwrap();
        }
    });
    let start = Instant::now();
    let error = read_initial::<Value>(
        &mut receiver,
        Duration::from_millis(500),
        Instant::now() + Duration::from_millis(1200),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // The limit fires while the writer is still actively sending.
    assert!(start.elapsed() < Duration::from_millis(1900));
    writer.join().unwrap();
}

#[test]
fn writes_round_trip_and_checks_bounds_before_sending() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let value = json!({"tool": "read", "value": 1.2});
    write(&mut sender, &value, deadline).unwrap();
    assert_eq!(read::<Value>(&mut receiver, deadline).unwrap(), value);
    let error = write(&mut sender, &"x".repeat(MAX_FRAME), deadline).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    receiver.set_nonblocking(true).unwrap();
    assert_eq!(
        receiver.read(&mut [0; 4]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn rejects_expired_deadlines_and_invalid_connect_paths() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    let deadline = Instant::now();
    assert_eq!(
        write(&mut sender, &json!({}), deadline).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        read::<Value>(&mut receiver, deadline).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    for path in ["relative", "/has\0nul", &format!("/{}", "a".repeat(200))] {
        assert_eq!(
            connect(Path::new(path), Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
