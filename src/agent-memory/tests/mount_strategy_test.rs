//! Phase 2: unit tests for the mount strategy layer.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::mount::{MountStrategyKind, pick_strategy};
use agent_memory::ns::Namespace;

#[test]
fn default_strategy_is_auto() {
    let cfg = AppConfig::default();
    assert_eq!(cfg.memory.mount.strategy, MountStrategyKind::Auto);
}

#[test]
fn from_str_loose_accepts_aliases() {
    assert_eq!(
        MountStrategyKind::from_str_loose("auto"),
        Some(MountStrategyKind::Auto)
    );
    assert_eq!(
        MountStrategyKind::from_str_loose("USERLAND"),
        Some(MountStrategyKind::Userland)
    );
    assert_eq!(
        MountStrategyKind::from_str_loose("userns"),
        Some(MountStrategyKind::Userns)
    );
    assert_eq!(
        MountStrategyKind::from_str_loose("user-ns"),
        Some(MountStrategyKind::Userns)
    );
    assert_eq!(MountStrategyKind::from_str_loose("garbage"), None);
}

#[test]
fn userland_strategy_resolves_under_base_dir() {
    let tmp = tempdir().unwrap();
    let picked = pick_strategy(MountStrategyKind::Userland).unwrap();
    assert_eq!(picked.strategy.name(), "userland");
    assert!(!picked.entered_userns);

    let ns = Namespace::user("alice").unwrap();
    let root = picked.strategy.ensure(&ns, tmp.path()).unwrap();

    assert_eq!(root, tmp.path().join("user-alice"));
    assert!(root.exists());
    assert!(root.join("README.md").exists());
    assert!(root.join(".anolisa").join("manifest.toml").exists());
}
