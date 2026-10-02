//! I2: Installer staging compatibility integration tests.
//!
//! Coverage:
//!
//! 1. `.openclaw-install-stage-*` created under managed root does NOT
//!    appear in `/skills` listing.
//! 2. Staging roots are accessible for lookup/getattr so installers
//!    can write inside them through the FUSE mount.
//! 3. Intermediate writes inside staging root do NOT trigger normal
//!    skill notify — not even after waiting well past any timeout.
//! 4. Rename staging root to valid skill triggers exactly one
//!    install-complete notify with no `.openclaw-install-stage-*` events.
//! 5. Rename to `.skill-meta`, `.skillfs-inbox`, lifecycle roots,
//!    `skill-discover`, or invalid skill names is rejected.
//! 6. Default config (no patterns) does not change existing behavior.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, ActiveTarget, InMemoryNotifyClient, InstallerStagingController,
    NotifyClient, NotifyController, PendingInstallController, QuietTimeoutController,
    SlowNotifyClient, StagingConfig, StagingMatcher, StagingPattern,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

fn create_skill(dir: &Path, name: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test\n---\n"),
    )
    .unwrap();
}

struct StagingMountFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    #[allow(dead_code)]
    staging_controller: Arc<InstallerStagingController>,
}

impl StagingMountFixture {
    fn new(seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));
        let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            staging_controller: Some(staging_ctrl.clone()),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            staging_controller: staging_ctrl,
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn wait_for_notify(&self, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.notify_controller.flush_for_testing();
            let count = self.notify_client.len();
            if count >= expected {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {} notify events, got {}",
                    expected, count
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for StagingMountFixture {
    fn drop(&mut self) {
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Default config preserves existing behavior
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn default_config_no_staging_patterns_preserves_behavior() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "alpha");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let mountpoint = tempfile::tempdir().unwrap();

    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        ..MountConfig::default()
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let skills = common::list_dir_names(&mountpoint.path().join("skills"));
    assert!(
        skills.contains(&"alpha".to_string()),
        "alpha must be visible without staging config"
    );

    std::fs::write(mountpoint.path().join("skills/alpha/test.txt"), "hello").unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        notify_ctrl.flush_for_testing();
        if !notify_client.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !notify_client.is_empty(),
        "normal skill write must trigger notify"
    );

    notify_ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Staging root hidden from /skills listing but accessible for writes
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn staging_root_not_in_skills_listing() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|src| {
        create_skill(src, "alpha");
        std::fs::create_dir_all(src.join(".openclaw-install-stage-beta")).unwrap();
        std::fs::write(
            src.join(".openclaw-install-stage-beta/SKILL.md"),
            "---\nname: beta\ndescription: test\n---\n",
        )
        .unwrap();
    });

    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(skills.contains(&"alpha".to_string()));
    assert!(skills.contains(&"skill-discover".to_string()));
    assert!(
        !skills.contains(&".openclaw-install-stage-beta".to_string()),
        "staging root must NOT appear in /skills listing"
    );
}

#[test]
fn staging_root_hidden_from_listing_but_accessible_for_writes() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|src| {
        std::fs::create_dir_all(src.join(".openclaw-install-stage-gamma")).unwrap();
    });

    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        !skills.contains(&".openclaw-install-stage-gamma".to_string()),
        "staging root must not appear in /skills listing"
    );

    let result = std::fs::metadata(fixture.skill_path(".openclaw-install-stage-gamma"));
    assert!(
        result.is_ok(),
        "staging root must be accessible via lookup for installer writes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Staging writes never produce any notify — even after waiting
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn staging_writes_never_trigger_notify_even_after_long_wait() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|src| {
        create_skill(src, "alpha");
    });

    // Create staging dir and write files through the mount path
    let staging_path = fixture.skill_path(".openclaw-install-stage-delta");
    std::fs::create_dir(&staging_path).unwrap();
    std::fs::write(staging_path.join("file1.txt"), "data1").unwrap();
    std::fs::write(staging_path.join("file2.txt"), "data2").unwrap();

    // Wait well past any reasonable timeout (500ms) then flush
    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let staging_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "staging root writes must never produce any notify event, got: {:?}",
        staging_events
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Rename target validation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn rename_target_validation() {
    use skillfs_fuse::security::install::is_valid_staging_rename_target;

    let matcher = StagingMatcher::new(StagingConfig {
        patterns: vec![StagingPattern::PrefixStar(
            ".openclaw-install-stage-".to_string(),
        )],
        ..StagingConfig::default()
    });

    assert!(is_valid_staging_rename_target("my-skill", &matcher));
    assert!(is_valid_staging_rename_target("weather", &matcher));
    assert!(is_valid_staging_rename_target("v2", &matcher));

    assert!(!is_valid_staging_rename_target(".skill-meta", &matcher));
    assert!(!is_valid_staging_rename_target(".skillfs-inbox", &matcher));
    assert!(!is_valid_staging_rename_target("skill-discover", &matcher));
    assert!(!is_valid_staging_rename_target(".staging", &matcher));
    assert!(!is_valid_staging_rename_target(".certified", &matcher));
    assert!(!is_valid_staging_rename_target(".quarantine", &matcher));
    assert!(!is_valid_staging_rename_target(".archive", &matcher));
    assert!(!is_valid_staging_rename_target("", &matcher));
    assert!(!is_valid_staging_rename_target(".git", &matcher));
    assert!(!is_valid_staging_rename_target("Foo_Bar", &matcher));
    assert!(!is_valid_staging_rename_target("-leading", &matcher));
    assert!(!is_valid_staging_rename_target(
        ".openclaw-install-stage-foo",
        &matcher
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// Pattern matching unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn staging_pattern_matching() {
    let config = StagingConfig {
        patterns: vec![
            StagingPattern::PrefixStar(".openclaw-install-stage-".to_string()),
            StagingPattern::Exact(".pip-staging".to_string()),
        ],
        ..StagingConfig::default()
    };
    let matcher = StagingMatcher::new(config);

    assert!(matcher.is_staging_root(".openclaw-install-stage-foo"));
    assert!(matcher.is_staging_root(".openclaw-install-stage-bar-baz"));
    assert!(matcher.is_staging_root(".pip-staging"));

    assert!(!matcher.is_staging_root("alpha"));
    assert!(!matcher.is_staging_root(".pip-staging2"));
    assert!(!matcher.is_staging_root("skill-discover"));
    assert!(!matcher.is_staging_root(".staging"));
    assert!(!matcher.is_staging_root(".skill-meta"));
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE e2e: full installer flow through the mount path
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fuse_e2e_staging_mkdir_write_rename_flow() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|src| {
        create_skill(src, "existing-skill");
    });

    let skills_root = fixture.skills_root();

    // Step 1: existing skill is visible
    let names = common::list_dir_names(&skills_root);
    assert!(names.contains(&"existing-skill".to_string()));

    // Step 2: create staging directory through mount path
    let staging_path = fixture.skill_path(".openclaw-install-stage-newskill");
    std::fs::create_dir(&staging_path).unwrap();

    // Step 3: staging directory must NOT appear in /skills listing
    let names = common::list_dir_names(&skills_root);
    assert!(
        !names.contains(&".openclaw-install-stage-newskill".to_string()),
        "staging root must not appear in /skills listing after mkdir"
    );

    // Step 4: write files inside the staging dir through the mount
    std::fs::write(
        staging_path.join("SKILL.md"),
        "---\nname: newskill\ndescription: installed skill\n---\n",
    )
    .unwrap();
    std::fs::create_dir(staging_path.join("scripts")).unwrap();
    std::fs::write(staging_path.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").unwrap();

    // Step 5: wait well past any timeout — no staging notify must appear
    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();
    let events_before_rename = fixture.notify_client.events();
    let staging_events: Vec<_> = events_before_rename
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "staging writes must not produce any notify, got: {:?}",
        staging_events
    );

    // Step 6: rename staging to final skill through the mount
    let final_path = fixture.skill_path("newskill");
    std::fs::rename(&staging_path, &final_path).unwrap();

    // Step 7: wait for the rename mutation notify
    fixture.wait_for_notify(1);

    // Step 8: verify exactly one rename mutation event for the final
    // skill and no events for the staging name
    let all_events = fixture.notify_client.events();
    let rename_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.skill_id == "newskill" && e.event_kind == "rename")
        .collect();
    assert_eq!(
        rename_events.len(),
        1,
        "expected exactly one rename notify for newskill, got all events: {:?}",
        all_events
    );
    assert_eq!(rename_events[0].schema_version, 2);
    assert_eq!(rename_events[0].skill_id, "newskill");
    assert!(rename_events[0].canonical_skill_dir.ends_with("/newskill"));
    let staging_name_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_name_events.is_empty(),
        "no events should reference the staging name, got: {:?}",
        staging_name_events
    );

    // Step 9: final skill directory should be accessible
    assert!(final_path.is_dir());
    let skill_md = std::fs::read_to_string(final_path.join("SKILL.md")).unwrap();
    assert!(skill_md.contains("installed skill"));
}

