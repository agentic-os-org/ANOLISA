use agentsight_enforcer::{EnforcementBackend, MockBackend};

#[test]
fn mock_backend_reports_explicit_test_capabilities() {
    let health = MockBackend::new()
        .health()
        .expect("mock health should succeed");

    assert!(health.capabilities.credential_observe);
    assert!(health.capabilities.credential_audit);
    assert!(health.capabilities.credential_enforce);
    assert!(health.capabilities.policy_handoff);
    assert!(health.capabilities.alternate_pid_retarget);
    assert!(health.capabilities.test_development);
}
