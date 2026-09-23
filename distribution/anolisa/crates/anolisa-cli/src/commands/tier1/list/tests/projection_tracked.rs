use anolisa_core::domain::LifecycleStatus;

use super::support::{
    FakeRpmQuery, adopted, pkg_info, projection_for, rpm_component_object,
    state_with_component_object,
};

#[test]
fn tracked_rpm_observed_projection_keeps_compatibility_status_available() {
    let query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };
    let state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        adopted(),
        "agentsight",
        "1.2.3-1.al8",
    ));

    let projection = projection_for("agentsight", &state, &query);

    assert_eq!(projection.local_state_label(), "tracked");
    assert_eq!(projection.ownership_label(), "adopted");
    assert_eq!(projection.action_label(), "status");
    assert_eq!(projection.status, "adopted");
}

#[test]
fn tracked_rpm_observed_projection_surfaces_rpm_drift_and_missing() {
    let state = state_with_component_object(rpm_component_object(
        "agentsight",
        LifecycleStatus::Installed,
        adopted(),
        "agentsight",
        "1.2.3-1.al8",
    ));

    let drifted = projection_for(
        "agentsight",
        &state,
        &FakeRpmQuery {
            installed: vec![(
                "agentsight".to_string(),
                pkg_info("agentsight", "2.0.0", Some("1.al8"), "x86_64"),
            )],
            command_missing: false,
            what_provides: Vec::new(),
        },
    );
    assert_eq!(drifted.local_state_label(), "drifted");
    assert_eq!(drifted.status, "adopted");

    let missing = projection_for("agentsight", &state, &FakeRpmQuery::default());
    assert_eq!(missing.local_state_label(), "missing");
    assert_eq!(missing.status, "adopted");
}

#[test]
fn tracked_rpm_problem_states_do_not_run_drift_probe() {
    let query = FakeRpmQuery::default();
    let cases = [
        (LifecycleStatus::Failed, "failed"),
        (LifecycleStatus::Partial, "degraded"),
        (LifecycleStatus::Disabled, "disabled"),
    ];

    for (status, expected_state) in cases {
        let state = state_with_component_object(rpm_component_object(
            "agentsight",
            status,
            adopted(),
            "agentsight",
            "1.2.3-1.al8",
        ));

        let projection = projection_for("agentsight", &state, &query);

        assert_eq!(projection.local_state_label(), expected_state);
    }
}