#[test]
fn fuse_e2e_staging_rename_to_sensitive_name_rejected() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|_src| {});

    let staging_path = fixture.skill_path(".openclaw-install-stage-bad");
    std::fs::create_dir(&staging_path).unwrap();

    let result = std::fs::rename(&staging_path, fixture.skill_path(".staging"));
    assert!(result.is_err(), "rename to .staging must be rejected");

    let result = std::fs::rename(&staging_path, fixture.skill_path("skill-discover"));
    assert!(result.is_err(), "rename to skill-discover must be rejected");

    assert!(staging_path.is_dir(), "staging dir must still exist");
}

// ─────────────────────────────────────────────────────────────────────────────
// Quiet timeout integration tests
//
// Quiet timeout targets final skill directories — direct writes to a
// legitimate skill name trigger install-complete after the configured
// quiet window. Staging roots are NOT observed by the quiet timeout;
// they complete only through the rename boundary.
// ─────────────────────────────────────────────────────────────────────────────

struct QuietTimeoutMountFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    #[allow(dead_code)]
    staging_controller: Arc<InstallerStagingController>,
    quiet_controller: Arc<QuietTimeoutController>,
}

impl QuietTimeoutMountFixture {
    fn new(quiet_timeout_ms: u64, seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));
        let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());
        let quiet_ctrl = QuietTimeoutController::new(
            notify_ctrl.clone(),
            Duration::from_millis(quiet_timeout_ms),
        );

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            staging_controller: Some(staging_ctrl.clone()),
            quiet_timeout_controller: Some(quiet_ctrl.clone()),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            staging_controller: staging_ctrl,
            quiet_controller: quiet_ctrl,
        }
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn wait_for_quiet_mutation(&self, skill_name: &str, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.quiet_controller.flush_for_testing();
            self.notify_controller.flush_for_testing();
            let count = self
                .notify_client
                .events()
                .iter()
                .filter(|e| {
                    e.skill_id == skill_name
                        && e.event_kind != "install-complete"
                        && e.paths.is_empty()
                })
                .count();
            if count >= expected {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {} quiet mutation events for {}, got {}.\nevents: {:?}",
                    expected,
                    skill_name,
                    count,
                    self.notify_client.events()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for QuietTimeoutMountFixture {
    fn drop(&mut self) {
        self.quiet_controller.shutdown();
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn quiet_timeout_staging_writes_never_trigger_notify() {
    skip_if_no_fuse!();

    let fixture = QuietTimeoutMountFixture::new(150, |src| {
        create_skill(src, "alpha");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-qt1");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("SKILL.md"), "---\nname: qt1\n---\n").unwrap();
    std::fs::write(staging.join("data.txt"), "payload").unwrap();

    std::thread::sleep(Duration::from_millis(300));
    fixture.quiet_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let staging_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "staging root writes must never trigger any notify, got: {:?}",
        staging_events
    );
}

#[test]
fn quiet_timeout_direct_final_skill_triggers_mutation_notify() {
    skip_if_no_fuse!();

    let fixture = QuietTimeoutMountFixture::new(150, |src| {
        create_skill(src, "direct-install");
    });

    let skill_dir = fixture.skill_path("direct-install");
    std::fs::write(skill_dir.join("data.txt"), "payload").unwrap();

    fixture.wait_for_quiet_mutation("direct-install", 1);

    let events = fixture.notify_client.events();
    let mutation_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.skill_id == "direct-install"
                && e.event_kind != "install-complete"
                && e.paths.is_empty()
        })
        .collect();
    assert_eq!(
        mutation_events.len(),
        1,
        "expected one quiet-timeout mutation notify for final skill, got: {:?}",
        events
    );
    assert_eq!(mutation_events[0].schema_version, 2);
    assert!(
        mutation_events[0]
            .canonical_skill_dir
            .ends_with("/direct-install")
    );
    assert!(
        events.iter().all(|e| e.event_kind != "install-complete"),
        "install-complete must not appear in protocol events"
    );
}

#[test]
fn quiet_timeout_direct_writes_collapse_to_one() {
    skip_if_no_fuse!();

    let fixture = QuietTimeoutMountFixture::new(200, |src| {
        create_skill(src, "multi-write");
    });

    let skill_dir = fixture.skill_path("multi-write");
    for i in 0..5 {
        std::fs::write(skill_dir.join(format!("file{i}.txt")), format!("data{i}")).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }

    fixture.wait_for_quiet_mutation("multi-write", 1);

    let events = fixture.notify_client.events();
    let quiet_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.skill_id == "multi-write" && e.event_kind != "install-complete" && e.paths.is_empty()
        })
        .collect();
    assert_eq!(
        quiet_events.len(),
        1,
        "multiple writes within quiet window must produce one mutation notify, got: {:?}",
        events
    );
    assert!(
        events.iter().all(|e| e.event_kind != "install-complete"),
        "install-complete must not appear in protocol events"
    );
}

#[test]
fn no_quiet_timeout_direct_writes_do_not_fire_quiet_mutation() {
    skip_if_no_fuse!();

    let fixture = StagingMountFixture::new(|src| {
        create_skill(src, "alpha");
    });

    std::fs::write(fixture.skill_path("alpha").join("data.txt"), "hello").unwrap();

    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    // Without quiet_timeout, only the debounced normal mutation fires
    // (with non-empty paths). No empty-paths quiet-timeout event should appear.
    let quiet_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "alpha" && e.paths.is_empty())
        .collect();
    assert!(
        quiet_events.is_empty(),
        "without quiet_timeout_ms, direct skill writes must not produce quiet-timeout mutation, got: {:?}",
        events
    );
}

#[test]
fn quiet_timeout_rename_still_immediate_no_staging_events() {
    skip_if_no_fuse!();

    let fixture = QuietTimeoutMountFixture::new(5000, |_src| {});

    let staging = fixture.skill_path(".openclaw-install-stage-ren");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("SKILL.md"), "---\nname: renamed-skill\n---\n").unwrap();

    let final_path = fixture.skill_path("renamed-skill");
    std::fs::rename(&staging, &final_path).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        let count = fixture
            .notify_client
            .events()
            .iter()
            .filter(|e| e.skill_id == "renamed-skill" && e.event_kind == "rename")
            .count();
        if count >= 1 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "rename mutation notify must fire promptly, events: {:?}",
                fixture.notify_client.events()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let events = fixture.notify_client.events();
    let staging_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "no staging root events should exist, got: {:?}",
        staging_events
    );
    assert!(
        events.iter().all(|e| e.event_kind != "install-complete"),
        "install-complete must not appear in any events"
    );
}

