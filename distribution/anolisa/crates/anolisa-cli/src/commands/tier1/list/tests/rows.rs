use anolisa_core::domain::LifecycleStatus;

use crate::commands::tier1::list::{ListArgs, build_rows};

use super::support::{
    FakeRpmQuery, adopted, delegated_component, empty_state, managed, owned_component, pkg_info,
    rpm_component_object, sample_index, state_with_component_object,
};

#[test]
fn rows_use_local_projection_for_untracked_observed_rpm() {
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

    let sight = rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.status, "not_installed");
    assert_eq!(sight.local_state, "observed");
    assert_eq!(sight.ownership, "rpm");
    assert_eq!(sight.action, "install");
}

#[test]
fn rows_without_rpm_query_do_not_surface_observed_system_rpms() {
    let index = sample_index();
    let state = empty_state();

    let all_rows = build_rows(&index, &ListArgs { installed: false }, &state, None);
    let sight = all_rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.local_state, "not_installed");
    assert_eq!(sight.ownership, "none");
    assert_eq!(sight.rpm_package, None);

    let installed_rows = build_rows(&index, &ListArgs { installed: true }, &state, None);
    assert!(installed_rows.is_empty());
}

#[test]
fn rows_ignore_rpm_query_failures() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = empty_state();
    let query = FakeRpmQuery {
        installed: Vec::new(),
        command_missing: true,
        what_provides: Vec::new(),
    };

    let rows = build_rows(&index, &args, &state, Some(&query));

    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.local_state == "not_installed"));
}

#[test]
fn rows_use_status_action_for_tracked_rpm_observed_state() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        adopted(),
        "agentsight",
        "1.2.3-1.al8",
    ));
    let query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };

    let rows = build_rows(&index, &args, &state, Some(&query));

    let sight = rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.status, "adopted");
    assert_eq!(sight.local_state, "tracked");
    assert_eq!(sight.ownership, "adopted");
    assert_eq!(sight.action, "status");
}

#[test]
fn rows_project_raw_and_rpm_managed_state_as_installed() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };

    let raw_state =
        state_with_component_object(owned_component("tokenless", LifecycleStatus::Installed));
    let raw_rows = build_rows(&index, &args, &raw_state, Some(&query));
    let token = raw_rows.iter().find(|r| r.name == "tokenless").unwrap();
    assert_eq!(token.local_state, "installed");
    assert_eq!(token.ownership, "owned");

    let rpm_state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        managed(),
        "agentsight",
        "1.2.3-1.al8",
    ));
    let rpm_rows = build_rows(&index, &args, &rpm_state, Some(&query));
    let sight = rpm_rows.iter().find(|r| r.name == "agentsight").unwrap();
    assert_eq!(sight.local_state, "installed");
    assert_eq!(sight.ownership, "managed");
}

#[test]
fn rows_surface_rpm_drift_and_missing() {
    let index = sample_index();
    let args = ListArgs { installed: false };
    let state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        managed(),
        "agentsight",
        "1.2.3-1.al8",
    ));

    let drifted_rows = build_rows(
        &index,
        &args,
        &state,
        Some(&FakeRpmQuery {
            installed: vec![(
                "agentsight".to_string(),
                pkg_info("agentsight", "2.0.0", Some("1.al8"), "x86_64"),
            )],
            command_missing: false,
            what_provides: Vec::new(),
        }),
    );
    let drifted = drifted_rows
        .iter()
        .find(|r| r.name == "agentsight")
        .unwrap();
    assert_eq!(drifted.local_state, "drifted");

    let missing_rows = build_rows(&index, &args, &state, Some(&FakeRpmQuery::default()));
    let missing = missing_rows
        .iter()
        .find(|r| r.name == "agentsight")
        .unwrap();
    assert_eq!(missing.local_state, "missing");
}

#[test]
fn installed_filter_keeps_only_currently_installed_local_states() {
    let index = sample_index();
    let args = ListArgs { installed: true };

    let observed_rows = build_rows(
        &index,
        &args,
        &empty_state(),
        Some(&FakeRpmQuery {
            installed: vec![(
                "agentsight".to_string(),
                pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
            )],
            command_missing: false,
            what_provides: Vec::new(),
        }),
    );
    assert_eq!(
        observed_rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        vec!["agentsight"]
    );

    let included_cases = [
        owned_component("tokenless", LifecycleStatus::Installed),
        delegated_component("tokenless", LifecycleStatus::Installed, adopted()),
        owned_component("tokenless", LifecycleStatus::Partial),
    ];
    let expected_states = ["installed", "tracked", "degraded"];
    for (object, expected_state) in included_cases.into_iter().zip(expected_states) {
        let state = state_with_component_object(object);
        let rows = build_rows(&index, &args, &state, Some(&FakeRpmQuery::default()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].local_state, expected_state);
    }

    for status in [LifecycleStatus::Failed, LifecycleStatus::Disabled] {
        let state = state_with_component_object(owned_component("tokenless", status));
        let rows = build_rows(&index, &args, &state, Some(&FakeRpmQuery::default()));
        assert!(rows.is_empty());
    }

    let rpm_state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        managed(),
        "agentsight",
        "1.2.3-1.al8",
    ));
    let drifted_rows = build_rows(
        &index,
        &args,
        &rpm_state,
        Some(&FakeRpmQuery {
            installed: vec![(
                "agentsight".to_string(),
                pkg_info("agentsight", "2.0.0", Some("1.al8"), "x86_64"),
            )],
            command_missing: false,
            what_provides: Vec::new(),
        }),
    );
    assert!(drifted_rows.is_empty());

    let missing_rows = build_rows(&index, &args, &rpm_state, Some(&FakeRpmQuery::default()));
    assert!(missing_rows.is_empty());

    let empty_rows = build_rows(
        &index,
        &args,
        &empty_state(),
        Some(&FakeRpmQuery::default()),
    );
    assert!(empty_rows.is_empty());
}
