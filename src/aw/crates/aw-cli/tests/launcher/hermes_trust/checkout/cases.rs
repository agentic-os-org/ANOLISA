//! Checkout scenarios share the real-Git fixture in the parent module.

use super::check_hermes_checkout;

#[test]
fn hermes_rejects_tracked_worktree_and_index_changes() {
    check_hermes_checkout(&[
        "writable-root",
        "writable-file",
        "unstaged",
        "staged",
        "assume-unchanged",
        "skip-worktree",
        "clean",
    ]);
}

#[test]
fn hermes_version_excludes_sticky_script_imports() {
    check_hermes_checkout(&["sticky-bin", "clean"]);
}

#[test]
fn hermes_rejects_writable_git_configuration() {
    check_hermes_checkout(&["fsmonitor", "fsmonitor-include", "clean"]);
}

#[test]
fn hermes_revalidates_git_after_dotenv() {
    check_hermes_checkout(&[
        "dotenv-path",
        "dotenv-global",
        "dotenv-safe",
        "context-env-parent-safe",
        "context-env-dotenv-safe",
        "context-env-bytes-safe",
        "clean",
    ]);
}

#[test]
fn hermes_ignores_git_replacement_objects() {
    check_hermes_checkout(&["replace-objects", "clean"]);
}

#[test]
fn hermes_rejects_external_fsmonitor_before_native_imports() {
    check_hermes_checkout(&["fsmonitor-helper", "fsmonitor-boolean", "clean"]);
}

#[test]
fn hermes_detects_dirty_content_with_relaxed_stat_settings() {
    check_hermes_checkout(&["stat-dirty", "clean"]);
}

#[test]
fn hermes_inspects_checkout_without_refreshing_index() {
    check_hermes_checkout(&["stat-clean", "clean"]);
}

#[test]
fn hermes_rejects_writable_venv_site_startup() {
    check_hermes_checkout(&["site-pth", "site-finder", "site-directory", "clean"]);
}

#[test]
fn hermes_rejects_dotenv_loader_injection() {
    check_hermes_checkout(&["dotenv-preload", "dotenv-library", "dotenv-audit", "clean"]);
}

#[test]
fn hermes_rejects_external_git_filters_before_status() {
    check_hermes_checkout(&["filter-clean", "filter-process", "clean"]);
}

#[test]
fn hermes_ignores_untracked_bytecode_caches() {
    check_hermes_checkout(&["checkout-cache", "clean"]);
}

#[test]
fn hermes_rejects_sticky_site_directories() {
    check_hermes_checkout(&["site-sticky", "clean"]);
}

#[test]
fn hermes_rejects_replaceable_private_cache_parent() {
    check_hermes_checkout(&["cache-parent", "clean"]);
}

#[test]
fn hermes_checks_relative_git_paths_from_native_directories() {
    check_hermes_checkout(&["relative-dirty", "relative-safe", "clean"]);
}

#[test]
fn hermes_inspects_discovered_worktree_despite_core_worktree() {
    check_hermes_checkout(&["worktree-dirty", "worktree-clean", "clean"]);
}

#[test]
fn hermes_validates_project_dotenv_before_loading_native_code() {
    check_hermes_checkout(&["source-dotenv", "clean"]);
}

#[test]
fn hermes_checks_missing_git_include_targets_before_native_imports() {
    check_hermes_checkout(&["include-missing-unsafe", "include-missing-safe", "clean"]);
}

#[test]
fn hermes_rejects_missing_configuration_scopes() {
    check_hermes_checkout(&[
        "config-missing-local-unsafe",
        "config-missing-local-safe",
        "config-missing-global-unsafe",
        "config-missing-global-safe",
        "config-missing-home-unsafe",
        "config-missing-home-safe",
        "config-missing-xdg-unsafe",
        "config-missing-xdg-safe",
        "config-missing-system-unsafe",
        "config-missing-system-safe",
        "config-missing-system-disabled",
        "config-null",
        "config-system-query-unavailable",
        "config-missing-worktree-unsafe",
        "config-missing-worktree-safe",
        "clean",
    ]);
}

#[test]
fn hermes_rejects_sticky_checkout_import_roots() {
    check_hermes_checkout(&["checkout-sticky", "checkout-sticky-dotenv", "clean"]);
}