#[test]
fn quiet_timeout_skill_meta_writes_do_not_trigger() {
    skip_if_no_fuse!();

    let fixture = QuietTimeoutMountFixture::new(150, |src| {
        create_skill(src, "meta-test");
        let meta = src.join("meta-test/.skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
    });

    // .skill-meta writes are blocked by policy; but even if a trusted
    // writer wrote there, the quiet timeout controller must not fire.
    // We verify indirectly: write to the skill (triggers quiet timeout)
    // then check that only non-.skill-meta writes contributed.
    let skill_dir = fixture.skill_path("meta-test");
    std::fs::write(skill_dir.join("data.txt"), "payload").unwrap();

    fixture.wait_for_quiet_mutation("meta-test", 1);

    let events = fixture.notify_client.events();
    let quiet_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.skill_id == "meta-test" && e.event_kind != "install-complete" && e.paths.is_empty()
        })
        .collect();
    assert_eq!(quiet_events.len(), 1);
    assert!(
        events.iter().all(|e| e.event_kind != "install-complete"),
        "install-complete must not appear"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Staging rename must not block FUSE reply on slow notify
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn staging_rename_does_not_block_fuse_reply() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "existing");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    // 2-second delay per send — if rename blocks on this, the test will timeout.
    let slow_client: Arc<dyn NotifyClient> =
        Arc::new(SlowNotifyClient::new(Duration::from_secs(2)));

    let notify_ctrl = NotifyController::new(
        slow_client,
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::PrefixStar(
            ".openclaw-install-stage-".to_string(),
        )],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        staging_matcher: Some(matcher),
        staging_controller: Some(staging_ctrl.clone()),
        ..MountConfig::default()
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let skills_root = mountpoint.path().join("skills");
    let staging_path = skills_root.join(".openclaw-install-stage-fast");
    std::fs::create_dir(&staging_path).unwrap();
    std::fs::write(
        staging_path.join("SKILL.md"),
        "---\nname: fast-skill\n---\n",
    )
    .unwrap();

    // Rename must complete quickly despite 2s slow client.
    let start = std::time::Instant::now();
    let final_path = skills_root.join("fast-skill");
    std::fs::rename(&staging_path, &final_path).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "staging rename must not block on slow notify client; took {:?}",
        elapsed
    );

    notify_ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Staging exact-path traversal with active resolver
//
// When an active resolver is present, skills without an entry default to
// hidden.  Staging roots must bypass the resolver so installers can
// opendir/readdir/read inside them.
// ─────────────────────────────────────────────────────────────────────────────

struct ResolverStagingFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
}

impl ResolverStagingFixture {
    fn new(seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));
        let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

        let resolver = ActiveSkillResolver::new(source.path().to_path_buf());
        let skill_names: Vec<String> = shared.read().list().iter().map(|s| s.to_string()).collect();
        for name in &skill_names {
            if name == "skill-discover" {
                continue;
            }
            let skill_dir = source.path().join(name);
            resolver.set(
                name.clone(),
                ActiveTarget::Current {
                    source_dir: skill_dir,
                },
            );
        }

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            staging_controller: Some(staging_ctrl.clone()),
            active_resolver: Some(Arc::new(resolver)),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }
}

impl Drop for ResolverStagingFixture {
    fn drop(&mut self) {
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn resolver_staging_not_in_skills_listing() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
        std::fs::create_dir_all(src.join(".openclaw-install-stage-beta")).unwrap();
        std::fs::write(
            src.join(".openclaw-install-stage-beta/SKILL.md"),
            "---\nname: beta\ndescription: staged\n---\n",
        )
        .unwrap();
    });

    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(skills.contains(&"alpha".to_string()));
    assert!(
        !skills.contains(&".openclaw-install-stage-beta".to_string()),
        "staging root must NOT appear in /skills listing: {:?}",
        skills
    );
}

#[test]
fn resolver_staging_exact_path_metadata_succeeds() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
        std::fs::create_dir_all(src.join(".openclaw-install-stage-beta")).unwrap();
        std::fs::write(
            src.join(".openclaw-install-stage-beta/SKILL.md"),
            "---\nname: beta\ndescription: staged\n---\n",
        )
        .unwrap();
    });

    let result = std::fs::metadata(fixture.skill_path(".openclaw-install-stage-beta"));
    assert!(
        result.is_ok(),
        "staging root must be accessible via exact lookup even with active resolver"
    );
    assert!(result.unwrap().is_dir());
}

#[test]
fn resolver_staging_exact_path_readdir_succeeds() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
        let staging = src.join(".openclaw-install-stage-gamma");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(
            staging.join("SKILL.md"),
            "---\nname: gamma\ndescription: staged\n---\n",
        )
        .unwrap();
        std::fs::write(staging.join("data.txt"), "payload").unwrap();
    });

    let staging_path = fixture.skill_path(".openclaw-install-stage-gamma");
    let entries = common::list_dir_names(&staging_path);
    assert!(
        entries.contains(&"SKILL.md".to_string()),
        "staging readdir must include SKILL.md: {:?}",
        entries
    );
    assert!(
        entries.contains(&"data.txt".to_string()),
        "staging readdir must include ordinary files: {:?}",
        entries
    );
}

#[test]
fn resolver_staging_subdirectory_readdir_succeeds() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
        let staging = src.join(".openclaw-install-stage-delta");
        std::fs::create_dir_all(staging.join("scripts")).unwrap();
        std::fs::write(staging.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    });

    let subdir = fixture
        .skill_path(".openclaw-install-stage-delta")
        .join("scripts");
    let entries = common::list_dir_names(&subdir);
    assert!(
        entries.contains(&"run.sh".to_string()),
        "staging subdirectory readdir must succeed: {:?}",
        entries
    );
}

#[test]
fn resolver_staging_file_read_succeeds() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
        let staging = src.join(".openclaw-install-stage-epsilon");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(
            staging.join("SKILL.md"),
            "---\nname: epsilon\ndescription: staged\n---\n",
        )
        .unwrap();
        std::fs::write(staging.join("payload.txt"), "test-data-42").unwrap();
    });

    let staging_path = fixture.skill_path(".openclaw-install-stage-epsilon");

    let skill_md = std::fs::read_to_string(staging_path.join("SKILL.md")).unwrap();
    assert!(
        skill_md.contains("name: epsilon"),
        "staging SKILL.md must be readable as raw file: {:?}",
        skill_md
    );

    let payload = std::fs::read_to_string(staging_path.join("payload.txt")).unwrap();
    assert_eq!(payload, "test-data-42");
}

#[test]
fn resolver_staging_intermediate_writes_do_not_notify() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-zeta");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("file.txt"), "data").unwrap();
    std::fs::create_dir(staging.join("sub")).unwrap();
    std::fs::write(staging.join("sub/nested.txt"), "nested").unwrap();

    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();

    let staging_events: Vec<_> = fixture
        .notify_client
        .events()
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .cloned()
        .collect();
    assert!(
        staging_events.is_empty(),
        "staging writes must not trigger notify: {:?}",
        staging_events
    );
}

#[test]
fn resolver_staging_rename_triggers_rename_mutation() {
    skip_if_no_fuse!();

    let fixture = ResolverStagingFixture::new(|src| {
        create_skill(src, "alpha");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-eta");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: eta\ndescription: installed\n---\n",
    )
    .unwrap();

    let final_path = fixture.skill_path("eta");
    std::fs::rename(&staging, &final_path).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        let count = fixture
            .notify_client
            .events()
            .iter()
            .filter(|e| e.skill_id == "eta" && e.event_kind == "rename")
            .count();
        if count >= 1 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for rename mutation, events: {:?}",
                fixture.notify_client.events()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let all_events = fixture.notify_client.events();
    let staging_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "no staging events should exist: {:?}",
        staging_events
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Direct final-skill pending install tests
//
// When a PendingInstallController is attached with an ActiveSkillResolver,
// newly created skill directories that have no activation entry are treated
// as pending installs: hidden from /skills listing, accessible via exact
// path, intermediate mutations suppressed, and notify only fires after the
// quiet timeout observes a complete skill shape (directory + parseable
// SKILL.md).
// ─────────────────────────────────────────────────────────────────────────────

struct PendingInstallFixture {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    pending_controller: Arc<PendingInstallController>,
    resolver: Arc<ActiveSkillResolver>,
}

impl PendingInstallFixture {
    fn new(pending_timeout_ms: u64, seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        let skill_names: Vec<String> = shared.read().list().iter().map(|s| s.to_string()).collect();
        for name in &skill_names {
            if name == "skill-discover" {
                continue;
            }
            let skill_dir = source.path().join(name);
            resolver.set(
                name.clone(),
                ActiveTarget::Current {
                    source_dir: skill_dir,
                },
            );
        }

        let pending_ctrl = PendingInstallController::new(
            notify_ctrl.clone(),
            Duration::from_millis(pending_timeout_ms),
            source.path().to_path_buf(),
        );

        let mount_resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        for name in &skill_names {
            if name == "skill-discover" {
                continue;
            }
            mount_resolver.set(
                name.clone(),
                ActiveTarget::Current {
                    source_dir: source.path().join(name),
                },
            );
        }

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            active_resolver: Some(mount_resolver.clone()),
            pending_install_controller: Some(pending_ctrl.clone()),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            pending_controller: pending_ctrl,
            resolver: mount_resolver,
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }
}

impl Drop for PendingInstallFixture {
    fn drop(&mut self) {
        self.pending_controller.shutdown();
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn pending_mkdir_does_not_notify() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(200, |src| {
        create_skill(src, "existing");
    });

    // Create a new skill directory (no activation for it)
    let new_skill = fixture.skill_path("new-skill");
    std::fs::create_dir(&new_skill).unwrap();

    // Wait well past timeout
    std::thread::sleep(Duration::from_millis(400));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    // No notify because SKILL.md is missing
    let events = fixture.notify_client.events();
    let new_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "new-skill")
        .collect();
    assert!(
        new_events.is_empty(),
        "mkdir without SKILL.md must not trigger notify: {:?}",
        new_events
    );
}

#[test]
fn pending_skill_not_in_listing() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("new-skill");
    std::fs::create_dir(&new_skill).unwrap();

    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        skills.contains(&"existing".to_string()),
        "existing skill must be visible"
    );
    assert!(
        !skills.contains(&"new-skill".to_string()),
        "pending skill must NOT appear in /skills listing: {:?}",
        skills
    );
}

