use anolisa_core::{
    InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind, ObjectStatus,
    Ownership, RpmMetadata, SubscriptionScope,
};
use anolisa_platform::fs_layout::{FsLayout, InstallMode as LayoutInstallMode};
use std::path::PathBuf;

use super::super::INSTALLED_STATE_FILE;

pub(super) fn user_layout(home: PathBuf) -> FsLayout {
    FsLayout::user_with_overrides(home, None, None, None, None, None)
}

pub(super) fn write_state(layout: &FsLayout, objects: Vec<InstalledObject>) {
    let state = InstalledState {
        install_mode: match layout.mode {
            LayoutInstallMode::System => StateInstallMode::System,
            LayoutInstallMode::User => StateInstallMode::User,
        },
        prefix: layout.prefix.clone(),
        objects,
        ..InstalledState::default()
    };
    crate::test_support::write_legacy_state(&state, &layout.state_dir.join(INSTALLED_STATE_FILE))
        .expect("save state");
}

pub(super) fn component(name: &str) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: name.to_string(),
        version: "1.0.0".to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: None,
        raw_package: None,
        install_backend: Some("raw".to_string()),
        ownership: Some(Ownership::RawManaged),
        rpm_metadata: None,
        installed_at: "2026-01-01T00:00:00Z".to_string(),
        last_operation_id: None,
        managed: true,
        adopted: false,
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    }
}

pub(super) fn rpm_component(name: &str, package: &str) -> InstalledObject {
    let mut object = component(name);
    object.install_backend = Some("rpm".to_string());
    object.ownership = Some(Ownership::RpmObserved);
    object.rpm_metadata = Some(RpmMetadata {
        package_name: package.to_string(),
        evr: None,
        arch: None,
        source_repo: None,
    });
    object.managed = false;
    object.adopted = true;
    object
}

pub(super) fn quarantined_component(name: &str) -> InstalledObject {
    let mut object = component(name);
    object.install_backend = None;
    object.ownership = None;
    object.managed = false;
    object
}
