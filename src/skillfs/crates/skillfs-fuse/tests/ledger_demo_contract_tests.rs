//! D1.0 ledger demo contract — integration coverage.
//!
//! Most of the parser/validator/mapping coverage lives next to the
//! implementation in `crates/skillfs-fuse/src/security/{ledger,active}.rs`
//! so the modules can be inspected in isolation. This file pins the
//! public surface from the perspective of an external embedder — exactly
//! what the future demo hook handler and the CLI wiring will consume:
//!
//! * the demo §4.2 JSON parses through the re-exported types in
//!   `skillfs_fuse::security`;
//! * the resolver produces the right [`ActiveTarget`] for each of the
//!   three decisions;
//! * the [`StaticLedgerAdapter`] can drive the resolver end-to-end
//!   without a real subprocess;
//! * "flags absent means behavior unchanged" — building a baseline
//!   resolver, never touching the adapter, and never installing any
//!   active target leaves the runtime exactly as it was before D1.0
//!   (no entries, no side-effects).
//!
//! These tests are pure-Rust: they do not require `/dev/fuse`,
//! `fusermount3`, or root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use skillfs_fuse::security::{
    ActiveResolverError, ActiveSkillResolver, ActiveTarget, DecisionCommand, LEDGER_SCHEMA_VERSION,
    LEDGER_SNAPSHOT_PREFIX, LedgerAdapter, LedgerDecision, LedgerError, LedgerResolveResult,
    LedgerStatus, LedgerTargetKind, StaticLedgerAdapter,
};

fn current_payload() -> &'static str {
    r#"{
        "schemaVersion": 1,
        "skillName": "demo-weather",
        "status": "pass",
        "decision": "current",
        "currentVersion": "v000001",
        "trustedVersion": "v000001"
    }"#
}

fn fallback_payload() -> &'static str {
    r#"{
        "schemaVersion": 1,
        "skillName": "demo-weather",
        "status": "deny",
        "decision": "fallback",
        "currentVersion": "v000003",
        "trustedVersion": "v000001",
        "target": ".skill-meta/versions/v000001.snapshot",
        "targetKind": "relative_to_skill_dir",
        "reason": "current version has high-risk findings"
    }"#
}

fn hidden_payload() -> &'static str {
    r#"{
        "schemaVersion": 1,
        "skillName": "demo-weather",
        "status": "none",
        "decision": "hidden",
        "reason": "no certified version yet"
    }"#
}

#[test]
fn pins_public_constants_for_demo_contract() {
    assert_eq!(LEDGER_SCHEMA_VERSION, 1);
    assert_eq!(LEDGER_SNAPSHOT_PREFIX, ".skill-meta/versions");
}

#[test]
fn parses_current_payload_through_reexports() {
    let r = LedgerResolveResult::from_json_str(current_payload()).unwrap();
    assert_eq!(r.status, LedgerStatus::Pass);
    assert_eq!(r.decision, LedgerDecision::Current);
    assert!(r.target.is_none());
    assert!(r.target_kind.is_none());
}

#[test]
fn parses_fallback_payload_through_reexports() {
    let r = LedgerResolveResult::from_json_str(fallback_payload()).unwrap();
    assert_eq!(r.status, LedgerStatus::Deny);
    assert_eq!(r.decision, LedgerDecision::Fallback);
    assert_eq!(
        r.target.as_deref(),
        Some(Path::new(".skill-meta/versions/v000001.snapshot"))
    );
    assert_eq!(r.target_kind, Some(LedgerTargetKind::RelativeToSkillDir));
}

#[test]
fn parses_hidden_payload_through_reexports() {
    let r = LedgerResolveResult::from_json_str(hidden_payload()).unwrap();
    assert_eq!(r.status, LedgerStatus::None);
    assert_eq!(r.decision, LedgerDecision::Hidden);
    assert_eq!(r.reason.as_deref(), Some("no certified version yet"));
}

#[test]
fn rejects_unsupported_schema_version_in_demo_payload() {
    let json = r#"{
        "schemaVersion": 99,
        "skillName": "demo-weather",
        "status": "pass",
        "decision": "current"
    }"#;
    let err = LedgerResolveResult::from_json_str(json).unwrap_err();
    assert!(matches!(err, LedgerError::UnsupportedSchemaVersion { .. }));
}