#[test]
fn pending_exact_path_accessible() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("new-skill");
    std::fs::create_dir(&new_skill).unwrap();

    // Exact path access must work
    let meta = std::fs::metadata(&new_skill);
    assert!(
        meta.is_ok(),
        "pending skill must be accessible via exact path"
    );
    assert!(meta.unwrap().is_dir());

    // Write files
    std::fs::write(new_skill.join("data.txt"), "test").unwrap();
    let content = std::fs::read_to_string(new_skill.join("data.txt")).unwrap();
    assert_eq!(content, "test");

    // Create subdirectory
    std::fs::create_dir(new_skill.join("sub")).unwrap();
    std::fs::write(new_skill.join("sub/nested.txt"), "nested").unwrap();

    // readdir
    let entries = common::list_dir_names(&new_skill);
    assert!(
        entries.contains(&"data.txt".to_string()),
        "pending skill readdir must work: {:?}",
        entries
    );
    assert!(
        entries.contains(&"sub".to_string()),
        "pending skill readdir must include subdirs: {:?}",
        entries
    );
}

#[test]
fn pending_skill_md_missing_no_notify() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(150, |src| {
        create_skill(src, "existing");
    });

    // Create skill dir with just a data file, no SKILL.md
    let new_skill = fixture.skill_path("incomplete");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(new_skill.join("data.txt"), "test").unwrap();

    std::thread::sleep(Duration::from_millis(300));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let incomplete_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "incomplete")
        .collect();
    assert!(
        incomplete_events.is_empty(),
        "incomplete skill (no SKILL.md) must not notify: {:?}",
        incomplete_events
    );
}

#[test]
fn pending_skill_md_unparseable_no_notify() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(150, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("bad-parse");
    std::fs::create_dir(&new_skill).unwrap();
    // Write empty SKILL.md — parser returns Error status for empty content
    std::fs::write(new_skill.join("SKILL.md"), "").unwrap();

    std::thread::sleep(Duration::from_millis(300));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let bad_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "bad-parse")
        .collect();
    assert!(
        bad_events.is_empty(),
        "unparseable SKILL.md (error status) must not notify: {:?}",
        bad_events
    );
}

#[test]
fn pending_complete_skill_notifies_after_timeout() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(200, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("complete-skill");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(
        new_skill.join("SKILL.md"),
        "---\nname: complete-skill\ndescription: test\n---\n",
    )
    .unwrap();

    // Wait for timeout + flush
    std::thread::sleep(Duration::from_millis(400));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let complete_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "complete-skill")
        .collect();
    assert_eq!(
        complete_events.len(),
        1,
        "complete skill must trigger exactly one notify: {:?}",
        events
    );
    assert_eq!(complete_events[0].schema_version, 2);
    assert!(
        complete_events[0]
            .canonical_skill_dir
            .ends_with("/complete-skill")
    );
    assert!(
        events.iter().all(|e| e.event_kind != "install-complete"),
        "install-complete must not appear in protocol events"
    );
}

#[test]
fn pending_multiple_writes_collapse() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(200, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("multi-write-pending");
    std::fs::create_dir(&new_skill).unwrap();
    for i in 0..5 {
        std::fs::write(new_skill.join(format!("file{i}.txt")), format!("data{i}")).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
    std::fs::write(
        new_skill.join("SKILL.md"),
        "---\nname: multi-write-pending\ndescription: test\n---\n",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(400));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    let events = fixture.notify_client.events();
    let mw_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "multi-write-pending")
        .collect();
    assert_eq!(
        mw_events.len(),
        1,
        "multiple writes must collapse to one notify: {:?}",
        events
    );
}

#[test]
fn pending_activation_clears_pending_and_shows_skill() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(200, |src| {
        create_skill(src, "existing");
    });

    // Create a new skill and write a complete SKILL.md
    let new_skill = fixture.skill_path("activated-new");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(
        new_skill.join("SKILL.md"),
        "---\nname: activated-new\ndescription: test\n---\n",
    )
    .unwrap();

    // Wait for pending to fire
    std::thread::sleep(Duration::from_millis(400));
    fixture.pending_controller.flush_for_testing();
    fixture.notify_controller.flush_for_testing();

    // Verify notify happened
    let events = fixture.notify_client.events();
    assert!(
        events.iter().any(|e| e.skill_id == "activated-new"),
        "pending complete must have fired notify"
    );

    // Still pending (notified state) — must not appear in listing yet
    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        !skills.contains(&"activated-new".to_string()),
        "notified-pending skill must not appear in listing before activation: {:?}",
        skills
    );

    // Simulate daemon writing activation: set resolver to current
    fixture.resolver.set(
        "activated-new".to_string(),
        ActiveTarget::Current {
            source_dir: fixture.source.path().join("activated-new"),
        },
    );

    // Now the skill must appear in /skills listing because
    // is_pending_install checks the resolver and auto-clears
    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        skills.contains(&"activated-new".to_string()),
        "skill must appear in listing after activation is set: {:?}",
        skills
    );
}

/// #1264 regression: a direct final-skill pending install dir must still
/// accept `cp -a`-style metadata preservation (chmod + utimensat) on the
/// top-level directory instead of failing with EROFS. Pending semantics
/// (hidden from /skills listing) must be preserved.
#[test]
fn pending_skill_dir_metadata_preservation() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let new_skill = fixture.skill_path("cp-a-pending");
    std::fs::create_dir(&new_skill).unwrap();

    // chmod (setattr mode) must succeed on the pending skill dir.
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&new_skill, std::fs::Permissions::from_mode(0o755))
        .expect("chmod pending skill dir must succeed");

    // utimensat (setattr atime/mtime) must succeed on the pending skill dir.
    let c_path = std::ffi::CString::new(new_skill.to_string_lossy().into_owned()).unwrap();
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
    ];
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(
        rc,
        0,
        "utimensat pending skill dir must succeed, errno={}",
        std::io::Error::last_os_error()
    );

    // Pending skill must still be hidden from the /skills listing.
    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        !skills.contains(&"cp-a-pending".to_string()),
        "pending skill must remain hidden from listing: {:?}",
        skills
    );
}

#[test]
fn pending_activated_skill_uses_normal_notify() {
    skip_if_no_fuse!();

    let fixture = PendingInstallFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    // "existing" has activation — writes should go through normal notify,
    // not pending install
    let existing = fixture.skill_path("existing");
    std::fs::write(existing.join("data.txt"), "update").unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        let count = fixture
            .notify_client
            .events()
            .iter()
            .filter(|e| e.skill_id == "existing")
            .count();
        if count >= 1 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "activated skill write must trigger normal notify, events: {:?}",
                fixture.notify_client.events()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // The notify for "existing" should come from normal notify path
    // (with paths populated), not from pending install
    let events = fixture.notify_client.events();
    let existing_events: Vec<_> = events.iter().filter(|e| e.skill_id == "existing").collect();
    assert!(
        !existing_events.is_empty(),
        "activated skill must use normal notify"
    );
}

