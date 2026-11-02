//! Phase 2: unit tests for the mount strategy layer.

use std::os::unix::fs::PermissionsExt;

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

#[test]
fn anolisa_metadata_dir_is_owner_only() {
    let tmp = tempdir().unwrap();
    let picked = pick_strategy(MountStrategyKind::Userland).unwrap();
    let ns = Namespace::user("alice").unwrap();
    let root = picked.strategy.ensure(&ns, tmp.path()).unwrap();

    assert_eq!(
        std::fs::metadata(root.join(".anolisa"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the .anolisa metadata tree must not be reachable by group/other"
    );
}

#[test]
fn anolisa_metadata_dir_tightens_a_pre_existing_0755_layout() {
    // A mount created by an older build keeps whatever the umask gave
    // the metadata dir (0755); re-ensuring must repair it.
    let tmp = tempdir().unwrap();
    let ns = Namespace::user("alice").unwrap();
    let root = tmp.path().join("user-alice");
    std::fs::create_dir_all(root.join(".anolisa")).unwrap();
    std::fs::set_permissions(
        root.join(".anolisa"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let picked = pick_strategy(MountStrategyKind::Userland).unwrap();
    picked.strategy.ensure(&ns, tmp.path()).unwrap();

    assert_eq!(
        std::fs::metadata(root.join(".anolisa"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "a mount from an older build must be re-tightened on start"
    );
}

#[test]
fn manifest_is_created_owner_only() {
    let tmp = tempdir().unwrap();
    let picked = pick_strategy(MountStrategyKind::Userland).unwrap();
    let ns = Namespace::user("alice").unwrap();
    let root = picked.strategy.ensure(&ns, tmp.path()).unwrap();

    assert_eq!(
        std::fs::metadata(root.join(".anolisa").join("manifest.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "manifest.toml carries namespace metadata and must be owner-only"
    );
}

#[test]
fn concurrent_first_starts_of_one_namespace_all_succeed() {
    // Regression for the manifest create race: `if !manifest.exists()`
    // followed by `create_new(true)` is check-then-create across two
    // lookups, so N concurrent first starts of the same fresh namespace
    // used to convert the loser's EEXIST into a hard startup failure
    // (probe-measured ~47% failure at just 2 concurrent starts). The
    // AlreadyExists arm must accept the legitimate winner via an
    // O_NOFOLLOW re-open — never via fs::write or blind acceptance,
    // which would re-open the planted-symlink hole create_new closes.
    use std::sync::{Arc, Barrier};
    let rounds = 20;
    let threads = 4;
    for _ in 0..rounds {
        let tmp = tempdir().unwrap();
        let ns = Namespace::user("race-probe").unwrap();
        let base = Arc::new(tmp.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::new();
        for t in 0..threads {
            let (b, base, ns) = (barrier.clone(), base.clone(), ns.clone());
            handles.push(std::thread::spawn(move || {
                b.wait();
                // PickedStrategy is not Clone (Box<dyn ...>): each racer
                // picks its own handle and races on the same fresh base.
                let picked = pick_strategy(MountStrategyKind::Userland).unwrap();
                let root = picked.strategy.ensure(&ns, base.as_path())?;
                let _ = t;
                Ok::<_, agent_memory::error::MemoryError>(root)
            }));
        }
        for h in handles {
            h.join()
                .expect("thread panicked")
                .expect("a concurrent first start must not fail on the manifest race");
        }
        // Every round ends with a real, non-symlink manifest. The userland
        // strategy roots a namespace at <base>/user-<user>/.anolisa.
        let meta = std::fs::symlink_metadata(
            tmp.path()
                .join("user-race-probe")
                .join(".anolisa")
                .join("manifest.toml"),
        )
        .unwrap();
        assert!(
            meta.file_type().is_file(),
            "manifest must be a regular file"
        );
    }
}
