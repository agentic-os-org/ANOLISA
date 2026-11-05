use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn real_capacity_failure_preserves_required_barriers_and_best_effort_degradation() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let mut recorder = CoreAuditRecorder::test_real("budget-session", &root);
    recorder.settings.mode = AuditMode::Required;
    recorder.settings.max_disk_bytes = 1;
    recorder.sink = CoreAuditSink::Real(
        AuditSegmentWriter::create_with_settings(
            &root,
            AuditComponentName::CoshCore,
            recorder.settings.clone(),
        )
        .unwrap(),
    );
    let scope = CoreAuditScope::request("run", Some("turn"), "request", None);
    let provider = cosh_types::audit::AuditProviderData {
        provider: "deterministic".into(),
        ..Default::default()
    };
    let error = recorder
        .record_provider_started(scope, "deterministic", &provider)
        .unwrap_err();
    assert!(error.contains("AUDIT_REQUIRED_UNAVAILABLE"));
    assert!(recorder.degraded && recorder.warned);
    recorder.settings.mode = AuditMode::BestEffort;
    recorder
        .record_provider_started(scope, "deterministic", &provider)
        .unwrap();
    assert!(recorder.degraded && recorder.warned);
    recorder.settings.max_disk_bytes = 8192;
    recorder.sink = CoreAuditSink::Real(
        AuditSegmentWriter::create_with_settings(
            &root,
            AuditComponentName::CoshCore,
            recorder.settings.clone(),
        )
        .unwrap(),
    );
    recorder
        .record_provider_started(scope, "deterministic", &provider)
        .unwrap();
    assert!(!recorder.degraded);
    assert!(!recorder.warned);
}