#[test]
fn pending_staging_inbox_not_affected() {
    skip_if_no_fuse!();

    // Create a fixture with staging patterns too
    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "alpha");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();
    let notify_client = Arc::new(InMemoryNotifyClient::new());

    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::PrefixStar(
            ".openclaw-install-stage-".to_string(),
        )],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver.set(
        "alpha".to_string(),
        ActiveTarget::Current {
            source_dir: source.path().join("alpha"),
        },
    );

    let pending_ctrl = PendingInstallController::new(
        notify_ctrl.clone(),
        Duration::from_millis(5000),
        source.path().to_path_buf(),
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        staging_matcher: Some(matcher),
        staging_controller: Some(staging_ctrl.clone()),
        active_resolver: Some(resolver),
        pending_install_controller: Some(pending_ctrl.clone()),
        ..MountConfig::default()
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    // Staging writes should still be fully suppressed
    let staging = mountpoint
        .path()
        .join("skills/.openclaw-install-stage-test");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("file.txt"), "data").unwrap();

    std::thread::sleep(Duration::from_millis(500));
    pending_ctrl.flush_for_testing();
    notify_ctrl.flush_for_testing();

    let staging_events: Vec<_> = notify_client
        .events()
        .iter()
        .filter(|e| e.skill_id.starts_with(".openclaw-install-stage-"))
        .cloned()
        .collect();
    assert!(
        staging_events.is_empty(),
        "staging writes must still be suppressed: {:?}",
        staging_events
    );

    pending_ctrl.shutdown();
    notify_ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// I4: Post-publish grace window tests
//
// After staging rename or pending install completion, a time-limited grace
// window allows the installer to write whitelisted metadata paths into the
// final skill directory even when the active resolver marks the skill as
// hidden. The grace window is explicit-whitelist, default-off, and rejects
// .skill-meta/** unconditionally.
// ─────────────────────────────────────────────────────────────────────────────

use skillfs_fuse::security::{PostPublishGraceController, PostPublishWritePattern};

struct PostPublishFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    #[allow(dead_code)]
    post_publish_controller: Arc<PostPublishGraceController>,
}

impl PostPublishFixture {
    fn new(grace_ms: u64, seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));
        let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

        // Active resolver: existing skills are current, new ones are hidden
        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        let skill_names: Vec<String> = shared.read().list().iter().map(|s| s.to_string()).collect();
        for name in &skill_names {
            if name == "skill-discover" {
                continue;
            }
            resolver.set(
                name.clone(),
                ActiveTarget::Current {
                    source_dir: source.path().join(name),
                },
            );
        }

        let pp_patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let pp_ctrl = PostPublishGraceController::new(Duration::from_millis(grace_ms), pp_patterns);

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            staging_controller: Some(staging_ctrl),
            active_resolver: Some(resolver),
            post_publish_controller: Some(pp_ctrl.clone()),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            post_publish_controller: pp_ctrl,
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn wait_for_notify(&self, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.notify_controller.flush_for_testing();
            let count = self.notify_client.len();
            if count >= expected {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {} notify events, got {}",
                    expected, count
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for PostPublishFixture {
    fn drop(&mut self) {
        self.post_publish_controller.shutdown();
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn post_publish_grace_allows_whitelisted_write_after_staging_rename() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    // Create staging dir and populate it
    let staging = fixture.skill_path(".openclaw-install-stage-newskill");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: newskill\ndescription: installed\n---\n",
    )
    .unwrap();

    // Rename staging to final skill
    let final_path = fixture.skill_path("newskill");
    std::fs::rename(&staging, &final_path).unwrap();

    // Wait for rename notify
    fixture.wait_for_notify(1);

    // The skill should be hidden from listing (no activation), but grace
    // allows exact-path access to whitelisted paths.
    let skills = common::list_dir_names(&fixture.skills_root());
    assert!(
        !skills.contains(&"newskill".to_string()),
        "newly published skill without activation must not appear in listing: {:?}",
        skills
    );

    // Create the .openclaw directory and write metadata — grace should allow this
    std::fs::create_dir(final_path.join(".openclaw")).unwrap();
    std::fs::write(
        final_path.join(".openclaw/.fs-safe-replace.tmp"),
        "metadata",
    )
    .unwrap();

    // Verify the write landed
    let content =
        std::fs::read_to_string(final_path.join(".openclaw/.fs-safe-replace.tmp")).unwrap();
    assert_eq!(content, "metadata");
}

#[test]
fn post_publish_grace_rejects_non_whitelisted_path() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-blocked");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: blocked\ndescription: test\n---\n",
    )
    .unwrap();

    let final_path = fixture.skill_path("blocked");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Attempt to create a non-whitelisted directory — should fail (ENOENT)
    let result = std::fs::create_dir(final_path.join("other-dir"));
    assert!(
        result.is_err(),
        "non-whitelisted path must be rejected during grace window"
    );
}

#[test]
fn post_publish_grace_expires() {
    skip_if_no_fuse!();

    // Short grace window — 200ms
    let fixture = PostPublishFixture::new(200, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-expired");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: expired\ndescription: test\n---\n",
    )
    .unwrap();

    let final_path = fixture.skill_path("expired");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Wait for grace to expire
    std::thread::sleep(Duration::from_millis(400));

    // Now whitelisted write should fail (grace expired)
    let result = std::fs::create_dir(final_path.join(".openclaw"));
    assert!(
        result.is_err(),
        "whitelisted write must fail after grace expires"
    );
}

#[test]
fn post_publish_grace_rejects_skill_meta() {
    skip_if_no_fuse!();

    // Pre-create the staging dir with .skill-meta in the source directly
    // (not through FUSE, where .skill-meta mkdir is policy-blocked).
    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
        let staging = src.join(".openclaw-install-stage-meta");
        std::fs::create_dir_all(staging.join(".skill-meta")).unwrap();
        std::fs::write(
            staging.join("SKILL.md"),
            "---\nname: meta-test\ndescription: test\n---\n",
        )
        .unwrap();
    });

    let staging = fixture.skill_path(".openclaw-install-stage-meta");
    let final_path = fixture.skill_path("meta-test");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // .skill-meta write must be rejected even during grace
    let result = std::fs::write(final_path.join(".skill-meta/activation.json"), "{}");
    assert!(
        result.is_err(),
        ".skill-meta write must be rejected even during grace"
    );
}

#[test]
fn post_publish_writes_produce_normal_mutation_notify() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-notified");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: notified\ndescription: test\n---\n",
    )
    .unwrap();

    let final_path = fixture.skill_path("notified");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Clear events from the rename
    let events_before = fixture.notify_client.len();

    // Write whitelisted path
    std::fs::create_dir(final_path.join(".openclaw")).unwrap();
    std::fs::write(final_path.join(".openclaw/metadata.tmp"), "installer-data").unwrap();

    // Grace writes should produce normal mutation notify
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        if fixture.notify_client.len() > events_before {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let all_events = fixture.notify_client.events();
    // Verify no install-complete event kind was generated
    assert!(
        all_events
            .iter()
            .all(|e| e.event_kind != "install-complete"),
        "grace writes must produce normal mutation notify, not install-complete"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// I4 negative security boundary tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn post_publish_grace_readdir_does_not_list_hidden_skill_contents() {
    skip_if_no_fuse!();

    // Don't pre-populate the skill in source — use staging rename so the
    // resolver has no activation for the skill (hidden by default).
    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    // Create staging dir with files
    let staging = fixture.skill_path(".openclaw-install-stage-readdir");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: readdir-test\ndescription: test\n---\n",
    )
    .unwrap();
    std::fs::write(staging.join("secret.txt"), "do not list me").unwrap();
    std::fs::create_dir(staging.join(".openclaw")).unwrap();
    std::fs::write(staging.join(".openclaw/meta.json"), "{}").unwrap();

    // Rename staging to final skill — starts grace session, skill is hidden
    let final_path = fixture.skill_path("readdir-test");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // The hidden skill must NOT be listable via readdir during grace.
    // read_dir calls opendir which is blocked for hidden skills.
    let result = std::fs::read_dir(&final_path);
    assert!(
        result.is_err(),
        "readdir on hidden skill must fail during grace, got entries: {:?}",
        result.ok().map(|r| r
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>())
    );
}

