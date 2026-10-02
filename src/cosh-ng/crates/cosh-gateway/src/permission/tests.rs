use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use crate::runtime::{
    AcpV1PermissionOption, AcpV1PermissionOptionKind, AcpV1PermissionRequest, AcpV1RequestId,
};

use super::*;

#[derive(Clone, Default)]
struct MemoryEvidence(Rc<RefCell<Vec<PermissionEvidence>>>);

impl PermissionEvidenceSink for MemoryEvidence {
    fn record(&mut self, evidence: &PermissionEvidence) -> Result<(), PermissionEvidenceError> {
        self.0.borrow_mut().push(evidence.clone());
        Ok(())
    }
}

fn request() -> AcpV1PermissionRequest {
    AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(7),
        session_id: "provider-session-secret".to_owned(),
        tool_call: serde_json::json!({
            "toolCallId": "tool-secret",
            "title": "Run tests",
            "rawInput": {"token": "credential-secret"}
        }),
        callback_payload_digest: cosh_gateway_contracts::common::Digest::parse("5".repeat(64))
            .unwrap(),
        options: vec![
            AcpV1PermissionOption {
                option_id: "allow".to_owned(),
                name: "Always trust me".to_owned(),
                kind: AcpV1PermissionOptionKind::AllowOnce,
            },
            AcpV1PermissionOption {
                option_id: "reject".to_owned(),
                name: "Reject".to_owned(),
                kind: AcpV1PermissionOptionKind::RejectOnce,
            },
            AcpV1PermissionOption {
                option_id: "always".to_owned(),
                name: "Allow always".to_owned(),
                kind: AcpV1PermissionOptionKind::AllowAlways,
            },
        ],
    }
}

fn context() -> PermissionEvidenceContext<'static> {
    PermissionEvidenceContext {
        profile: "codex",
        canonical_workspace: b"/private/workspace",
        actor_uid: 1000,
        occurred_at_ms: 123,
    }
}

#[test]
fn allow_once_is_correlated_and_evidence_contains_only_digests() {
    let output = Rc::new(RefCell::new(Vec::new()));
    struct SharedOutput(Rc<RefCell<Vec<u8>>>);
    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let presenter = TextPermissionPresenter::new(Cursor::new(b"a\n"), SharedOutput(output));
    let evidence = MemoryEvidence::default();
    let captured = evidence.clone();
    let mut proxy = OncePermissionProxy::new(presenter, evidence);

    assert_eq!(
        proxy.resolve(context(), &request()).unwrap(),
        AcpV1PermissionDecision::Selected {
            option_id: "allow".to_owned()
        }
    );
    let records = captured.0.borrow();
    assert_eq!(records[0].decision, OncePermissionDecision::AllowOnce);
    let encoded = serde_json::to_string(&records[0]).unwrap();
    assert!(!encoded.contains("credential-secret"));
    assert!(!encoded.contains("provider-session-secret"));
    assert!(!encoded.contains("/private/workspace"));
}

#[test]
fn unsupported_or_eof_choice_cancels_without_durable_rule() {
    let presenter = TextPermissionPresenter::new(Cursor::new(Vec::<u8>::new()), Vec::<u8>::new());
    let evidence = MemoryEvidence::default();
    let captured = evidence.clone();
    let mut proxy = OncePermissionProxy::new(presenter, evidence);

    assert_eq!(
        proxy.resolve(context(), &request()).unwrap(),
        AcpV1PermissionDecision::Cancelled
    );
    assert_eq!(
        captured.0.borrow()[0].decision,
        OncePermissionDecision::Cancelled
    );
}

#[test]
fn durable_only_options_cannot_be_selected_by_a_spoofed_label() {
    let mut request = request();
    request.options.retain(|option| {
        matches!(
            option.kind,
            AcpV1PermissionOptionKind::AllowAlways | AcpV1PermissionOptionKind::RejectAlways
        )
    });
    request.options[0].name = "allow_once".to_owned();
    let evidence = MemoryEvidence::default();
    let captured = evidence.clone();
    let mut proxy = OncePermissionProxy::new(
        TextPermissionPresenter::new(Cursor::new(b"a\n"), Vec::<u8>::new()),
        evidence,
    );

    assert_eq!(
        proxy.resolve(context(), &request).unwrap(),
        AcpV1PermissionDecision::Cancelled
    );
    assert_eq!(
        captured.0.borrow()[0].decision,
        OncePermissionDecision::Cancelled
    );
}

#[test]
fn terminal_title_is_bounded_and_strips_control_injection() {
    let mut request = request();
    request.tool_call["title"] = serde_json::Value::String(format!(
        "safe\u{1b}[2J\r{}",
        "x".repeat(MAX_DISPLAY_BYTES * 2)
    ));
    let output = Rc::new(RefCell::new(Vec::new()));
    struct SharedOutput(Rc<RefCell<Vec<u8>>>);
    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let presenter =
        TextPermissionPresenter::new(Cursor::new(b"r\n"), SharedOutput(Rc::clone(&output)));
    let mut proxy = OncePermissionProxy::new(presenter, MemoryEvidence::default());
    proxy.resolve(context(), &request).unwrap();

    let rendered = output.borrow();
    assert!(!rendered.contains(&0x1b));
    assert!(!rendered.contains(&b'\r'));
    assert!(rendered.len() < MAX_DISPLAY_BYTES + 128);
}

#[cfg(unix)]
#[test]
fn file_evidence_requires_private_directory_and_syncs_jsonl() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("permission.jsonl");
    let mut sink = FilePermissionEvidenceSink::open(&path).unwrap();
    let mut proxy = OncePermissionProxy::new(
        TextPermissionPresenter::new(Cursor::new(b"r\n"), Vec::<u8>::new()),
        &mut sink,
    );
    assert_eq!(
        proxy.resolve(context(), &request()).unwrap(),
        AcpV1PermissionDecision::Selected {
            option_id: "reject".to_owned()
        }
    );
    drop(proxy);
    drop(sink);
    let content = std::fs::read_to_string(path).unwrap();
    assert!(content.contains("\"reject_once\""));
    assert!(!content.contains("credential-secret"));
}

#[cfg(unix)]
#[test]
fn evidence_rejects_symlink_and_public_existing_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("target.jsonl");
    std::fs::write(&target, "").unwrap();
    let link = root.path().join("link.jsonl");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        FilePermissionEvidenceSink::open(link),
        Err(PermissionEvidenceError::UnsafePath)
    ));

    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        FilePermissionEvidenceSink::open(target),
        Err(PermissionEvidenceError::UnsafePath)
    ));
}

#[cfg(unix)]
#[test]
fn private_state_creation_rejects_symlinked_parent() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("state");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        FilePermissionEvidenceSink::open_in_private_state(link.join("permission.jsonl")),
        Err(PermissionEvidenceError::UnsafePath)
    ));
}
