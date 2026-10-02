use anolisa_core::domain::LifecycleStatus;
use anolisa_core::state::ObjectKind;

use crate::commands::tier1::list::{ListArgs, ListPayload, build_rows, build_rows_for_target};
use crate::resolution::{ComponentBackendEntry, ComponentIndex, ComponentIndexEntry};

use super::support::{
    empty_state, sample_index, state_with_adopted_object, state_with_owned_object, target,
};

#[test]
fn index_builds_rows() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    assert_eq!(rows.len(), 2);

    let sight = &rows[0];
    assert_eq!(sight.name, "agentsight");
    assert_eq!(sight.display_name, "AgentSight");
    assert_eq!(sight.summary, "eBPF-based AI agent observability tool");
    assert_eq!(sight.backends, vec!["raw", "rpm"]);
    assert_eq!(
        sight.targets,
        vec![target("linux", "x86_64"), target("macos", "aarch64")]
    );
    assert!(sight.target_available);
    assert_eq!(sight.status, "not_installed");

    let token = &rows[1];
    assert_eq!(token.name, "tokenless");
    assert_eq!(token.backends, vec!["raw"]);
    assert_eq!(
        token.targets,
        vec![
            target("linux", "x86_64"),
            target("linux", "aarch64"),
            target("macos", "aarch64"),
        ]
    );
    assert!(token.target_available);
}

#[test]
fn target_matrix_distinguishes_os_and_architecture() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();

    let rows = build_rows_for_target(&index, &args, &state, None, "macos", "aarch64");

    assert_eq!(rows.len(), 2);
    let sight = rows.iter().find(|row| row.name == "agentsight").unwrap();
    assert!(sight.target_available);
    assert_eq!(sight.action, "install");
    let token = rows.iter().find(|row| row.name == "tokenless").unwrap();
    assert!(token.target_available);
    assert_eq!(token.action, "install");

    let rows = build_rows_for_target(&index, &args, &state, None, "macos", "x86_64");
    assert!(rows.iter().all(|row| !row.target_available));
    assert!(rows.iter().all(|row| row.action == "unavailable"));

    let rows = build_rows_for_target(&index, &args, &state, None, "linux", "aarch64");
    let sight = rows.iter().find(|row| row.name == "agentsight").unwrap();
    assert!(!sight.target_available);
    assert_eq!(sight.action, "unavailable");
    let token = rows.iter().find(|row| row.name == "tokenless").unwrap();
    assert!(token.target_available);
    assert_eq!(token.action, "install");
}

#[test]
fn empty_state_all_not_installed() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    for row in &rows {
        assert_eq!(row.status, "not_installed");
        assert_eq!(row.local_state, "not_installed");
    }
}

#[test]
fn installed_component_shows_installed() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_owned_object(
        ObjectKind::Component,
        "tokenless",
        LifecycleStatus::Installed,
    );
    let rows = build_rows(&index, &args, &state, None);

    let sight = rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.status, "not_installed");

    let token = rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.status, "installed");
}

#[test]
fn adopted_rpm_component_shows_adopted() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_adopted_object(ObjectKind::Component, "agentsight");
    let rows = build_rows(&index, &args, &state, None);

    let sight = rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.status, "adopted");
}

#[test]
fn compatibility_status_labels_are_preserved_before_local_projection() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_adopted_object(ObjectKind::Component, "agentsight");

    let rows = build_rows(&index, &args, &state, None);

    let sight = rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.status, "adopted");
    let token = rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.status, "not_installed");
}

#[test]
fn adapter_object_does_not_mark_component_installed() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state =
        state_with_owned_object(ObjectKind::Adapter, "tokenless", LifecycleStatus::Installed);
    let rows = build_rows(&index, &args, &state, None);
    let token = rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.status, "not_installed");
}

#[test]
fn failed_component_shows_failed() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state =
        state_with_owned_object(ObjectKind::Component, "tokenless", LifecycleStatus::Failed);
    let rows = build_rows(&index, &args, &state, None);
    let token = rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.status, "failed");
}

#[test]
fn disabled_component_shows_disabled() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_owned_object(
        ObjectKind::Component,
        "tokenless",
        LifecycleStatus::Disabled,
    );
    let rows = build_rows(&index, &args, &state, None);
    let token = rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.status, "disabled");
}

#[test]
fn installed_filter_returns_only_installed() {
    let index = sample_index();
    let args = ListArgs { installed: true };
    let state = state_with_owned_object(
        ObjectKind::Component,
        "tokenless",
        LifecycleStatus::Installed,
    );
    let rows = build_rows(&index, &args, &state, None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "tokenless");
    assert_eq!(rows[0].status, "installed");
}

#[test]
fn installed_filter_includes_adopted_rpm_components() {
    let index = sample_index();
    let args = ListArgs { installed: true };
    let state = state_with_adopted_object(ObjectKind::Component, "agentsight");
    let rows = build_rows(&index, &args, &state, None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "agentsight");
    assert_eq!(rows[0].status, "adopted");
}

#[test]
fn installed_filter_with_empty_state_returns_empty() {
    let index = sample_index();
    let args = ListArgs { installed: true };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    assert!(rows.is_empty());
}

#[test]
fn json_payload_uses_components_key() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    let payload = ListPayload {
        components: rows,
        warnings: Vec::new(),
    };
    let json_str = serde_json::to_string(&payload).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("reparse");
    assert!(val.get("components").is_some());
}

#[test]
fn json_payload_status_reflects_install_state() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_owned_object(
        ObjectKind::Component,
        "agentsight",
        LifecycleStatus::Installed,
    );
    let rows = build_rows(&index, &args, &state, None);
    let payload = ListPayload {
        components: rows,
        warnings: Vec::new(),
    };
    let json_str = serde_json::to_string(&payload).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("reparse");
    let components = val["components"].as_array().unwrap();
    let sight = components
        .iter()
        .find(|c| c["name"] == "agentsight")
        .unwrap();
    assert_eq!(sight["status"], "installed");
    let token = components
        .iter()
        .find(|c| c["name"] == "tokenless")
        .unwrap();
    assert_eq!(token["status"], "not_installed");
}

#[test]
fn missing_optional_fields_use_defaults() {
    let index = ComponentIndex {
        schema_version: 2,
        generated_at: None,
        publisher: None,
        components: vec![ComponentIndexEntry {
            name: "minimal".to_string(),
            display_name: None,
            summary: None,
            targets: vec![target("linux", "x86_64")],
            backends: Vec::new(),
            aliases: Vec::new(),
        }],
    };
    let args = ListArgs { installed: false };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, "minimal");
    assert_eq!(row.display_name, "minimal");
    assert!(row.summary.is_empty());
    assert!(row.backends.is_empty());
    assert_eq!(row.targets, [target("linux", "x86_64")]);
    assert!(row.target_available);
    assert_eq!(row.status, "not_installed");
    assert_eq!(row.action, "unavailable");
}

#[test]
fn unknown_backend_kind_preserved() {
    let index = ComponentIndex {
        schema_version: 2,
        generated_at: None,
        publisher: None,
        components: vec![ComponentIndexEntry {
            name: "test".to_string(),
            display_name: None,
            summary: None,
            targets: vec![target("linux", "x86_64")],
            backends: vec![ComponentBackendEntry {
                kind: "custom-repo".to_string(),
                package: "test".to_string(),
                provides: None,
                legacy_adopt: false,
            }],
            aliases: Vec::new(),
        }],
    };
    let args = ListArgs { installed: false };
    let state = empty_state();
    let rows = build_rows(&index, &args, &state, None);
    assert_eq!(rows[0].backends, vec!["custom-repo"]);
}