#[test]
fn post_publish_grace_non_whitelisted_file_read_rejected() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        let skill = src.join("read-reject");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: read-reject\ndescription: test\n---\n",
        )
        .unwrap();
        std::fs::write(skill.join("secret.txt"), "confidential").unwrap();
    });

    let staging = fixture.skill_path(".openclaw-install-stage-readreject");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("SKILL.md"), "---\nname: read-reject\n---\n").unwrap();
    let final_path = fixture.skill_path("read-reject");
    let _ = std::fs::remove_dir_all(&final_path);
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Non-whitelisted file read must fail during grace
    let result = std::fs::read_to_string(final_path.join("secret.txt"));
    assert!(
        result.is_err(),
        "reading non-whitelisted file must fail during grace"
    );
}

#[test]
fn post_publish_grace_skill_md_write_rejected() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-mdwrite");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: md-write-test\ndescription: test\n---\n",
    )
    .unwrap();
    let final_path = fixture.skill_path("md-write-test");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // SKILL.md write must fail during grace (SKILL.md is never grace-allowed)
    let result = std::fs::write(
        final_path.join("SKILL.md"),
        "---\nname: md-write-test\ndescription: hacked\n---\n",
    );
    assert!(
        result.is_err(),
        "SKILL.md write must be rejected during grace"
    );
}

#[test]
fn post_publish_grace_cross_whitelist_rename_rejected() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-crossrename");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: cross-rename\ndescription: test\n---\n",
    )
    .unwrap();
    std::fs::create_dir(staging.join(".openclaw")).unwrap();
    std::fs::write(staging.join(".openclaw/tmp.dat"), "data").unwrap();
    let final_path = fixture.skill_path("cross-rename");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Rename from whitelist to non-whitelist must fail
    let result = std::fs::rename(
        final_path.join(".openclaw/tmp.dat"),
        final_path.join("escaped.txt"),
    );
    assert!(
        result.is_err(),
        "rename from .openclaw/* to non-whitelist path must fail during grace"
    );
}

#[test]
fn post_publish_grace_intra_whitelist_rename_allowed() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        create_skill(src, "existing");
    });

    let staging = fixture.skill_path(".openclaw-install-stage-intrarename");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: intra-rename\ndescription: test\n---\n",
    )
    .unwrap();
    std::fs::create_dir(staging.join(".openclaw")).unwrap();
    std::fs::write(staging.join(".openclaw/old.tmp"), "data").unwrap();
    let final_path = fixture.skill_path("intra-rename");
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Rename within whitelist must succeed
    let result = std::fs::rename(
        final_path.join(".openclaw/old.tmp"),
        final_path.join(".openclaw/new.dat"),
    );
    assert!(
        result.is_ok(),
        "rename within .openclaw/** must succeed during grace: {:?}",
        result.err()
    );

    // Verify the file was actually renamed
    let content = std::fs::read_to_string(final_path.join(".openclaw/new.dat")).unwrap();
    assert_eq!(content, "data");
}

#[test]
fn post_publish_grace_non_whitelisted_truncate_rejected() {
    skip_if_no_fuse!();

    let fixture = PostPublishFixture::new(5000, |src| {
        let skill = src.join("truncate-test");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: truncate-test\ndescription: test\n---\n",
        )
        .unwrap();
        std::fs::write(skill.join("data.txt"), "important content").unwrap();
    });

    let staging = fixture.skill_path(".openclaw-install-stage-truncate");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("SKILL.md"), "---\nname: truncate-test\n---\n").unwrap();
    let final_path = fixture.skill_path("truncate-test");
    let _ = std::fs::remove_dir_all(&final_path);
    std::fs::rename(&staging, &final_path).unwrap();
    fixture.wait_for_notify(1);

    // Opening a non-whitelisted file for writing (truncate) must fail
    let result = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(final_path.join("data.txt"));
    assert!(
        result.is_err(),
        "opening non-whitelisted file for truncate must fail during grace"
    );
}

#[test]
fn post_publish_grace_direct_pending_complete_triggers_session() {
    skip_if_no_fuse!();

    // This test uses a PendingInstallController with PostPublishGraceController
    // to verify that pending install completion triggers a grace session.
    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "existing");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();
    let notify_client = Arc::new(InMemoryNotifyClient::new());

    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver.set(
        "existing".to_string(),
        ActiveTarget::Current {
            source_dir: source.path().join("existing"),
        },
    );

    let pp_patterns = vec![PostPublishWritePattern::PrefixRecursive(
        ".openclaw".to_string(),
    )];
    let pp_ctrl = PostPublishGraceController::new(Duration::from_millis(5000), pp_patterns);

    let pending_ctrl = PendingInstallController::new_with_post_publish(
        notify_ctrl.clone(),
        Duration::from_millis(200),
        source.path().to_path_buf(),
        Some(pp_ctrl.clone()),
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        active_resolver: Some(resolver),
        pending_install_controller: Some(pending_ctrl.clone()),
        post_publish_controller: Some(pp_ctrl.clone()),
        ..MountConfig::default()
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    // Create a new skill via direct-write install
    let new_skill = mountpoint.path().join("skills/direct-grace");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(
        new_skill.join("SKILL.md"),
        "---\nname: direct-grace\ndescription: test\n---\n",
    )
    .unwrap();

    // Wait for pending timeout + flush
    std::thread::sleep(Duration::from_millis(400));
    pending_ctrl.flush_for_testing();
    notify_ctrl.flush_for_testing();

    // Verify notify fired
    let events = notify_client.events();
    assert!(
        events.iter().any(|e| e.skill_id == "direct-grace"),
        "pending complete must have fired notify: {:?}",
        events
    );

    // Now grace session should be active — whitelisted write should succeed
    std::fs::create_dir(new_skill.join(".openclaw")).unwrap();
    let result = std::fs::write(new_skill.join(".openclaw/installed.json"), "{}");
    assert!(
        result.is_ok(),
        "whitelisted write after pending complete must succeed: {:?}",
        result.err()
    );

    pp_ctrl.shutdown();
    pending_ctrl.shutdown();
    notify_ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}
// ─────────────────────────────────────────────────────────────────────────────
// I4: Post-publish grace fallback snapshot tests
//
// When the active resolver maps a skill to a Snapshot (fallback) that does NOT
// contain `.clawhub`, and a post-publish grace session is active with pattern
// `.clawhub/**`, exact-path writes to `.clawhub/**` must route to the physical
// source instead of failing with ENOENT from the snapshot view.
// ─────────────────────────────────────────────────────────────────────────────

use skillfs_fuse::security::PostPublishSessionKind;

struct FallbackGraceFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    #[allow(dead_code)]
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    post_publish_controller: Arc<PostPublishGraceController>,
}

impl FallbackGraceFixture {
    fn new(grace_ms: u64, seed: impl FnOnce(&Path, &Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        let snapshot_dir = source.path().join("fallback-skill/.skill-meta/versions/v1");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        seed(source.path(), &snapshot_dir);

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        resolver.set(
            "fallback-skill".to_string(),
            ActiveTarget::Snapshot {
                snapshot_dir: snapshot_dir.clone(),
                version: "v1".to_string(),
            },
        );

        let pp_patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".clawhub".to_string(),
        )];
        let pp_ctrl = PostPublishGraceController::new(Duration::from_millis(grace_ms), pp_patterns);

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            active_resolver: Some(resolver),
            post_publish_controller: Some(pp_ctrl.clone()),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            config,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            post_publish_controller: pp_ctrl,
        }
    }

    fn skill_path(&self) -> PathBuf {
        self.mountpoint.path().join("skills/fallback-skill")
    }
}

