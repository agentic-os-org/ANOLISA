use super::*;
use chrono::Utc;

#[test]
fn subsystem_serializes_as_lowercase() {
    let s = serde_json::to_string(&ActionSubsystem::Pkg).unwrap();
    assert_eq!(s, "\"pkg\"");
    let s = serde_json::to_string(&ActionSubsystem::Checkpoint).unwrap();
    assert_eq!(s, "\"checkpoint\"");
}

#[test]
fn subsystem_from_token_is_case_insensitive() {
    assert_eq!(ActionSubsystem::from_token("pkg"), ActionSubsystem::Pkg);
    assert_eq!(ActionSubsystem::from_token("PKG"), ActionSubsystem::Pkg);
    assert_eq!(
        ActionSubsystem::from_token("custom"),
        ActionSubsystem::Other("custom".to_string())
    );
}

#[test]
fn outcome_serializes_pascal_case() {
    assert_eq!(serde_json::to_string(&Outcome::Allow).unwrap(), "\"Allow\"");
    assert_eq!(
        serde_json::to_string(&Outcome::RequireApproval).unwrap(),
        "\"RequireApproval\""
    );
}

#[test]
fn action_round_trip() {
    let act = Action {
        subsystem: ActionSubsystem::Pkg,
        operation: "install".to_string(),
        target: Some("nginx".to_string()),
        args: vec![("dry-run".to_string(), "true".to_string())],
        raw: Some("pkg install nginx --dry-run".to_string()),
    };
    let json = serde_json::to_string(&act).unwrap();
    let back: Action = serde_json::from_str(&json).unwrap();
    assert_eq!(act, back);
}

#[test]
fn decision_round_trip() {
    let dec = Decision {
        outcome: Outcome::Deny,
        reason: "destructive".to_string(),
        matched_rule: Some("shell-deny-destructive".to_string()),
        policy_version: "builtin-balanced@0.2.0+sha256:abc".to_string(),
    };
    let json = serde_json::to_string(&dec).unwrap();
    let back: Decision = serde_json::from_str(&json).unwrap();
    assert_eq!(dec, back);
}

#[test]
fn string_match_untagged_round_trip() {
    // Exact via plain string
    let exact: StringMatch = serde_json::from_str("\"install\"").unwrap();
    assert_eq!(exact, StringMatch::Exact("install".to_string()));

    // OneOf
    let oo: StringMatch = serde_json::from_str(r#"{"one_of":["a","b"]}"#).unwrap();
    assert_eq!(
        oo,
        StringMatch::OneOf {
            one_of: vec!["a".to_string(), "b".to_string()]
        }
    );

    // Glob
    let g: StringMatch = serde_json::from_str(r#"{"glob":"ng*"}"#).unwrap();
    assert_eq!(
        g,
        StringMatch::Glob {
            glob: "ng*".to_string()
        }
    );
}

#[test]
fn match_is_empty_detects_blank_match() {
    let m = Match::default();
    assert!(m.is_empty());

    let m = Match {
        operation: Some(StringMatch::Exact("install".to_string())),
        ..Match::default()
    };
    assert!(!m.is_empty());
}

// TOML-based load/validation tests live in `cosh-platform::audit::policy`,
// which owns the TOML adapter and `Policy::from_toml_str` validation.

#[test]
fn log_entry_round_trip() {
    let entry = LogEntry {
        timestamp: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        session_id: "sess-1".to_string(),
        user: "alice".to_string(),
        uid: 1000,
        euid: 1000,
        sudo_user: None,
        pid: 1234,
        action: Action {
            subsystem: ActionSubsystem::Pkg,
            operation: "install".to_string(),
            target: Some("nginx".to_string()),
            args: vec![],
            raw: None,
        },
        decision: Decision {
            outcome: Outcome::Allow,
            reason: "default".to_string(),
            matched_rule: None,
            policy_version: "builtin-permissive@0.2.0+sha256:abc".to_string(),
        },
        source: LogSource::Cli,
        redacted: false,
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: LogEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(entry, back);
}

#[test]
fn log_source_internally_tagged() {
    let s = serde_json::to_string(&LogSource::Cli).unwrap();
    assert!(s.contains("\"kind\":\"cli\""), "got {}", s);

    let s = serde_json::to_string(&LogSource::Tui {
        tool_name: "shell".to_string(),
    })
    .unwrap();
    assert!(s.contains("\"kind\":\"tui\""), "got {}", s);
    assert!(s.contains("\"tool_name\":\"shell\""), "got {}", s);
}

fn v1_event(event_type: AuditEventType, identity: AuditIdentity) -> AuditEventV1 {
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    AuditEventV1::new(
        "event-1".to_string(),
        event_type,
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshCore,
            version: "0.12.0".to_string(),
        },
        identity,
        AuditActor {
            kind: AuditActorKind::Agent,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Started,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "tool".to_string(),
            name: None,
        },
        &AuditToolData {
            tool_kind: "fixture".to_string(),
            ..AuditToolData::default()
        },
        AuditRedaction {
            policy_version: "audit-redaction-v1".to_string(),
            status: AuditRedactionStatus::Clean,
            fields: Vec::new(),
        },
    )
    .unwrap()
}

#[test]
fn v1_wire_names_and_units_are_stable() {
    let event = v1_event(
        KnownAuditEventType::ToolRequested.into(),
        AuditIdentity {
            run_id: Some("run-1".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..AuditIdentity::default()
        },
    );
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["schema"], AUDIT_EVENT_SCHEMA);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["event_type"], "tool.requested");
    assert!(value.get("occurred_at").is_some());
    assert!(value.get("observed_at").is_some());
    assert!(value["data"].get("duration_ms").is_none());
}

#[test]
fn v1_timestamps_serialize_as_utc_milliseconds() {
    let mut event = v1_event(
        KnownAuditEventType::ToolRequested.into(),
        AuditIdentity {
            run_id: Some("run-1".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..AuditIdentity::default()
        },
    );
    event.occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-23T09:07:15.303017+08:00")
        .unwrap()
        .with_timezone(&Utc);

    let value = serde_json::to_value(event).unwrap();

    assert_eq!(value["occurred_at"], "2026-07-23T01:07:15.303Z");
}

#[test]
fn known_event_rejects_unknown_payload_fields() {
    let mut event = v1_event(
        KnownAuditEventType::ToolRequested.into(),
        AuditIdentity {
            run_id: Some("run-1".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..AuditIdentity::default()
        },
    );
    event.data = serde_json::json!({
        "tool_kind": "shell",
        "raw_command": "secret"
    });

    assert!(event.validate().is_err());
}

#[test]
fn v1_tool_and_shell_identity_minima_fail_closed() {
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let result = AuditEventV1::new(
        "event-1".to_string(),
        KnownAuditEventType::ShellCommandStarted.into(),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshShell,
            version: "0.12.0".to_string(),
        },
        AuditIdentity::default(),
        AuditActor {
            kind: AuditActorKind::User,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Started,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "shell_command".to_string(),
            name: None,
        },
        &AuditShellCommandData::default(),
        AuditRedaction {
            policy_version: "audit-redaction-v1".to_string(),
            status: AuditRedactionStatus::Dropped,
            fields: Vec::new(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn unknown_event_payload_is_bounded() {
    let mut event = v1_event(
        AuditEventType::Unknown("future.event".to_string()),
        AuditIdentity::default(),
    );
    event.data = serde_json::json!({"future": "x".repeat(MAX_UNKNOWN_DATA_BYTES + 1)});
    assert!(event.validate().is_err());
}
