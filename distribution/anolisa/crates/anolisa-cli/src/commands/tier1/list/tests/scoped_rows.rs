use std::path::PathBuf;

use anolisa_core::domain::LifecycleStatus;
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;

use crate::commands::state_view::{ScopedStateRoot, StateScope, StateView};
use crate::commands::tier1::list::{ListArgs, build_rows_from_view};

use super::support::{
    delegated_component, managed, owned_component, sample_index, state_with_component_object,
};

#[test]
fn user_view_installed_filter_keeps_system_provided_component() {
    let index = sample_index();
    let args = ListArgs { installed: true };
    let view = user_plus_system_view(
        StateStore::empty(),
        state_with_component_object(owned_component("agentsight", LifecycleStatus::Installed)),
    );

    let rows = build_rows_from_view(&index, &args, &view, None, "linux", "x86_64");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "agentsight");
    assert_eq!(rows[0].local_state, "installed");
    assert_eq!(rows[0].scope, "system");
    assert_eq!(
        rows[0].state_path.as_deref(),
        Some("/tmp/anolisa-system-state/installed.toml")
    );
    assert!(rows[0].active);
    assert!(!rows[0].mutable_by_current_invocation);
}

#[test]
fn user_record_shadows_system_record_in_list_rows() {
    let index = sample_index();
    let args = ListArgs { installed: true };
    let view = user_plus_system_view(
        state_with_component_object(owned_component("agentsight", LifecycleStatus::Installed)),
        state_with_component_object(delegated_component(
            "agentsight",
            LifecycleStatus::Installed,
            managed(),
        )),
    );

    let rows = build_rows_from_view(&index, &args, &view, None, "linux", "x86_64");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "agentsight");
    assert_eq!(rows[0].scope, "user");
    assert!(rows[0].active);
    assert!(rows[0].mutable_by_current_invocation);
    assert_eq!(rows[1].name, "agentsight");
    assert_eq!(rows[1].scope, "system");
    assert!(!rows[1].active);
    assert!(!rows[1].mutable_by_current_invocation);
    assert_eq!(rows[1].shadowed_by.as_deref(), Some("user"));
}

fn user_plus_system_view(user_state: StateStore, system_state: StateStore) -> StateView {
    let user_root = ScopedStateRoot {
        scope: StateScope::User,
        layout: FsLayout::user_with_overrides(
            PathBuf::from("/tmp/anolisa-home"),
            None,
            None,
            Some(PathBuf::from("/tmp/anolisa-user-state")),
            None,
            None,
        ),
        state_path: PathBuf::from("/tmp/anolisa-user-state/installed.toml"),
        writable: true,
        state: user_state,
    };
    let system_root = ScopedStateRoot {
        scope: StateScope::System,
        layout: FsLayout::system(Some(PathBuf::from("/tmp/anolisa-system"))),
        state_path: PathBuf::from("/tmp/anolisa-system-state/installed.toml"),
        writable: false,
        state: system_state,
    };
    StateView {
        writable: user_root.clone(),
        visible_roots: vec![user_root, system_root],
        unavailable_roots: Vec::new(),
        warnings: Vec::new(),
    }
}