impl Drop for FallbackGraceFixture {
    fn drop(&mut self) {
        self.post_publish_controller.shutdown();
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn post_publish_grace_fallback_snapshot_create_and_rename_to_source() {
    skip_if_no_fuse!();

    let fixture = FallbackGraceFixture::new(5000, |src, snapshot| {
        // Physical source has the skill with .clawhub
        let skill = src.join("fallback-skill");
        std::fs::create_dir_all(skill.join(".clawhub")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: test\n---\n",
        )
        .unwrap();

        // Snapshot does NOT contain .clawhub — only SKILL.md
        std::fs::write(
            snapshot.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: snapshot\n---\n",
        )
        .unwrap();
    });

    // Start grace session
    fixture
        .post_publish_controller
        .start_session("fallback-skill", PostPublishSessionKind::StagingRename);

    let skill_via_fuse = fixture.skill_path();

    // Create a temp file inside .clawhub via FUSE
    let tmp_path = skill_via_fuse.join(".clawhub/.fs-safe-replace-001.tmp");
    let result = std::fs::write(&tmp_path, r#"{"origin":"test"}"#);
    assert!(
        result.is_ok(),
        "create .clawhub/.fs-safe-replace-*.tmp must succeed during grace on fallback: {:?}",
        result.err()
    );

    // Rename to final path
    let final_path = skill_via_fuse.join(".clawhub/origin.json");
    let result = std::fs::rename(&tmp_path, &final_path);
    assert!(
        result.is_ok(),
        "rename within .clawhub/** must succeed during grace on fallback: {:?}",
        result.err()
    );

    // Verify the file is readable via FUSE
    let content = std::fs::read_to_string(&final_path).unwrap();
    assert_eq!(content, r#"{"origin":"test"}"#);

    // Verify it landed in physical source
    let phys_final = fixture
        .source
        .path()
        .join("fallback-skill/.clawhub/origin.json");
    assert!(
        phys_final.exists(),
        "final file must exist in physical source"
    );
}

#[test]
fn post_publish_grace_fallback_non_whitelisted_write_rejected() {
    skip_if_no_fuse!();

    let fixture = FallbackGraceFixture::new(5000, |src, snapshot| {
        let skill = src.join("fallback-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: test\n---\n",
        )
        .unwrap();
        // Put a non-whitelisted file in physical source only
        std::fs::create_dir_all(skill.join("data")).unwrap();
        std::fs::write(skill.join("data/secret.txt"), "secret").unwrap();

        std::fs::write(
            snapshot.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: snapshot\n---\n",
        )
        .unwrap();
    });

    fixture
        .post_publish_controller
        .start_session("fallback-skill", PostPublishSessionKind::StagingRename);

    let skill_via_fuse = fixture.skill_path();

    // Non-whitelisted write must fail (data/ is not in .clawhub/**)
    let result = std::fs::write(skill_via_fuse.join("data/injected.txt"), "bad");
    assert!(
        result.is_err(),
        "non-whitelisted write must fail during grace on fallback snapshot"
    );
}

#[test]
fn post_publish_grace_fallback_readdir_does_not_leak_source() {
    skip_if_no_fuse!();

    let fixture = FallbackGraceFixture::new(5000, |src, snapshot| {
        let skill = src.join("fallback-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: test\n---\n",
        )
        .unwrap();
        // Physical source has .clawhub and other dirs
        std::fs::create_dir_all(skill.join(".clawhub")).unwrap();
        std::fs::write(skill.join(".clawhub/origin.json"), "{}").unwrap();
        std::fs::create_dir_all(skill.join("extra")).unwrap();

        // Snapshot only has SKILL.md
        std::fs::write(
            snapshot.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: snapshot\n---\n",
        )
        .unwrap();
    });

    fixture
        .post_publish_controller
        .start_session("fallback-skill", PostPublishSessionKind::StagingRename);

    let skill_via_fuse = fixture.skill_path();

    // readdir on the skill directory should show snapshot contents,
    // NOT physical source contents — grace must not leak directory listing.
    let entries = common::list_dir_names(&skill_via_fuse);
    assert!(
        !entries.contains(&".clawhub".to_string()),
        "readdir must not expose .clawhub from physical source during grace: {:?}",
        entries
    );
    assert!(
        !entries.contains(&"extra".to_string()),
        "readdir must not expose extra/ from physical source during grace: {:?}",
        entries
    );
}

#[test]
fn post_publish_grace_fallback_skill_md_not_opened_via_grace() {
    skip_if_no_fuse!();

    let fixture = FallbackGraceFixture::new(5000, |src, snapshot| {
        let skill = src.join("fallback-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: live\n---\n",
        )
        .unwrap();
        std::fs::create_dir_all(skill.join(".clawhub")).unwrap();

        // Snapshot SKILL.md has different content
        std::fs::write(
            snapshot.join("SKILL.md"),
            "---\nname: fallback-skill\ndescription: snapshot\n---\n",
        )
        .unwrap();
    });

    fixture
        .post_publish_controller
        .start_session("fallback-skill", PostPublishSessionKind::StagingRename);

    let skill_via_fuse = fixture.skill_path();

    // SKILL.md must be served from the snapshot (compiled), not physical source.
    // The .clawhub/** pattern must NOT grant access to SKILL.md.
    let content = std::fs::read_to_string(skill_via_fuse.join("SKILL.md")).unwrap();
    assert!(
        content.contains("snapshot"),
        "SKILL.md must come from snapshot, not physical source: {}",
        content
    );
}
// ─────────────────────────────────────────────────────────────────────────────
// H3: Hermes installer compatibility tests
//
// Tests for installer transactions under Hermes layout mode: staging dirs
// inside categories, direct-write pending install with nested skill IDs,
// post-publish grace for nested skills, and management path noise regression.
// ─────────────────────────────────────────────────────────────────────────────

use skillfs_fuse::SkillLayout;

fn seed_hermes_workspace(dir: &Path) {
    std::fs::create_dir_all(dir.join(".hub")).unwrap();
    std::fs::write(dir.join(".hub/config.json"), r#"{"version": 1}"#).unwrap();
    std::fs::write(dir.join(".bundled_manifest"), "manifest-content").unwrap();

    let apple_notes = dir.join("apple/apple-notes");
    std::fs::create_dir_all(&apple_notes).unwrap();
    std::fs::write(
        apple_notes.join("SKILL.md"),
        "---\nname: apple-notes\ndescription: notes\n---\n",
    )
    .unwrap();
}

struct HermesStagingFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    #[allow(dead_code)]
    staging_controller: Arc<InstallerStagingController>,
}

impl HermesStagingFixture {
    fn new(seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));
        let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            staging_matcher: Some(matcher),
            staging_controller: Some(staging_ctrl.clone()),
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
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            staging_controller: staging_ctrl,
        }
    }

    fn mp(&self) -> &Path {
        self.mountpoint.path()
    }

    fn wait_for_notify(&self, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.notify_controller.flush_for_testing();
            if self.notify_client.len() >= expected {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {} notify events, got {}",
                    expected,
                    self.notify_client.len()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for HermesStagingFixture {
    fn drop(&mut self) {
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn hermes_staging_writes_no_notify() {
    skip_if_no_fuse!();

    let fix = HermesStagingFixture::new(|src| {
        seed_hermes_workspace(src);
    });

    let staging = fix.mp().join("apple/.openclaw-install-stage-new");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: new-skill\ndescription: test\n---\n",
    )
    .unwrap();
    std::fs::write(staging.join("data.txt"), "payload").unwrap();

    std::thread::sleep(Duration::from_millis(500));
    fix.notify_controller.flush_for_testing();

    let staging_events: Vec<_> = fix
        .notify_client
        .events()
        .iter()
        .filter(|e| e.skill_id.contains(".openclaw-install-stage-"))
        .cloned()
        .collect();
    assert!(
        staging_events.is_empty(),
        "Hermes staging writes must not trigger any notify: {:?}",
        staging_events
    );
}

#[test]
fn hermes_staging_rename_triggers_notify() {
    skip_if_no_fuse!();

    let fix = HermesStagingFixture::new(|src| {
        seed_hermes_workspace(src);
    });

    let staging = fix.mp().join("apple/.openclaw-install-stage-beta");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: beta\ndescription: installed\n---\n",
    )
    .unwrap();

    let final_path = fix.mp().join("apple/beta");
    std::fs::rename(&staging, &final_path).unwrap();

    fix.wait_for_notify(1);

    let events = fix.notify_client.events();
    let rename_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "apple/beta" && e.event_kind == "rename")
        .collect();
    assert_eq!(
        rename_events.len(),
        1,
        "expected one rename notify for apple/beta, got: {:?}",
        events
    );
    assert!(
        rename_events[0]
            .canonical_skill_dir
            .ends_with("/apple/beta"),
        "skillDir must end with /apple/beta, got: {}",
        rename_events[0].canonical_skill_dir
    );

    let staging_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id.contains(".openclaw-install-stage-"))
        .collect();
    assert!(
        staging_events.is_empty(),
        "no staging name events must exist: {:?}",
        staging_events
    );
}

#[test]
fn hermes_staging_hidden_from_category_listing() {
    skip_if_no_fuse!();

    let fix = HermesStagingFixture::new(|src| {
        seed_hermes_workspace(src);
        let staging = src.join("apple/.openclaw-install-stage-gamma");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("SKILL.md"), "---\nname: gamma\n---\n").unwrap();
    });

    let entries = common::list_dir_names(&fix.mp().join("apple"));
    assert!(
        entries.contains(&"apple-notes".to_string()),
        "existing skill must be visible: {:?}",
        entries
    );
    assert!(
        !entries.contains(&".openclaw-install-stage-gamma".to_string()),
        "staging root must NOT appear in category listing: {:?}",
        entries
    );
}

#[test]
fn hermes_staging_exact_path_accessible() {
    skip_if_no_fuse!();

    let fix = HermesStagingFixture::new(|src| {
        seed_hermes_workspace(src);
        let staging = src.join("apple/.openclaw-install-stage-delta");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("data.txt"), "payload").unwrap();
    });

