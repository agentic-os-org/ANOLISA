use anolisa_core::domain::LifecycleStatus;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError};

use super::support::{
    FakeRpmQuery, adopted, empty_state, managed, owned_component, pkg_info, projection_for,
    projection_for_index, rpm_component_object, sample_index_with_aliases,
    state_with_component_object,
};

#[test]
fn local_projection_labels_cover_list_states() {
    let empty = empty_state();
    let absent_query = FakeRpmQuery::default();
    let observed_query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };

    let observed = projection_for("agentsight", &empty, &observed_query);
    assert_eq!(observed.local_state_label(), "observed");

    let tracked = projection_for(
        "agentsight",
        &state_with_component_object(rpm_component_object(
            "agentsight",
            LifecycleStatus::Installed,
            adopted(),
            "agentsight",
            "1.2.3-1.al8",
        )),
        &observed_query,
    );
    assert_eq!(tracked.local_state_label(), "tracked");

    let installed = projection_for(
        "tokenless",
        &state_with_component_object(owned_component("tokenless", LifecycleStatus::Installed)),
        &absent_query,
    );
    assert_eq!(installed.local_state_label(), "installed");

    let drifted = projection_for(
        "agentsight",
        &state_with_component_object(rpm_component_object(
            "agentsight",
            LifecycleStatus::Installed,
            managed(),
            "agentsight",
            "1.2.3-1.al8",
        )),
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

    let missing = projection_for(
        "agentsight",
        &state_with_component_object(rpm_component_object(
            "agentsight",
            LifecycleStatus::Installed,
            managed(),
            "agentsight",
            "1.2.3-1.al8",
        )),
        &absent_query,
    );
    assert_eq!(missing.local_state_label(), "missing");

    let failed = projection_for(
        "tokenless",
        &state_with_component_object(owned_component("tokenless", LifecycleStatus::Failed)),
        &absent_query,
    );
    assert_eq!(failed.local_state_label(), "failed");

    let degraded = projection_for(
        "tokenless",
        &state_with_component_object(owned_component("tokenless", LifecycleStatus::Partial)),
        &absent_query,
    );
    assert_eq!(degraded.local_state_label(), "degraded");

    let disabled = projection_for(
        "tokenless",
        &state_with_component_object(owned_component("tokenless", LifecycleStatus::Disabled)),
        &absent_query,
    );
    assert_eq!(disabled.local_state_label(), "disabled");

    let not_installed = projection_for("tokenless", &empty, &absent_query);
    assert_eq!(not_installed.local_state_label(), "not_installed");
}

#[test]
fn untracked_observed_rpm_projection_uses_rpm_backend_rpm_ownership_and_install_action() {
    let query = FakeRpmQuery {
        installed: vec![(
            "agentsight".to_string(),
            pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };

    let projection = projection_for("agentsight", &empty_state(), &query);

    assert_eq!(projection.local_state_label(), "observed");
    assert_eq!(projection.backend.as_deref(), Some("rpm"));
    assert_eq!(projection.ownership_label(), "rpm");
    assert_eq!(projection.action_label(), "install");
    assert_eq!(projection.rpm_package.as_deref(), Some("agentsight"));
    assert_eq!(projection.status, "not_installed");
}

#[test]
fn untracked_observed_rpm_projection_fetches_source_repo_when_installed_info_has_no_origin() {
    struct OriginQuery;

    impl PackageQuery for OriginQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            assert_eq!(package, "agentsight");
            let mut info = pkg_info("agentsight", "1.2.3", Some("1.al8"), "x86_64");
            info.origin = None;
            Ok(Some(info))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
            assert_eq!(package, "agentsight");
            Ok(Some("alinux3-plus".to_string()))
        }
    }

    let projection = projection_for("agentsight", &empty_state(), &OriginQuery);

    assert_eq!(projection.local_state_label(), "observed");
    assert_eq!(projection.rpm_source_repo.as_deref(), Some("alinux3-plus"));
}

#[test]
fn rpm_query_command_missing_keeps_state_index_projection() {
    let query = FakeRpmQuery {
        installed: Vec::new(),
        command_missing: true,
        what_provides: Vec::new(),
    };

    let projection = projection_for("agentsight", &empty_state(), &query);

    assert_eq!(projection.local_state_label(), "not_installed");
    assert_eq!(projection.backend, None);
    assert_eq!(projection.ownership_label(), "none");
    assert_eq!(projection.action_label(), "install");
    assert_eq!(projection.status, "not_installed");
}

