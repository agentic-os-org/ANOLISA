use crate::commands::tier1::list::render::{availability_label, human_header};
use crate::commands::tier1::list::{ListArgs, ListPayload, Row, build_rows};

use super::support::{FakeRpmQuery, empty_state, pkg_info, sample_index, target};

#[test]
fn row_json_retains_status_and_adds_local_fields() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();
    let query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };
    let rows = build_rows(&index, &args, &state, Some(&query));
    let payload = ListPayload {
        components: rows,
        warnings: Vec::new(),
    };

    let json_str = serde_json::to_string(&payload).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json_str).expect("reparse");
    let sight = val["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["name"] == "agentsight")
        .unwrap();

    assert_eq!(sight["status"], "not_installed");
    assert_eq!(sight["backends"], serde_json::json!(["raw", "rpm"]));
    assert_eq!(sight["local_state"], "observed");
    assert_eq!(sight["ownership"], "rpm");
    assert_eq!(sight["scope"], "none");
    assert_eq!(sight["active"], false);
    assert_eq!(sight["mutable_by_current_invocation"], false);
    assert_eq!(sight["action"], "install");
    assert_eq!(
        sight["targets"],
        serde_json::json!([
            {"os": "linux", "arch": "x86_64"},
            {"os": "macos", "arch": "aarch64"}
        ])
    );
    assert_eq!(sight["target_available"], true);
    assert!(sight.get("platforms").is_none());
    assert!(sight.get("platform_available").is_none());
}

#[test]
fn human_header_contains_local_state() {
    let rows = vec![Row {
        name: "agentsight".to_string(),
        display_name: "AgentSight".to_string(),
        summary: "observability".to_string(),
        backends: vec!["rpm".to_string()],
        targets: vec![target("linux", "x86_64")],
        target_available: true,
        status: "not_installed".to_string(),
        local_state: "observed".to_string(),
        ownership: "none".to_string(),
        scope: "system".to_string(),
        active: true,
        mutable_by_current_invocation: false,
        shadowed_by: None,
        state_path: Some("/var/lib/anolisa/installed.toml".to_string()),
        action: "install".to_string(),
        rpm_package: Some("agentsight".to_string()),
        rpm_evr: Some("1.2.3-1.al8".to_string()),
        rpm_arch: Some("x86_64".to_string()),
        rpm_source_repo: Some("@System".to_string()),
    }];

    let header = human_header(&rows);
    assert!(header.contains("SCOPE"));
    assert!(header.contains("AVAILABILITY"));
    assert!(header.contains("LOCAL STATE"));
    assert!(!header.contains("BACKENDS"));
    assert!(!header.contains("OWNERSHIP"));
}

#[test]
fn human_availability_explains_the_supported_targets() {
    let mut rows = build_rows(
        &sample_index(),
        &ListArgs { installed: false },
        &empty_state(),
        None,
    );
    let sight = rows
        .iter_mut()
        .find(|row| row.name == "agentsight")
        .unwrap();
    sight.target_available = false;

    assert_eq!(
        availability_label(sight),
        "Linux/x86_64, macOS/aarch64 only"
    );
}

#[test]
fn list_clap_surface_has_no_catalog_or_tracked_args() {
    use crate::commands::Cli;
    use clap::CommandFactory;

    let help = Cli::command().render_long_help().to_string();

    assert!(!help.contains("--catalog"));
    assert!(!help.contains("--tracked"));
}