    let staging = fix.mp().join("apple/.openclaw-install-stage-delta");
    let meta = std::fs::metadata(&staging);
    assert!(
        meta.is_ok(),
        "staging root must be accessible via exact path"
    );
    assert!(meta.unwrap().is_dir());

    let data = std::fs::read_to_string(staging.join("data.txt")).unwrap();
    assert_eq!(data, "payload");
}

// ─────────────────────────────────────────────────────────────────────────────
// H3: Hermes pending install tests
// ─────────────────────────────────────────────────────────────────────────────

struct HermesPendingFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_client: Arc<InMemoryNotifyClient>,
    notify_controller: Arc<NotifyController>,
    pending_controller: Arc<PendingInstallController>,
    #[allow(dead_code)]
    resolver: Arc<ActiveSkillResolver>,
}

impl HermesPendingFixture {
    fn new(pending_timeout_ms: u64, seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();
        let notify_client = Arc::new(InMemoryNotifyClient::new());

        let notify_ctrl = NotifyController::new(
            notify_client.clone(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        resolver.set(
            "apple/apple-notes",
            ActiveTarget::Current {
                source_dir: source.path().join("apple/apple-notes"),
            },
        );

        let pending_ctrl = PendingInstallController::new(
            notify_ctrl.clone(),
            Duration::from_millis(pending_timeout_ms),
            source.path().to_path_buf(),
        );

        let config = MountConfig {
            notify_controller: Some(notify_ctrl.clone()),
            active_resolver: Some(resolver.clone()),
            pending_install_controller: Some(pending_ctrl.clone()),
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
        .unwrap();

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_client,
            notify_controller: notify_ctrl,
            pending_controller: pending_ctrl,
            resolver,
        }
    }

    fn mp(&self) -> &Path {
        self.mountpoint.path()
    }
}

impl Drop for HermesPendingFixture {
    fn drop(&mut self) {
        self.pending_controller.shutdown();
        self.notify_controller.shutdown();
        if let Some(handle) = self.handle.take() {
            drop(handle);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

#[test]
fn hermes_pending_complete_notifies() {
    skip_if_no_fuse!();

    let fix = HermesPendingFixture::new(200, |src| {
        seed_hermes_workspace(src);
    });

    let new_skill = fix.mp().join("apple/new-skill");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(
        new_skill.join("SKILL.md"),
        "---\nname: new-skill\ndescription: test\n---\n",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(400));
    fix.pending_controller.flush_for_testing();
    fix.notify_controller.flush_for_testing();

    let events = fix.notify_client.events();
    let new_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "apple/new-skill")
        .collect();
    assert_eq!(
        new_events.len(),
        1,
        "complete Hermes pending install must trigger one notify: {:?}",
        events
    );
}

#[test]
fn hermes_pending_incomplete_no_notify() {
    skip_if_no_fuse!();

    let fix = HermesPendingFixture::new(150, |src| {
        seed_hermes_workspace(src);
    });

    let new_skill = fix.mp().join("apple/incomplete");
    std::fs::create_dir(&new_skill).unwrap();
    std::fs::write(new_skill.join("data.txt"), "no SKILL.md").unwrap();

    std::thread::sleep(Duration::from_millis(300));
    fix.pending_controller.flush_for_testing();
    fix.notify_controller.flush_for_testing();

    let events = fix.notify_client.events();
    let inc_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "apple/incomplete")
        .collect();
    assert!(
        inc_events.is_empty(),
        "incomplete Hermes pending install must not notify: {:?}",
        inc_events
    );
}

#[test]
fn hermes_pending_hidden_from_category_listing() {
    skip_if_no_fuse!();

    let fix = HermesPendingFixture::new(5000, |src| {
        seed_hermes_workspace(src);
    });

    let new_skill = fix.mp().join("apple/pending-hidden");
    std::fs::create_dir(&new_skill).unwrap();

    let entries = common::list_dir_names(&fix.mp().join("apple"));
    assert!(
        entries.contains(&"apple-notes".to_string()),
        "existing skill must be visible: {:?}",
        entries
    );
    assert!(
        !entries.contains(&"pending-hidden".to_string()),
        "pending skill must NOT appear in category listing: {:?}",
        entries
    );
}

#[test]
fn hermes_pending_exact_path_accessible() {
    skip_if_no_fuse!();

    let fix = HermesPendingFixture::new(5000, |src| {
        seed_hermes_workspace(src);
    });

    let new_skill = fix.mp().join("apple/pending-access");
    std::fs::create_dir(&new_skill).unwrap();

    let meta = std::fs::metadata(&new_skill);
    assert!(
        meta.is_ok(),
        "pending nested skill must be accessible via exact path"
    );
    assert!(meta.unwrap().is_dir());

    std::fs::write(new_skill.join("data.txt"), "test").unwrap();
    let content = std::fs::read_to_string(new_skill.join("data.txt")).unwrap();
    assert_eq!(content, "test");
}

// ─────────────────────────────────────────────────────────────────────────────
// H3: Hermes post-publish grace tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hermes_post_publish_grace_allows_whitelisted() {
    skip_if_no_fuse!();

    use skillfs_fuse::security::PostPublishWritePattern;

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();
    let notify_client = Arc::new(InMemoryNotifyClient::new());

    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::PrefixStar(
            ".openclaw-install-stage-".to_string(),
        )],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-notes"),
        },
    );

    let pp_patterns = vec![PostPublishWritePattern::PrefixRecursive(
        ".openclaw".to_string(),
    )];
    let pp_ctrl = PostPublishGraceController::new(Duration::from_millis(5000), pp_patterns);

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        staging_matcher: Some(matcher),
        staging_controller: Some(staging_ctrl),
        active_resolver: Some(resolver),
        post_publish_controller: Some(pp_ctrl.clone()),
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
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    let staging = mp.join("apple/.openclaw-install-stage-grace");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(
        staging.join("SKILL.md"),
        "---\nname: grace-test\ndescription: test\n---\n",
    )
    .unwrap();
    let final_path = mp.join("apple/grace-test");
    std::fs::rename(&staging, &final_path).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        notify_ctrl.flush_for_testing();
        if !notify_client.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for rename notify, events: {:?}",
                notify_client.events()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Grace: whitelisted .openclaw/** write must succeed.
    std::fs::create_dir(final_path.join(".openclaw")).unwrap();
    let result = std::fs::write(final_path.join(".openclaw/metadata.tmp"), "installer-data");
    assert!(
        result.is_ok(),
        "whitelisted .openclaw/** write must succeed during grace: {:?}",
        result.err()
    );

    // Non-whitelisted write must fail.
    let result = std::fs::create_dir(final_path.join("other-dir"));
    assert!(
        result.is_err(),
        "non-whitelisted path must be rejected during grace"
    );

    pp_ctrl.shutdown();
    notify_ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}
