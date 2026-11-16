//! Focused tests for the guide login privilege protocol (no sudo, no writes to
//! host accounts): file/identity boundaries and the read-only query worker.

use std::os::unix::fs::{symlink, PermissionsExt};

use super::guide_protocol::{
    classify, failure_from_status, parse_sudo_uid, private_file, read_request, run_worker,
    run_worker_for, validate_identity, HandoffFailure, Outcome, Reply, Request, MAX_REPLY_BYTES,
    MAX_REQUEST_BYTES, VERSION, WORKER_FLAG,
};

#[test]
fn non_zero_handoff_status_distinguishes_cancel_usage_and_failure() {
    use std::os::unix::process::ExitStatusExt;
    let status = std::process::ExitStatus::from_raw;
    assert_eq!(failure_from_status(status(2)), HandoffFailure::Cancelled);
    assert_eq!(failure_from_status(status(15)), HandoffFailure::Cancelled);
    assert_eq!(failure_from_status(status(2 << 8)), HandoffFailure::Usage);
    assert_eq!(failure_from_status(status(1 << 8)), HandoffFailure::Exit(1));
    assert_eq!(
        failure_from_status(status(130 << 8)),
        HandoffFailure::Exit(130)
    );
}

fn reply_json(username: &str, uid: u32, shell: &str) -> String {
    serde_json::json!({
        "version": VERSION,
        "outcome": "ok",
        "username": username,
        "uid": uid,
        "current_shell": shell,
        "original_bash": null,
        "managed": false,
        "conflict": false,
        "error": null,
    })
    .to_string()
}