#[test]
fn end_to_end_static_adapter_drives_resolver_through_all_three_decisions() {
    // Same skill name across the three payloads on purpose: the demo
    // §6 walk-through cycles `demo-weather` through hidden -> current
    // -> fallback without ever changing the entry point name. We
    // replay each phase through StaticLedgerAdapter -> resolver and
    // assert the resulting ActiveTarget matches what /skills/demo-weather
    // would have to be backed by.
    let adapter = StaticLedgerAdapter::new();
    let resolver = ActiveSkillResolver::new("/srv/skills");

    // Phase A: hidden (just landed; not certified yet).
    adapter.insert(
        "demo-weather",
        LedgerResolveResult::from_json_str(hidden_payload()).unwrap(),
    );
    let result = adapter
        .resolve(Path::new("/srv/skills/demo-weather"))
        .unwrap();
    let target = resolver.set_from_resolve(&result).unwrap();
    assert!(matches!(target, ActiveTarget::Hidden { .. }));
    assert!(!target.is_visible());
    assert!(target.read_dir().is_none());

    // Phase B: certified -> current.
    adapter.insert(
        "demo-weather",
        LedgerResolveResult::from_json_str(current_payload()).unwrap(),
    );
    let result = adapter
        .resolve(Path::new("/srv/skills/demo-weather"))
        .unwrap();
    let target = resolver.set_from_resolve(&result).unwrap();
    assert_eq!(
        target,
        ActiveTarget::Current {
            source_dir: PathBuf::from("/srv/skills/demo-weather"),
        }
    );
    assert_eq!(target.as_label(), "current");

    // Phase C: drift / deny -> fallback to last trusted snapshot.
    adapter.insert(
        "demo-weather",
        LedgerResolveResult::from_json_str(fallback_payload()).unwrap(),
    );
    let result = adapter
        .resolve(Path::new("/srv/skills/demo-weather"))
        .unwrap();
    let target = resolver.set_from_resolve(&result).unwrap();
    match &target {
        ActiveTarget::Snapshot {
            snapshot_dir,
            version,
        } => {
            assert_eq!(
                snapshot_dir,
                Path::new("/srv/skills/demo-weather/.skill-meta/versions/v000001.snapshot")
            );
            assert_eq!(version, "v000001");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
    assert_eq!(target.as_label(), "fallback:v000001");
    // Resolver state must reflect the final phase only.
    assert_eq!(resolver.len(), 1);
    let stored = resolver.get("demo-weather").unwrap();
    assert_eq!(stored, target);
}

#[test]
fn ledger_adapter_is_object_safe_via_arc_dyn() {
    // The hook handler the D1.x package will land must be able to keep
    // a `LedgerAdapter` behind an `Arc<dyn LedgerAdapter>`. Construct
    // the trait object explicitly so a future refactor that drops
    // `Send + Sync` from the trait fails this test instead of failing
    // at the hook handler call site months later.
    let static_adapter = StaticLedgerAdapter::new();
    static_adapter.insert(
        "demo-weather",
        LedgerResolveResult::from_json_str(current_payload()).unwrap(),
    );
    let dyn_adapter: Arc<dyn LedgerAdapter> = Arc::new(static_adapter);
    let r = dyn_adapter
        .resolve(Path::new("/srv/skills/demo-weather"))
        .unwrap();
    assert_eq!(r.decision, LedgerDecision::Current);
}

#[test]
fn decision_command_accepts_single_binary_and_split_prefix() {
    let single =
        DecisionCommand::parse("/usr/local/bin/xxx-cli").expect("single binary command parses");
    assert_eq!(single.program(), Path::new("/usr/local/bin/xxx-cli"));
    assert!(single.fixed_args().is_empty());

    let prefix = DecisionCommand::parse("agent-sec-cli skill-ledger")
        .expect("whitespace-split prefix parses");
    assert_eq!(prefix.program(), Path::new("agent-sec-cli"));
    assert_eq!(prefix.fixed_args(), &["skill-ledger".to_string()]);
}

#[test]
fn decision_command_rejects_empty_or_whitespace() {
    for raw in ["", " ", "\t", "  \n\t  "] {
        let err = DecisionCommand::parse(raw).unwrap_err();
        assert!(
            matches!(
                err,
                LedgerError::InvalidField {
                    field: "decision-command",
                    ..
                }
            ),
            "expected InvalidField(decision-command) for {raw:?}, got {err:?}"
        );
    }
}

#[test]
fn decision_command_appends_scan_and_resolve_argv() {
    // Pin the exact argv shape SkillFS will spawn for each subcommand.
    let prefix = DecisionCommand::parse("agent-sec-cli skill-ledger").unwrap();
    let skill_dir = Path::new("/srv/skills/demo-weather");
    assert_eq!(
        prefix.build_scan_args(skill_dir),
        vec![
            "skill-ledger".to_string(),
            "scan".to_string(),
            "/srv/skills/demo-weather".to_string(),
            "--json".to_string(),
        ]
    );
    assert_eq!(
        prefix.build_resolve_args(skill_dir),
        vec![
            "skill-ledger".to_string(),
            "resolve".to_string(),
            "/srv/skills/demo-weather".to_string(),
            "--json".to_string(),
        ]
    );

    let single = DecisionCommand::parse("/usr/local/bin/xxx-cli").unwrap();
    assert_eq!(
        single.build_scan_args(skill_dir),
        vec![
            "scan".to_string(),
            "/srv/skills/demo-weather".to_string(),
            "--json".to_string(),
        ]
    );
    assert_eq!(
        single.build_resolve_args(skill_dir),
        vec![
            "resolve".to_string(),
            "/srv/skills/demo-weather".to_string(),
            "--json".to_string(),
        ]
    );
}

#[test]
fn parses_declared_name_through_reexports_and_treats_as_metadata() {
    // N1/D1.6: providers may surface the on-disk SKILL.md `name:` as
    // `declaredName`. SkillFS keeps it as metadata only; the active
    // mapping is keyed off `skillName`.
    let json = r#"{
        "schemaVersion": 1,
        "skillName": "weather",
        "declaredName": "calculator",
        "status": "deny",
        "decision": "hidden",
        "reason": "frontmatter name disagrees with directory"
    }"#;
    let r = LedgerResolveResult::from_json_str(json).unwrap();
    assert_eq!(r.skill_name, "weather");
    assert_eq!(r.declared_name.as_deref(), Some("calculator"));
    assert_eq!(r.decision, LedgerDecision::Hidden);
    assert_eq!(r.status, LedgerStatus::Deny);

    // Pre-N1 payloads stay valid.
    let no_declared = LedgerResolveResult::from_json_str(current_payload()).unwrap();
    assert!(no_declared.declared_name.is_none());
}

#[test]
fn validate_for_expected_skill_enforces_canonical_identity() {
    let r = LedgerResolveResult::from_json_str(current_payload()).unwrap();
    r.validate_for_expected_skill("demo-weather")
        .expect("matching skillName must validate");

    let mismatch = LedgerResolveResult::from_json_str(current_payload()).unwrap();
    let err = mismatch
        .validate_for_expected_skill("calculator")
        .unwrap_err();
    match err {
        LedgerError::SkillNameMismatch { expected, actual } => {
            assert_eq!(expected, "calculator");
            assert_eq!(actual, "demo-weather");
            // Display must surface both names so the operator can read it.
            let rendered = LedgerError::SkillNameMismatch {
                expected: expected.clone(),
                actual: actual.clone(),
            }
            .to_string();
            assert!(rendered.contains("calculator"));
            assert!(rendered.contains("demo-weather"));
        }
        other => panic!("expected SkillNameMismatch, got {other:?}"),
    }
}

#[test]
fn set_from_resolve_for_expected_is_the_checked_runtime_api() {
    // N1/D1.6: production runtime callers must consume resolve results
    // through the checked API so a future embedder cannot accidentally
    // bypass the canonical-identity contract.
    let r = ActiveSkillResolver::new("/srv/skills");

    // Matching skillName installs cleanly.
    let result = LedgerResolveResult::from_json_str(current_payload()).unwrap();
    let target = r
        .set_from_resolve_for_expected("demo-weather", &result)
        .expect("matching skillName must install");
    assert!(matches!(target, ActiveTarget::Current { .. }));

    // Mismatched skillName is rejected and does not mutate state.
    let bogus = LedgerResolveResult::from_json_str(current_payload()).unwrap(); // skillName=demo-weather
    let err = r
        .set_from_resolve_for_expected("calculator", &bogus)
        .unwrap_err();
    match err {
        ActiveResolverError::SkillNameMismatch { expected, actual } => {
            assert_eq!(expected, "calculator");
            assert_eq!(actual, "demo-weather");
        }
        other => panic!("expected SkillNameMismatch, got {other:?}"),
    }

    // The original demo-weather entry survives the rejected call;
    // calculator was never aliased.
    assert!(r.get("demo-weather").is_some());
    assert!(r.get("calculator").is_none());
}

#[test]
fn flags_absent_means_resolver_is_empty_and_inactive() {
    // Operator does not pass `--decision-command` / `--ledger-demo-mode` /
    // `--demo-events`. The CLI does not construct an adapter, does not
    // install any active target, and the resolver — even if a future
    // refactor pre-constructs it — stays empty. Anyone querying it
    // sees `None` for every skill, which is exactly what the FUSE
    // callbacks must observe to keep the pre-D1.0 mount behavior.
    let resolver = ActiveSkillResolver::new("/srv/skills");
    assert!(resolver.is_empty());
    assert_eq!(resolver.len(), 0);
    assert!(resolver.get("demo-weather").is_none());
    assert!(resolver.snapshot().is_empty());

    // Even the no-op static adapter is harmless when nothing calls it.
    let adapter = StaticLedgerAdapter::new();
    let _: &dyn LedgerAdapter = &adapter;
    assert!(
        resolver.is_empty(),
        "no side-effects from adapter construction"
    );
}
