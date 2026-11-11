use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn compat() -> Value {
    json!({
        "operation": "compat", "version": 2, "source": "external-bash", "remaining_budget_ms": 50,
        "proof": {"phase": "registration"}
    })
}

fn wire(value: &Value) -> Vec<u8> {
    let proof = &value["proof"];
    let mut fields = vec![
        value["version"].to_string(),
        value["source"].as_str().unwrap().to_string(),
    ];
    fields.push(proof["phase"].as_str().unwrap().to_string());
    fields.push(if proof["phase"] == "activation" {
        DIGEST.to_string()
    } else {
        String::new()
    });
    fields.push("123456".into());
    fields.push(value["remaining_budget_ms"].to_string());
    if value["operation"] == "advise" {
        let event = &value["event"];
        fields.extend([
            event["expression"].as_str().unwrap().to_string(),
            event["status"].to_string(),
            event["cwd"].as_str().unwrap().to_string(),
            event["observed_at_ms"].to_string(),
            event["scope"].as_str().unwrap().to_string(),
            event["identity"].as_str().unwrap().to_string(),
            value["host"].as_str().unwrap().to_string(),
            value["session"].as_str().unwrap().to_string(),
            value["generation"].as_str().unwrap().to_string(),
        ]);
    }
    let mut bytes = fields.join("\0").into_bytes();
    bytes.push(0);
    bytes
}

fn request(value: &Value) -> Request {
    decode(value["operation"].as_str().unwrap(), &wire(value)).unwrap()
}

#[test]
fn wire_v2_carries_registration_snapshot_and_an_absolute_deadline() {
    let bytes = wire(&compat());
    let mut fields = std::str::from_utf8(&bytes)
        .unwrap()
        .trim_end_matches('\0')
        .split('\0')
        .map(str::to_string)
        .collect::<Vec<_>>();
    fields[0] = "2".into();

    let encode = |fields: &[String]| {
        let mut bytes = fields.join("\0").into_bytes();
        bytes.push(0);
        bytes
    };
    assert!(decode("compat", &encode(&fields)).is_ok());
    fields[2] = "activation".into();
    assert!(decode("compat", &encode(&fields)).is_err());
    fields[3] = DIGEST.into();
    assert!(decode("compat", &encode(&fields)).is_ok());
    fields[4] = "0".into();
    assert!(decode("compat", &encode(&fields)).is_err());
}

fn render(_event: &BashEvent, _kind: FailureKind, _context_id: &str) -> Option<String> {
    Some("shared renderer fixture".to_string())
}

fn fixture() -> (tempfile::TempDir, GuideStore, RuntimeState) {
    #[cfg(target_os = "linux")]
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = GuideStore::open(root.join("guide")).unwrap();
    let runtime = RuntimeState::open(&root).unwrap();
    store.initialize_runtime(&runtime).unwrap();
    (dir, store, runtime)
}

fn advice(generation: &str) -> Value {
    let mut value = compat();
    value["operation"] = json!("advise");
    value["proof"]["phase"] = json!("activation");
    value["event"] = json!({"expression":"cargo test", "status":101, "cwd":"/project",
        "observed_at_ms":1000, "scope":"observed_command_fragment", "identity":"external_unshadowed"});
    value["host"] = json!("host");
    value["session"] = json!("session");
    value["generation"] = json!(generation);
    value
}

#[test]
fn wire_rejects_version_source_operation_encoding_and_size_mismatches() {
    assert!(decode("compat", &vec![b' '; MAX_BYTES + 1]).is_err());
    assert!(decode("compat", b"\xff").is_err());
    assert!(decode("advise", &wire(&compat())).is_err());
    for (field, replacement) in [
        ("version", json!(1)),
        ("source", json!("cosh")),
        ("remaining_budget_ms", json!(501)),
    ] {
        let mut value = compat();
        value[field] = replacement;
        assert!(decode("compat", &wire(&value)).is_err());
    }
    assert!(decode("compat", &wire(&compat())).is_ok());
}

#[test]
fn wire_has_no_caller_supplied_manifest_or_binary_identity() {
    let bytes = wire(&compat());
    assert_eq!(bytes.split(|byte| *byte == 0).count(), 7);
    let mut extra = bytes;
    extra.extend_from_slice(b"caller-hash\0");
    assert!(decode("compat", &extra).is_err());
}

