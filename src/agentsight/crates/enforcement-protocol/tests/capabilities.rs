use agentsight_enforcement_protocol::{EnforcementCapabilities, HealthStatus};

fn assert_structured_file_protection_is_unsupported(capabilities: &EnforcementCapabilities) {
    assert!(!capabilities.file_entry_delete_protection);
    assert!(!capabilities.directory_tree_delete_protection);
    assert!(!capabilities.ordinary_write_protection);
    assert!(!capabilities.static_mount_topology);
    assert!(!capabilities.dynamic_mount_topology);
    assert!(!capabilities.complete_content_protection);
}

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
    assert_structured_file_protection_is_unsupported(&capabilities);
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
    assert_structured_file_protection_is_unsupported(&capabilities);
}

#[test]
fn unsupported_capabilities_reject_structured_file_protection() {
    assert_structured_file_protection_is_unsupported(&EnforcementCapabilities::unsupported());
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