#[test]
fn classify_verifies_only_when_identity_and_target_match() {
    let raw = reply_json("alice", 1000, "/usr/bin/cosh");
    assert!(matches!(
        classify(Some(&raw), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Verified(_)
    ));
    assert_eq!(
        classify(Some(&raw), "alice", 1000, "/bin/bash"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        classify(Some(&raw), "bob", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        classify(Some(&raw), "alice", 1001, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        classify(None, "alice", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    assert_eq!(
        classify(Some("not json"), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
}

#[test]
fn classify_rejects_wrong_version_and_reports_errors() {
    let mut value: serde_json::Value =
        serde_json::from_str(&reply_json("alice", 1000, "/usr/bin/cosh")).unwrap();
    value["version"] = serde_json::json!(VERSION + 1);
    assert_eq!(
        classify(Some(&value.to_string()), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
    value["version"] = serde_json::json!(VERSION);
    value["outcome"] = serde_json::json!("error");
    value["error"] = serde_json::json!("boom");
    assert_eq!(
        classify(Some(&value.to_string()), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Error("boom".into())
    );
}

#[test]
fn query_worker_returns_status_or_structured_platform_refusal() {
    let request = caller_request("query");
    let input = request_file(&request);
    let mut output = tempfile::NamedTempFile::new().unwrap();
    assert_eq!(
        run_worker_for(
            input.path().to_str().unwrap(),
            output.as_file_mut(),
            request.expected_uid
        ),
        0
    );
    let reply: Reply =
        serde_json::from_str(&std::fs::read_to_string(output.path()).unwrap()).unwrap();
    assert_eq!(reply.uid, request.expected_uid);
    assert_eq!(reply.username, request.username);
    let expected = super::account::require_platform()
        .map_err(|error| error.to_string())
        .and_then(|()| super::query_status(Some(&request.username)));
    match expected {
        Ok(status) => {
            assert_eq!(reply.outcome, "ok");
            assert_eq!(reply.current_shell, status.current_shell);
        }
        Err(error) => {
            assert_eq!(reply.outcome, "error");
            assert_eq!(reply.error.as_deref(), Some(error.as_str()));
        }
    }
}

#[test]
fn worker_flag_is_private_and_versioned() {
    assert_eq!(WORKER_FLAG, format!("--__cosh-login-guide-v{VERSION}"));
}

#[test]
fn worker_rejects_caller_selected_result_path_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let request_path = dir.path().join("request.json");
    let victim = dir.path().join("unrelated-file");
    std::fs::write(&victim, "preserve me").unwrap();
    let request = serde_json::json!({
        "version": 1,
        "action": "bogus",
        "username": "alice",
        "expected_uid": 1000,
        "result_path": victim,
    });
    std::fs::write(&request_path, request.to_string()).unwrap();
    assert_eq!(run_worker(request_path.to_str().unwrap()), 1);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "preserve me");
}

#[test]
fn classify_rejects_oversized_replies() {
    let mut value: serde_json::Value =
        serde_json::from_str(&reply_json("alice", 1000, "/usr/bin/cosh")).unwrap();
    value["outcome"] = serde_json::json!("error");
    value["error"] = serde_json::json!("x".repeat(MAX_REPLY_BYTES + 1));
    assert!(matches!(
        classify(Some(&value.to_string()), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    ));
}

fn sample_request() -> Request {
    Request {
        version: VERSION,
        action: "set".into(),
        username: "alice".into(),
        expected_uid: 1000,
    }
}

#[test]
fn worker_error_reply_carries_identity_and_classifies_as_error() {
    let request = sample_request();
    let reply = Reply::error(&request, "boom".into());
    assert_eq!(reply.username, "alice");
    assert_eq!(reply.uid, 1000);
    let raw = serde_json::to_string(&reply).unwrap();
    assert_eq!(
        classify(Some(&raw), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Error("boom".into())
    );
}

#[test]
fn worker_post_write_uncertainty_classifies_as_unconfirmed() {
    let reply = Reply::unconfirmed(&sample_request());
    let raw = serde_json::to_string(&reply).unwrap();
    assert_eq!(
        classify(Some(&raw), "alice", 1000, "/usr/bin/cosh"),
        Outcome::Unconfirmed
    );
}

#[test]
fn worker_unknown_action_error_reaches_classify() {
    let request = caller_request("bogus");
    let input = request_file(&request);
    let mut output = tempfile::NamedTempFile::new().unwrap();
    assert_eq!(
        run_worker_for(
            input.path().to_str().unwrap(),
            output.as_file_mut(),
            request.expected_uid
        ),
        0
    );
    let raw = std::fs::read_to_string(output.path()).unwrap();
    assert_eq!(
        classify(
            Some(&raw),
            &request.username,
            request.expected_uid,
            "/usr/bin/cosh"
        ),
        Outcome::Error("unknown login action".into())
    );
}

#[test]
fn inherited_pipe_carries_the_reply_for_sudo_output_auditing() {
    use std::io::Read;

    let request = caller_request("bogus");
    let input = request_file(&request);
    let (reader, writer) = nix::unistd::pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    assert_eq!(
        run_worker_for(
            input.path().to_str().unwrap(),
            &mut writer,
            request.expected_uid
        ),
        0
    );
    use std::os::fd::AsRawFd;
    let flags = unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) };
    assert_ne!(flags & nix::libc::O_NONBLOCK, 0);
    drop(writer);
    let mut raw = String::new();
    std::fs::File::from(reader)
        .read_to_string(&mut raw)
        .unwrap();
    assert_eq!(
        classify(
            Some(&raw),
            &request.username,
            request.expected_uid,
            "/usr/bin/cosh"
        ),
        Outcome::Error("unknown login action".into())
    );
}

#[test]
fn a_full_inherited_result_pipe_returns_an_unconfirmed_transport_failure() {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let request = caller_request("bogus");
    let input = request_file(&request);
    let (_reader, writer) = nix::unistd::pipe().unwrap();
    let mut writer = std::fs::File::from(writer);
    let flags = unsafe { nix::libc::fcntl(writer.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                writer.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0
    );
    let mut filled = 0;
    loop {
        match writer.write(&[0; 4096]) {
            Ok(0) => panic!("result pipe stopped accepting data before becoming full"),
            Ok(written) => filled += written,
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
                break;
            }
        }
    }
    assert!(filled > 0);
    assert_eq!(
        run_worker_for(
            input.path().to_str().unwrap(),
            &mut writer,
            request.expected_uid
        ),
        1
    );
}

fn caller_request(action: &str) -> Request {
    let uid = unsafe { nix::libc::getuid() };
    let caller = super::account::lookup_nss(None, uid).unwrap();
    Request {
        version: VERSION,
        action: action.into(),
        username: caller.name,
        expected_uid: uid,
    }
}

fn request_file(request: &Request) -> tempfile::NamedTempFile {
    let input = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(input.path(), serde_json::to_vec(request).unwrap()).unwrap();
    input
}

#[test]
fn sudo_uid_requires_unsigned_decimal_without_whitespace_or_overflow() {
    assert_eq!(parse_sudo_uid("0"), Some(0));
    assert_eq!(parse_sudo_uid("1000"), Some(1000));
    for value in ["", "+1000", "-1", " 1000", "1000\n", "4294967296", "١"] {
        assert_eq!(parse_sudo_uid(value), None, "{value:?}");
    }
}

#[test]
fn identity_is_bound_to_the_callers_uid_and_nss_name() {
    let mut request = caller_request("bogus");
    let caller_uid = request.expected_uid;
    assert!(validate_identity(&request, caller_uid));
    request.expected_uid = caller_uid.wrapping_add(1);
    assert!(!validate_identity(&request, caller_uid));
    request.expected_uid = caller_uid;
    request.username = "not-a-real-cosh-user".into();
    assert!(!validate_identity(&request, caller_uid));
}

#[test]
fn identity_mismatch_is_rejected_before_query_or_reply_write() {
    let mut request = caller_request("query");
    let caller_uid = request.expected_uid;
    request.expected_uid = caller_uid.wrapping_add(1);
    let input = request_file(&request);
    let mut output = tempfile::NamedTempFile::new().unwrap();
    assert_eq!(
        run_worker_for(
            input.path().to_str().unwrap(),
            output.as_file_mut(),
            caller_uid
        ),
        1
    );
    assert_eq!(output.as_file().metadata().unwrap().len(), 0);
}

#[test]
fn request_rejects_symlinks_directories_shared_modes_and_wrong_owner() {
    let request = caller_request("bogus");
    let input = request_file(&request);
    let uid = request.expected_uid;
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("request-link");
    symlink(input.path(), &link).unwrap();
    assert!(read_request(link.to_str().unwrap(), uid).is_none());
    assert!(read_request(dir.path().to_str().unwrap(), uid).is_none());
    assert!(read_request(input.path().to_str().unwrap(), uid.wrapping_add(1)).is_none());
    std::fs::set_permissions(input.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_request(input.path().to_str().unwrap(), uid).is_none());
    std::fs::set_permissions(input.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let hardlink = dir.path().join("request-hardlink");
    std::fs::hard_link(input.path(), &hardlink).unwrap();
    assert!(read_request(input.path().to_str().unwrap(), uid).is_none());
}

#[test]
fn request_rejects_oversize_unknown_fields_and_old_versions() {
    let request = caller_request("bogus");
    let input = request_file(&request);
    let path = input.path().to_str().unwrap();
    assert!(read_request(path, request.expected_uid).is_some());
    let mut value = serde_json::to_value(&request).unwrap();
    value["result_path"] = serde_json::json!("/tmp/not-opened");
    std::fs::write(input.path(), value.to_string()).unwrap();
    assert!(read_request(path, request.expected_uid).is_none());
    value.as_object_mut().unwrap().remove("result_path");
    value["version"] = serde_json::json!(VERSION - 1);
    std::fs::write(input.path(), value.to_string()).unwrap();
    assert!(read_request(path, request.expected_uid).is_none());
    std::fs::write(input.path(), " ".repeat(MAX_REQUEST_BYTES + 1)).unwrap();
    assert!(read_request(path, request.expected_uid).is_none());
}

#[test]
fn result_fd_rejects_existing_content_shared_modes_and_wrong_owner() {
    let request = caller_request("bogus");
    let input = request_file(&request);
    let path = input.path().to_str().unwrap();
    let mut output = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(output.path(), "preserve me").unwrap();
    assert_eq!(
        run_worker_for(path, output.as_file_mut(), request.expected_uid),
        1
    );
    assert_eq!(
        std::fs::read_to_string(output.path()).unwrap(),
        "preserve me"
    );
    output.as_file_mut().set_len(0).unwrap();
    std::fs::set_permissions(output.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        run_worker_for(path, output.as_file_mut(), request.expected_uid),
        1
    );
    std::fs::set_permissions(output.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!private_file(
        &output.as_file().metadata().unwrap(),
        request.expected_uid.wrapping_add(1)
    ));
}

#[test]
fn result_fd_rejects_read_only_files_nonzero_offsets_and_devices() {
    use std::io::{Seek, SeekFrom};

    let request = caller_request("bogus");
    let input = request_file(&request);
    let path = input.path().to_str().unwrap();
    let mut output = tempfile::NamedTempFile::new().unwrap();
    let mut read_only = std::fs::File::open(output.path()).unwrap();
    assert_eq!(
        run_worker_for(path, &mut read_only, request.expected_uid),
        1
    );
    output.as_file_mut().seek(SeekFrom::Start(100)).unwrap();
    assert_eq!(
        run_worker_for(path, output.as_file_mut(), request.expected_uid),
        1
    );
    let mut device = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    assert_eq!(run_worker_for(path, &mut device, request.expected_uid), 1);
}

#[test]
fn platform_fixtures_distinguish_supported_linux_from_ubuntu() {
    assert!(super::account::validate_platform("ID=alinux\nVERSION_ID=4.0\n").is_ok());
    assert!(super::account::validate_platform("ID=ubuntu\nVERSION_ID=24.04\n").is_err());
}
