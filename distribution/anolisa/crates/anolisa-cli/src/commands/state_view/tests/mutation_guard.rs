use anolisa_platform::fs_layout::FsLayout;
use tempfile::tempdir;

use super::super::{RootSpec, StateScope, StateView};
use super::support::{component, quarantined_component, rpm_component, user_layout, write_state};

#[test]
fn read_only_system_record_blocks_lifecycle_mutation() {
    let tmp = tempdir().expect("tempdir");
    let user_layout = user_layout(tmp.path().join("home"));
    let system_layout = FsLayout::system(Some(tmp.path().join("system")));
    write_state(&user_layout, Vec::new());
    write_state(&system_layout, vec![component("system-tool")]);

    let view = StateView::from_layouts(
        "test",
        vec![
            (
                user_layout,
                RootSpec {
                    scope: StateScope::User,
                    writable: true,
                },
            ),
            (
                system_layout,
                RootSpec {
                    scope: StateScope::System,
                    writable: false,
                },
            ),
        ],
    )
    .expect("state view");

    let err = view
        .reject_non_writable_component_mutation(
            "uninstall --remove-system-package system-tool",
            "system-tool",
        )
        .expect_err("system-only component must be read-only in user mode");

    match err {
        crate::response::CliError::PermissionDenied { reason, hint, .. } => {
            assert!(reason.contains("system-tool"), "reason: {reason}");
            assert!(reason.contains("system"), "reason: {reason}");
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("--install-mode system")),
                "hint: {hint:?}"
            );
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("uninstall --remove-system-package system-tool")),
                "hint must preserve the original uninstall intent: {hint:?}"
            );
        }
        other => panic!("expected permission error, got {other:?}"),
    }
}

#[test]
fn read_only_system_record_blocks_lifecycle_mutation_by_rpm_package_name() {
    let tmp = tempdir().expect("tempdir");
    let user_layout = user_layout(tmp.path().join("home"));
    let system_layout = FsLayout::system(Some(tmp.path().join("system")));
    write_state(&user_layout, Vec::new());
    write_state(&system_layout, vec![rpm_component("cosh", "copilot-shell")]);

    let view = StateView::from_layouts(
        "test",
        vec![
            (
                user_layout,
                RootSpec {
                    scope: StateScope::User,
                    writable: true,
                },
            ),
            (
                system_layout,
                RootSpec {
                    scope: StateScope::System,
                    writable: false,
                },
            ),
        ],
    )
    .expect("state view");

    let err = view
        .reject_non_writable_component_mutation("uninstall copilot-shell", "copilot-shell")
        .expect_err("system RPM package alias must be read-only in user mode");

    match err {
        crate::response::CliError::PermissionDenied { reason, hint, .. } => {
            assert!(reason.contains("copilot-shell"), "reason: {reason}");
            assert!(reason.contains("system"), "reason: {reason}");
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("--install-mode system")),
                "hint: {hint:?}"
            );
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("uninstall copilot-shell")),
                "hint must preserve the original alias input: {hint:?}"
            );
        }
        other => panic!("expected permission error, got {other:?}"),
    }
}

#[test]
fn read_only_system_record_blocks_lifecycle_mutation_by_raw_package_name() {
    let tmp = tempdir().expect("tempdir");
    let user_layout = user_layout(tmp.path().join("home"));
    let system_layout = FsLayout::system(Some(tmp.path().join("system")));
    let mut object = component("foo");
    object.raw_package = Some("altpkg".to_string());
    write_state(&user_layout, Vec::new());
    write_state(&system_layout, vec![object]);

    let view = StateView::from_layouts(
        "test",
        vec![
            (
                user_layout,
                RootSpec {
                    scope: StateScope::User,
                    writable: true,
                },
            ),
            (
                system_layout,
                RootSpec {
                    scope: StateScope::System,
                    writable: false,
                },
            ),
        ],
    )
    .expect("state view");

    let err = view
        .reject_non_writable_component_mutation("update altpkg", "altpkg")
        .expect_err("system raw package alias must be read-only in user mode");

    match err {
        crate::response::CliError::PermissionDenied { reason, hint, .. } => {
            assert!(reason.contains("altpkg"), "reason: {reason}");
            assert!(reason.contains("system"), "reason: {reason}");
            assert!(
                hint.as_deref()
                    .is_some_and(|h| h.contains("--install-mode system")),
                "hint: {hint:?}"
            );
            assert!(
                hint.as_deref().is_some_and(|h| h.contains("update altpkg")),
                "hint must preserve the original alias input: {hint:?}"
            );
        }
        other => panic!("expected permission error, got {other:?}"),
    }
}

#[test]
fn read_only_system_quarantine_blocks_exact_name_mutation() {
    let tmp = tempdir().expect("tempdir");
    let user_layout = user_layout(tmp.path().join("home"));
    let system_layout = FsLayout::system(Some(tmp.path().join("system")));
    write_state(&user_layout, Vec::new());
    write_state(&system_layout, vec![quarantined_component("legacy-name")]);

    let view = StateView::from_layouts(
        "test",
        vec![
            (
                user_layout,
                RootSpec {
                    scope: StateScope::User,
                    writable: true,
                },
            ),
            (
                system_layout,
                RootSpec {
                    scope: StateScope::System,
                    writable: false,
                },
            ),
        ],
    )
    .expect("state view");

    assert!(view.has_exact_component("legacy-name"));
    let err = view
        .reject_non_writable_component_mutation("repair legacy-name", "legacy-name")
        .expect_err("system quarantine must remain addressable but read-only");
    assert_eq!(err.code(), "PERMISSION_DENIED");
    assert!(err.reason().contains("system-scope"));
}
