use agentsight_enforcement_protocol::{EnforcementCapabilities, HealthStatus};

#[test]
fn actplane_capabilities_are_observe_and_audit_only() {
    let capabilities = EnforcementCapabilities::actplane();

    assert!(capabilities.credential_observe);
    assert!(capabilities.credential_audit);
    assert!(!capabilities.credential_enforce);
    assert!(!capabilities.policy_handoff);
    assert!(!capabilities.alternate_pid_retarget);
    assert!(!capabilities.test_development);
    // Static default is false; ActPlaneBackend overrides it at runtime from the
    // actually loaded profile (agent-file-guard enables it).
    assert!(!capabilities.file_delete_guard);
}

#[test]
fn mock_capabilities_are_explicitly_test_only() {
    let capabilities = EnforcementCapabilities::mock_development();

    assert!(capabilities.credential_observe);
    assert!(capabilities.credential_audit);
    assert!(capabilities.credential_enforce);
    assert!(capabilities.policy_handoff);
    assert!(capabilities.alternate_pid_retarget);
    assert!(capabilities.test_development);
    assert!(capabilities.file_delete_guard);
}

#[test]
fn missing_wire_capabilities_fail_closed() {
    let health: HealthStatus = serde_json::from_value(serde_json::json!({
        "ready": true,
        "backend": "legacy",
        "message": null
    }))
    .expect("legacy health should remain decodable");

    assert_eq!(health.capabilities, EnforcementCapabilities::unsupported());
}
