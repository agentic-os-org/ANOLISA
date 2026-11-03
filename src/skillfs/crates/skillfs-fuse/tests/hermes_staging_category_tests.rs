//! Integration tests: Exact staging patterns cover the Hermes category
//! component of a nested skill id.
//!
//! The staging contract (I2) suppresses intermediate mutations inside
//! staging roots — no notify, no refresh, no quiet-timeout — and bypasses
//! the activation hidden gate so installers keep exact-path access while
//! a ledger-driven resolver is attached. The existing staging suites pin
//! both properties with `PrefixStar` patterns in the flat layout. For a
//! Hermes nested id (`category/skill`) the shared helpers check the full
//! id and the leaf: a prefix pattern matches the full id through its
//! category prefix, but an Exact pattern (`.pip-staging` is the second
//! documented example in the `[install]` config docs) names the category
//! exactly and matches neither the full id nor the leaf — so a staging
//! root used as a Hermes category escapes both the suppression and the
//! gate bypass.
//!
//! Mount readiness is polled instead of assumed after a fixed delay: the
//! background mount runs the environment-profile probe loop before the
//! FUSE session comes up, which can take seconds on a loaded host.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, InMemoryNotifyClient, NotifyController, StagingConfig, StagingMatcher,
    StagingPattern,
};
use skillfs_fuse::{
    MountConfig, MountHandle, MountOptions, SkillLayout, mount_background_configured,
};

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

/// Mounts a Hermes workspace with a `.pip-staging` Exact staging pattern.
struct ExactStagingHermesMount {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    notify_controller: Arc<NotifyController>,
    handle: Option<MountHandle>,
}

impl ExactStagingHermesMount {
    /// `with_resolver` attaches an empty [`ActiveSkillResolver`]: every
    /// skill it holds no entry for resolves Hidden, which is exactly the
    /// state a newly staged skill must stay writable through.
    fn new(
        client: Arc<InMemoryNotifyClient>,
        with_resolver: bool,
        seed: impl FnOnce(&Path),
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");

        let notify_ctrl = NotifyController::new(
            client,
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::Exact(".pip-staging".to_string())],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            active_resolver: with_resolver
                .then(|| Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()))),
            skill_layout: Some(SkillLayout::Hermes),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            true,
            config,
        )
        .expect("mount hermes workspace with exact staging pattern");

        let probe = mountpoint.path().join(".pip-staging/notes.txt");
        assert!(
            wait_for(Duration::from_secs(30), || {
                std::fs::read_to_string(&probe).is_ok()
            }),
            "hermes mount never served .pip-staging/notes.txt"
        );

        Self {
            source,
            mountpoint,
            notify_controller: notify_ctrl,
            handle: Some(handle),
        }
    }

    fn mount_path(&self, rel: &str) -> PathBuf {
        self.mountpoint.path().join(rel)
    }

    /// Drive the debounced notify worker a few rounds so any queued
    /// mutation has every chance to surface.
    fn flush_and_settle(&self) {
        for _ in 0..3 {
            self.notify_controller.flush_for_testing();
            std::thread::sleep(Duration::from_millis(60));
        }
        self.notify_controller.flush_for_testing();
    }
}

impl Drop for ExactStagingHermesMount {
    fn drop(&mut self) {
        self.notify_controller.shutdown();
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

fn seed_staging_root(src: &Path) {
    std::fs::create_dir_all(src.join(".pip-staging")).expect("staging root");
    std::fs::write(src.join(".pip-staging/notes.txt"), "staging\n").expect("probe file");
}

/// Staging intermediate mutations inside the Exact-pattern staging root
/// must be suppressed: no notify may fire for a nested id whose category
/// is the staging root. Hermes twin of
/// `staging_writes_never_trigger_notify_even_after_long_wait` (flat,
/// PrefixStar).
#[test]
fn exact_staging_category_suppresses_nested_mutation_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let m = ExactStagingHermesMount::new(client.clone(), false, seed_staging_root);

    // The installer stages a nested skill inside the staging root.
    std::fs::create_dir_all(m.mount_path(".pip-staging/web"))
        .expect("mkdir staged nested skill dir");
    std::fs::write(
        m.mount_path(".pip-staging/web/SKILL.md"),
        "---\nname: web\ndescription: staged\n---\n",
    )
    .expect("write staged nested manifest");
    std::fs::write(m.mount_path(".pip-staging/web/payload.txt"), "data\n")
        .expect("write staged payload");

    m.flush_and_settle();
    assert!(
        client.is_empty(),
        "staging intermediate mutations inside the Exact-pattern staging root must not notify: {:?}",
        client.events()
    );
}

/// With a ledger-driven resolver attached, the staging root keeps full
/// exact-path access: the installer can keep writing the staged skill's
/// files after the manifest makes the directory skill-shaped. Hermes twin
/// of `resolver_staging_exact_path_metadata_succeeds` (flat, PrefixStar).
#[test]
fn exact_staging_category_keeps_installer_access_under_resolver() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let m = ExactStagingHermesMount::new(client.clone(), true, seed_staging_root);

    std::fs::create_dir_all(m.mount_path(".pip-staging/web"))
        .expect("mkdir staged nested skill dir");
    std::fs::write(
        m.mount_path(".pip-staging/web/SKILL.md"),
        "---\nname: web\ndescription: staged\n---\n",
    )
    .expect("write staged nested manifest");

    // The manifest makes the directory skill-shaped, so from here on the
    // hidden-write gate applies to every further file — the staging
    // exemption is the only thing that keeps the installer writing.
    std::fs::write(m.mount_path(".pip-staging/web/payload.txt"), "data\n")
        .expect("staged skill payload must stay writable inside the staging root");

    let read_back = std::fs::read_to_string(m.mount_path(".pip-staging/web/payload.txt"))
        .expect("read staged payload back");
    assert_eq!(read_back, "data\n");
}