#[test]
fn compat_requires_trusted_runtime_state_and_returns_its_generation_and_directory() {
    let value = compat();
    assert_eq!(
        evaluate(request(&value), 1000, None, render).decision,
        Status::Disable
    );
    let (_dir, store, runtime) = fixture();
    let result = evaluate(request(&value), 1000, Some((&store, &runtime)), render);
    assert_eq!(result.decision, Status::Show);
    assert_eq!(
        result.generation,
        store.snapshot(Some(&runtime)).unwrap().generation
    );
    assert_eq!(
        result.runtime_directory,
        runtime.directory().to_str().unwrap()
    );
    store.set_guide(Some(&runtime), false).unwrap();
    assert_eq!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Disable
    );
}

#[test]
fn advice_claims_once_and_rejects_closed_or_stale_generations() {
    let (_dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let value = advice(&snapshot.generation);
    let result = evaluate(request(&value), 1000, Some((&store, &runtime)), render);
    assert_eq!(result.decision, Status::Show);
    assert_eq!(result.generation, snapshot.generation);
    assert!(!result.decision_id.is_empty());
    assert_eq!(result.text, "shared renderer fixture");
    let suppressed = evaluate(request(&value), 1000, Some((&store, &runtime)), render);
    assert_eq!(suppressed.decision, Status::Skip);
    assert_eq!(suppressed.reason, "suppressed");
    store.set_guide(Some(&runtime), false).unwrap();
    assert_eq!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Disable
    );
    store.set_guide(Some(&runtime), true).unwrap();
    let stale = evaluate(request(&value), 1000, Some((&store, &runtime)), render);
    assert_eq!(stale.decision, Status::Disable);
    assert_eq!(stale.reason, "stale_generation");
    assert!(stale.text.is_empty());
    assert!(stale.cache_dependencies.is_empty());
}

#[test]
fn unsupported_commands_are_cacheable_only_on_their_actual_dependencies() {
    let (_dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let mut value = advice(&snapshot.generation);
    value["event"]["expression"] = json!("ls absent");
    let result = evaluate(request(&value), 1000, Some((&store, &runtime)), render);
    assert_eq!(result.decision, Status::Skip);
    assert_eq!(result.cache_dependencies, vec!["expression", "status"]);
    assert!(result.text.is_empty());
}

#[test]
fn advice_requires_activation_and_recent_event_and_never_caches_state_failures() {
    let (_dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let mut value = advice(&snapshot.generation);
    value["proof"]["phase"] = json!("registration");
    assert_ne!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Show
    );
    value["proof"]["phase"] = json!("activation");
    value["event"]["observed_at_ms"] = json!(1001);
    assert_ne!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Show
    );
    value["event"]["observed_at_ms"] = json!(1000);
    let result = evaluate(request(&value), 1000, None, render);
    assert_eq!(result.decision, Status::Disable);
    assert!(result.cache_dependencies.is_empty());
}

#[test]
fn final_generation_check_discards_rendered_text_after_off() {
    for reenable in [false, true] {
        let (_dir, store, runtime) = fixture();
        let snapshot = store.snapshot(Some(&runtime)).unwrap();
        let value = advice(&snapshot.generation);
        let result = evaluate(
            request(&value),
            1000,
            Some((&store, &runtime)),
            |_, _, _| {
                store.set_guide(Some(&runtime), false).unwrap();
                if reenable {
                    store.set_guide(Some(&runtime), true).unwrap();
                }
                Some("must not be displayed".to_string())
            },
        );
        assert_eq!(result.decision, Status::Disable, "reenable={reenable}");
        assert_eq!(
            result.reason,
            if reenable {
                "stale_generation"
            } else {
                "guidance_off"
            }
        );
        assert!(result.text.is_empty());
        assert!(result.cache_dependencies.is_empty());
    }
}