#[test]
fn alias_does_not_override_the_install_package() {
    let index = sample_index_with_aliases();
    // Backend package "copilot-shell" is not installed; alias "cosh-old" is.
    let query = FakeRpmQuery {
        installed: vec![(
            "cosh-old".to_string(),
            pkg_info("cosh-old", "1.0.0", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: Vec::new(),
    };

    let projection = projection_for_index(&index, "cosh", &empty_state(), &query);

    assert_eq!(projection.local_state_label(), "not_installed");
    assert_eq!(projection.ownership_label(), "none");
    assert_eq!(projection.action_label(), "install");
    assert_eq!(projection.rpm_package, None);
}

#[test]
fn capability_does_not_override_the_install_package() {
    let index = sample_index_with_aliases();
    // Neither backend package nor alias is installed, but a package providing
    // `anolisa-component(cosh)` exists in rpmdb.
    let query = FakeRpmQuery {
        installed: vec![(
            "cosh-legacy".to_string(),
            pkg_info("cosh-legacy", "0.9.0", Some("1.al8"), "x86_64"),
        )],
        command_missing: false,
        what_provides: vec![(
            "anolisa-component(cosh)".to_string(),
            vec!["cosh-legacy".to_string()],
        )],
    };

    let projection = projection_for_index(&index, "cosh", &empty_state(), &query);

    assert_eq!(projection.local_state_label(), "not_installed");
    assert_eq!(projection.ownership_label(), "none");
    assert_eq!(projection.action_label(), "install");
    assert_eq!(projection.rpm_package, None);
}

#[test]
fn observed_rpm_provides_with_ambiguous_providers_is_not_observed() {
    let index = sample_index_with_aliases();
    // Two packages provide the same capability — list must not pick one.
    let query = FakeRpmQuery {
        installed: vec![
            (
                "cosh-legacy".to_string(),
                pkg_info("cosh-legacy", "0.9.0", Some("1.al8"), "x86_64"),
            ),
            (
                "cosh-vendor".to_string(),
                pkg_info("cosh-vendor", "1.0.0", Some("1.al8"), "x86_64"),
            ),
        ],
        command_missing: false,
        what_provides: vec![(
            "anolisa-component(cosh)".to_string(),
            vec!["cosh-legacy".to_string(), "cosh-vendor".to_string()],
        )],
    };

    let projection = projection_for_index(&index, "cosh", &empty_state(), &query);

    assert_eq!(projection.local_state_label(), "not_installed");
    assert_eq!(projection.ownership_label(), "none");
}

#[test]
fn legacy_cosh_ng_capability_is_not_counted_as_cosh() {
    let mut index = sample_index_with_aliases();
    let mut ng = index.components[0].clone();
    ng.name = "cosh-ng".to_string();
    ng.aliases.clear();
    ng.backends.retain(|backend| backend.kind == "rpm");
    ng.backends[0].package = "cosh-ng".to_string();
    ng.backends[0].provides = Some("anolisa-component(cosh-ng)".to_string());
    index.components.push(ng);
    let query = FakeRpmQuery {
        installed: vec![(
            "cosh-ng".to_string(),
            pkg_info("cosh-ng", "0.23.0", Some("1.alnx4"), "x86_64"),
        )],
        what_provides: vec![(
            "anolisa-component(cosh)".to_string(),
            vec!["cosh-ng".to_string()],
        )],
        command_missing: false,
    };
    for state in [
        empty_state(),
        state_with_component_object(rpm_component_object(
            "cosh-ng",
            LifecycleStatus::Installed,
            managed(),
            "cosh-ng",
            "0.23.0-1.alnx4",
        )),
    ] {
        let cosh = projection_for_index(&index, "cosh", &state, &query);
        assert_eq!(cosh.local_state_label(), "not_installed");
        assert_eq!(cosh.rpm_package, None);
        let ng = projection_for_index(&index, "cosh-ng", &state, &query);
        assert_eq!(ng.rpm_package.as_deref(), Some("cosh-ng"));
    }
}

#[test]
fn ambiguous_install_packages_are_not_arbitrarily_observed() {
    let mut index = sample_index_with_aliases();
    let mut backend = index.components[0].backends[1].clone();
    backend.package = "cosh-vendor".to_string();
    index.components[0].backends.push(backend);
    let query = FakeRpmQuery {
        installed: vec![(
            "copilot-shell".to_string(),
            pkg_info("copilot-shell", "2.8.0", None, "x86_64"),
        )],
        ..Default::default()
    };
    let projection = projection_for_index(&index, "cosh", &empty_state(), &query);
    assert_eq!(projection.rpm_package, None);
}

#[test]
fn raw_only_list_entry_does_not_query_rpm_repositories() {
    struct NoRpmQuery;
    impl PackageQuery for NoRpmQuery {
        fn query_installed(&self, _: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            panic!("raw-only entries have no RPM observation target");
        }
        fn query_available(&self, _: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            panic!("list must not query available packages");
        }
        fn what_provides_installed(&self, _: &str) -> Result<Vec<String>, PackageQueryError> {
            panic!("raw-only entries must not enter capability resolution");
        }
    }
    let projection = projection_for("tokenless", &empty_state(), &NoRpmQuery);
    assert_eq!(projection.local_state_label(), "not_installed");
}