#[test]
fn response_encoding_is_bounded_nul_data_and_oversize_fails_closed() {
    let response = Response::new(Status::Disable, "incompatible_runtime");
    let bytes = encode(&response);
    assert!(bytes.len() <= MAX_BYTES);
    let fields: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    assert_eq!(fields.len(), 9);
    assert_eq!(fields[1], b"DISABLE");
    let mut response = Response::new(Status::Show, "suggest_only");
    response.text = "x".repeat(MAX_BYTES);
    let bytes = encode(&response);
    assert!(bytes.len() <= MAX_BYTES);
    let fields: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    assert_eq!(fields.len(), 9);
    assert_eq!(fields[1], b"DISABLE");
    assert!(fields[7].is_empty());
}

#[test]
fn unbounded_or_control_text_never_consumes_an_invitation() {
    let (_dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let value = advice(&snapshot.generation);
    for text in ["x".repeat(MAX_BYTES), "bad\x1b[31mdata".to_string()] {
        let result = evaluate(
            request(&value),
            1000,
            Some((&store, &runtime)),
            |_, _, _| Some(text),
        );
        assert_ne!(result.decision, Status::Show);
        assert!(result.text.is_empty());
    }
    assert_eq!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Show
    );
}

#[test]
fn frame_reader_requires_eof_within_deadline_and_never_exceeds_the_limit() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    let (reader, writer) = nix::unistd::pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(b"{}").unwrap();
    drop(writer);
    assert_eq!(
        read_frame(
            reader.as_raw_fd(),
            Instant::now() + Duration::from_millis(50)
        ),
        Ok(b"{}".to_vec())
    );
    let (reader, _writer) = nix::unistd::pipe().unwrap();
    assert!(read_frame(reader.as_raw_fd(), Instant::now()).is_err());
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![b'x'; MAX_BYTES + 1]).unwrap();
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).unwrap();
    assert!(read_frame(file.as_raw_fd(), Instant::now() + Duration::from_millis(50)).is_err());
}

#[test]
fn full_output_pipe_stops_at_deadline_without_a_reader() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (reader, writer) = nix::unistd::pipe().unwrap();
    let fd = writer.as_raw_fd();
    let original = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    assert!(original >= 0);
    assert_eq!(
        unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, original | nix::libc::O_NONBLOCK) },
        0
    );
    let data = [b'x'; 1024];
    loop {
        let count = unsafe { nix::libc::write(fd, data.as_ptr().cast(), data.len()) };
        if count < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            break;
        }
    }
    assert_eq!(
        unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, original) },
        0
    );
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = write_frame(
            writer.as_raw_fd(),
            b"result",
            Instant::now() + Duration::from_millis(5),
        );
        sender.send(result).unwrap();
    });
    let result = receiver.recv_timeout(Duration::from_millis(100));
    let mut reader = std::fs::File::from(reader);
    if result.is_err() {
        let mut buffer = [0u8; 8192];
        assert!(reader.read(&mut buffer).unwrap() > 0);
        let _ = receiver.recv_timeout(Duration::from_secs(1));
    }
    worker.join().unwrap();
    assert_eq!(result, Ok(Err(())));
}

#[test]
fn output_preserves_regular_file_offset_and_socket_flags() {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"before:").unwrap();
    let flags = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        write_frame(
            file.as_raw_fd(),
            b"after",
            Instant::now() + Duration::from_secs(1)
        ),
        Ok(())
    );
    assert_eq!(file.stream_position().unwrap(), 12);
    assert_eq!(
        unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL) },
        flags
    );
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "before:after");

    let (writer, mut reader) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let flags = unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        write_frame(
            writer.as_raw_fd(),
            b"result",
            Instant::now() + Duration::from_secs(1)
        ),
        Ok(())
    );
    let mut bytes = [0; 6];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"result");
    assert_eq!(
        unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) },
        flags
    );
}

#[cfg(target_os = "linux")]
#[test]
fn pipe_output_preserves_the_callers_file_status_flags() {
    use std::io::Read;
    use std::os::fd::AsRawFd;

    let (reader, writer) = nix::unistd::pipe().unwrap();
    let flags = unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        write_frame(
            writer.as_raw_fd(),
            b"result",
            Instant::now() + Duration::from_secs(1)
        ),
        Ok(())
    );
    let mut bytes = [0; 6];
    std::fs::File::from(reader).read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"result");
    assert_eq!(
        unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) },
        flags
    );
}

#[test]
fn tty_writer_child_process() {
    if std::env::var_os("COSH_TEST_TTY_WRITER_CHILD").is_none() {
        return;
    }
    let ready = unsafe { nix::libc::write(4, b"ready".as_ptr().cast(), 5) };
    if ready != 5 {
        std::process::exit(2);
    }
    let result = write_frame(
        3,
        &vec![b'x'; 8192],
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    );
    std::process::exit(i32::from(result.is_err()));
}

#[test]
fn killed_tty_writer_preserves_the_parent_terminal_file_status_flags() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    for kind in ["tty", "pipe"] {
        let (reader, writer_fd) = if kind == "pipe" {
            nix::unistd::pipe().unwrap()
        } else {
            let terminal = nix::pty::openpty(None, None).unwrap();
            (terminal.master, terminal.slave)
        };
        let fd = writer_fd.as_raw_fd();
        for descriptor in [reader.as_raw_fd(), fd] {
            assert_eq!(
                unsafe { nix::libc::fcntl(descriptor, nix::libc::F_SETFD, nix::libc::FD_CLOEXEC) },
                0
            );
        }
        let original = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        assert!(original >= 0);
        assert_eq!(
            unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, original | nix::libc::O_NONBLOCK) },
            0
        );
        let bytes = [b'x'; 1024];
        let mut full = false;
        for _ in 0..2048 {
            let count = unsafe { nix::libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if count < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                full = true;
                break;
            }
        }
        assert_eq!(
            unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, original) },
            0
        );
        assert!(full);
        let parent_flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        assert_eq!(parent_flags & nix::libc::O_NONBLOCK, 0);
        let (mut ready, writer) = UnixStream::pair().unwrap();
        ready
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let tty_copy = unsafe { nix::libc::fcntl(fd, nix::libc::F_DUPFD_CLOEXEC, 10) };
        let ready_copy =
            unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 10) };
        assert!(tty_copy >= 0 && ready_copy >= 0);
        let tty_copy = unsafe { OwnedFd::from_raw_fd(tty_copy) };
        let ready_copy = unsafe { OwnedFd::from_raw_fd(ready_copy) };
        let tty_raw = tty_copy.as_raw_fd();
        let ready_raw = ready_copy.as_raw_fd();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::shell_integration::tests::tty_writer_child_process",
                "--test-threads=1",
            ])
            .env("COSH_TEST_TTY_WRITER_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if nix::libc::dup2(tty_raw, 3) < 0 || nix::libc::dup2(ready_raw, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let mut marker = [0u8; 5];
        if let Err(error) = ready.read_exact(&mut marker) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not reach the writer: {error}");
        }
        assert_eq!(&marker, b"ready");
        std::thread::sleep(Duration::from_millis(50));
        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let status = child.wait().unwrap();
        if kind == "tty" || cfg!(target_os = "linux") {
            assert!(running, "writer did not encounter {kind} backpressure");
            assert_eq!(status.signal(), Some(nix::libc::SIGKILL));
        } else {
            assert!(
                !status.success(),
                "unsupported pipe must fail without changing flags"
            );
        }
        assert_eq!(
            unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) },
            parent_flags
        );
    }
}

#[test]
fn shown_context_reference_resolves_to_the_exact_observation() {
    let (_dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let mut value = advice(&snapshot.generation);
    value["event"]["expression"] = json!("cargo test --test 'my suite'");
    let expected = match request(&value) {
        Request::Advise { event, .. } => event,
        _ => unreachable!(),
    };
    let result = evaluate(
        request(&value),
        1000,
        Some((&store, &runtime)),
        |_, _, id| Some(format!("cosh --ask 'Analyze this failure' --context {id}")),
    );
    assert_eq!(result.decision, Status::Show);
    assert!(result.text.ends_with(&result.decision_id));
    assert_eq!(
        store
            .load_failure_context(&result.decision_id, 1001)
            .unwrap(),
        expected
    );
}

#[test]
fn failed_context_persistence_does_not_consume_an_invitation() {
    let (dir, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let value = advice(&snapshot.generation);
    let path = dir.path().join("guide/failure-contexts.json");
    std::fs::write(&path, b"broken").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Disable
    );
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        evaluate(request(&value), 1000, Some((&store, &runtime)), render).decision,
        Status::Show
    );
}
