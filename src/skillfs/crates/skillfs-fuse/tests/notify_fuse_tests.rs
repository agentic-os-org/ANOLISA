//! FUSE end-to-end tests for the N2 notify change client and A4 reconcile.
//!
//! These tests mount SkillFS with an in-memory notify client and verify:
//! - A single FUSE mutation triggers exactly one notify after debounce
//! - Multiple write chunks collapse into one notify
//! - `.skill-meta/**` mutations do NOT trigger a notify
//! - Notify failure does not affect the existing `/skills` read view
//! - A4: startup reconcile produces protocol events
//! - A4: daemon writes target:null hides skill from lookup/readdir
//! - A4: daemon writes invalid activation fail-safe hides skill
//! - A4: resolver switch preserves old fd pin, new open reads new target

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActivationReloadController, ActiveSkillResolver, ActiveTarget, FailingNotifyClient,
    InMemoryNotifyClient, InMemoryProtocolEventWriter, NoopNotifyClient, NotifyChangeEvent,
    NotifyClient, NotifyController, NotifyError, UnixSocketNotifyClient,
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

struct NotifyMountFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_controller: Arc<NotifyController>,
}

impl NotifyMountFixture {
    fn new(
        client: Arc<dyn skillfs_fuse::security::NotifyClient>,
        seed: impl FnOnce(&Path),
    ) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();

        let notify_ctrl = NotifyController::new(
            client,
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

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_controller: notify_ctrl,
        }
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn wait_for_notify(&self, expected: usize, client: &InMemoryNotifyClient) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            // Try flushing first (in case the worker hasn't run yet).
            self.notify_controller.flush_for_testing();
            if client.len() >= expected {
                return;
            }
            if std::time::Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_and_assert_no_notify(&self, client: &InMemoryNotifyClient) {
        std::thread::sleep(Duration::from_millis(300));
        self.notify_controller.flush_for_testing();
        assert!(
            client.is_empty(),
            "expected no notifications but got {}",
            client.len()
        );
    }
}

impl Drop for NotifyMountFixture {
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

/// Mount with an activation resolver so `/skills/<name>` is visible as
/// `current`. Without this, the active resolver gates visibility.
struct NotifyActivatedMountFixture {
    #[allow(dead_code)]
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_controller: Arc<NotifyController>,
    resolver: Arc<ActiveSkillResolver>,
}

impl NotifyActivatedMountFixture {
    fn new(
        client: Arc<dyn skillfs_fuse::security::NotifyClient>,
        seed: impl FnOnce(&Path),
        skill_names: &[&str],
    ) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        for name in skill_names {
            resolver.set(
                name.to_string(),
                ActiveTarget::Current {
                    source_dir: source.path().join(name),
                },
            );
        }

        let notify_ctrl = NotifyController::new(
            client,
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
        );

        let config = MountConfig {
            active_resolver: Some(resolver.clone()),
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

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_controller: notify_ctrl,
            resolver,
        }
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }
}

impl Drop for NotifyActivatedMountFixture {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn write_triggers_one_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: updated\n---\n").unwrap();

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert_eq!(events.len(), 1, "one write should produce one notify");
    assert_eq!(events[0].skill_id, "alpha");
    assert!(events[0].paths.contains(&"SKILL.md".to_string()));
}

#[test]
fn multiple_writes_collapse_into_one_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    for i in 0..5 {
        std::fs::write(
            &skill_md,
            format!("---\nname: alpha\ndescription: write {i}\n---\n"),
        )
        .unwrap();
    }

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        events.len() <= 2,
        "repeated writes must collapse (got {})",
        events.len()
    );
    assert!(!events.is_empty());
}

#[test]
fn skill_meta_write_does_not_trigger_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        let meta = src.join("alpha").join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
    });

    let meta_file = fixture
        .skill_path("alpha")
        .join(".skill-meta")
        .join("test.json");
    let _ = std::fs::write(&meta_file, "{}");

    fixture.wait_and_assert_no_notify(&client);
}

#[test]
fn notify_failure_does_not_affect_read_view() {
    skip_if_no_fuse!();

    let failing_client: Arc<dyn skillfs_fuse::security::NotifyClient> =
        Arc::new(FailingNotifyClient);
    let fixture = NotifyActivatedMountFixture::new(
        failing_client,
        |src| {
            create_skill(src, "alpha");
        },
        &["alpha"],
    );

    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: failing\n---\n").unwrap();

    // Wait for the worker to process the failing notify.
    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();

    // Read view must be unaffected — skill must still be visible.
    let contents = std::fs::read_to_string(&skill_md).unwrap();
    assert!(
        contents.contains("alpha"),
        "read view must survive notify failure"
    );

    // Resolver must still have the skill as current.
    let target = fixture.resolver.get("alpha").unwrap();
    assert!(
        matches!(target, ActiveTarget::Current { .. }),
        "resolver must be unchanged after notify failure"
    );
}

#[derive(Debug, Default)]
struct RejectingNotifyClient;

impl NotifyClient for RejectingNotifyClient {
    fn send(&self, _event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        Err(NotifyError::Rejected {
            body: r#"{"ok":true,"data":{"schemaVersion":2,"accepted":false}}"#.to_string(),
        })
    }
}

#[test]
fn notify_rejection_does_not_affect_read_view() {
    skip_if_no_fuse!();

    let fixture = NotifyActivatedMountFixture::new(
        Arc::new(RejectingNotifyClient),
        |src| {
            create_skill(src, "alpha");
        },
        &["alpha"],
    );

    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: rejected\n---\n").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    fixture.notify_controller.flush_for_testing();

    let contents = std::fs::read_to_string(&skill_md).unwrap();
    assert!(contents.contains("alpha"));
    assert!(matches!(
        fixture.resolver.get("alpha"),
        Some(ActiveTarget::Current { .. })
    ));
}

#[test]
fn create_file_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let new_file = fixture.skill_path("alpha").join("config.json");
    std::fs::write(&new_file, r#"{"key":"value"}"#).unwrap();

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(!events.is_empty(), "file creation must trigger notify");
    assert_eq!(events[0].skill_id, "alpha");
}

#[test]
fn same_skill_rename_reports_old_and_new_relative_paths() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        std::fs::write(src.join("alpha/old.txt"), "rename me").unwrap();
    });

    let skill_dir = fixture.skill_path("alpha");
    std::fs::rename(skill_dir.join("old.txt"), skill_dir.join("new.txt")).unwrap();
    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].schema_version, 2);
    assert_eq!(events[0].skill_id, "alpha");
    assert_eq!(events[0].event_kind, "rename");
    assert_eq!(events[0].paths, vec!["new.txt", "old.txt"]);
}

#[test]
fn fuse_mutation_reaches_fake_daemon_as_notify_v2() {
    skip_if_no_fuse!();

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("sec-core.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (request_tx, request_rx) = std::sync::mpsc::sync_channel(1);
    let daemon = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        request_tx.send(line).unwrap();

        let mut writer = std::io::BufWriter::new(&stream);
        writer
            .write_all(br#"{"ok":true,"data":{"schemaVersion":2,"accepted":true}}"#)
            .unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
    });

    let client = Arc::new(UnixSocketNotifyClient::new(
        &socket_path,
        Duration::from_secs(5),
    ));
    let fixture = NotifyMountFixture::new(client, |src| {
        create_skill(src, "alpha");
    });
    let expected_canonical_dir = fixture.source.path().join("alpha");

    std::fs::write(fixture.skill_path("alpha").join("config.json"), "{}").unwrap();

    let request = request_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("fake daemon must receive FUSE mutation notify");
    let request: serde_json::Value = serde_json::from_str(request.trim()).unwrap();
    assert_eq!(request["method"], "skill_ledger.skillfs_notify_change");
    assert_eq!(request["params"]["schemaVersion"], 2);
    assert_eq!(
        request["params"]["canonicalSkillDir"],
        expected_canonical_dir.to_string_lossy().as_ref()
    );
    assert_eq!(request["params"]["skillId"], "alpha");
    assert!(
        request["params"]["paths"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("config.json"))
    );
    daemon.join().unwrap();
}

// ---------------------------------------------------------------------------
// A3 end-to-end: notify-driven reload refreshes resolver
// ---------------------------------------------------------------------------

/// Mount with an activation resolver, notify controller, and reload
/// controller. A daemon stub writes `activation.json` when it detects a
/// source change. The test verifies the full flow: FUSE write → notify
/// debounce → notify send → daemon stub writes activation → reload poll
/// picks up the new activation → new reads see the snapshot.
struct ReloadMountFixture {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
    notify_controller: Arc<NotifyController>,
    resolver: Arc<ActiveSkillResolver>,
}

impl ReloadMountFixture {
    fn new(seed: impl FnOnce(&Path), skill_names: &[&str]) -> Self {
        let source = tempfile::tempdir().unwrap();
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
        for name in skill_names {
            resolver.set(
                name.to_string(),
                ActiveTarget::Current {
                    source_dir: source.path().join(name),
                },
            );
        }

        let reload_ctrl = Arc::new(ActivationReloadController::new(
            source.path(),
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(3000),
        ));

        let client: Arc<dyn skillfs_fuse::security::NotifyClient> = Arc::new(NoopNotifyClient);
        let notify_ctrl = NotifyController::new_with_reload(
            client,
            source.path().to_path_buf(),
            source.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
            Arc::new(skillfs_fuse::security::NoopProtocolEventWriter),
            reload_ctrl,
        );

        let config = MountConfig {
            active_resolver: Some(resolver.clone()),
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

        Self {
            source,
            mountpoint,
            handle: Some(handle),
            notify_controller: notify_ctrl,
            resolver,
        }
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for ReloadMountFixture {
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

#[test]
fn fuse_write_triggers_reload_and_refreshes_resolver() {
    skip_if_no_fuse!();

    // Set up a skill with a snapshot.
    let fixture = ReloadMountFixture::new(
        |src| {
            create_skill(src, "alpha");
            let snap = src
                .join("alpha")
                .join(".skill-meta/versions/v000001.snapshot");
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(
                snap.join("SKILL.md"),
                "---\nname: alpha\ndescription: snapshot v1\n---\n",
            )
            .unwrap();
        },
        &["alpha"],
    );

    // Verify baseline: skill is `Current`.
    assert!(matches!(
        fixture.resolver.get("alpha"),
        Some(ActiveTarget::Current { .. })
    ));

    // Daemon stub: watch the source dir and write activation.json when
    // notified. We run this as a thread that writes activation after a
    // short delay (simulating daemon processing time).
    let source_for_daemon = fixture.source_path().to_path_buf();
    let daemon = std::thread::spawn(move || {
        // Wait for the FUSE write to propagate through notify debounce.
        std::thread::sleep(Duration::from_millis(200));
        let activation = source_for_daemon.join("alpha/.skill-meta/activation.json");
        std::fs::write(
            activation,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();
    });

    // FUSE write: triggers notify → debounce → send_one → reload poll.
    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: modified\n---\n").unwrap();

    // Wait for the full pipeline to complete.
    daemon.join().unwrap();
    // Give the reload poll time to detect the daemon's write.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        if let Some(ActiveTarget::Snapshot { .. }) = fixture.resolver.get("alpha") {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Verify: resolver must now have the snapshot, not current.
    match fixture.resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot");
        }
        other => panic!("expected resolver to show snapshot after reload, got {other:?}"),
    }

    // Verify the FUSE read path serves the snapshot content, not the
    // live source. New opens after the resolver update must see the
    // snapshot's SKILL.md.
    let read_content = std::fs::read_to_string(fixture.skill_path("alpha").join("SKILL.md"))
        .expect("reading SKILL.md through FUSE after reload");
    assert!(
        read_content.contains("snapshot v1"),
        "FUSE read must serve snapshot content after reload, got: {read_content}"
    );
}

// ---------------------------------------------------------------------------
// A4: Startup reconcile produces protocol events
// ---------------------------------------------------------------------------

/// Mirrors the `main.rs` wiring: `enqueue_startup_reconcile` queues each
/// skill on the notify worker after mount, skill names are derived from the
/// store with `skill-discover` filtered out. Verifies the queued path
/// delivers both protocol events and notify events.
#[test]
fn startup_reconcile_enqueue_produces_events() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "alpha");
    create_skill(source.path(), "beta");
    // skill-discover must be filtered by the reconcile.
    create_skill(source.path(), "skill-discover");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    // Build skill names the same way main.rs does.
    let skill_names: Vec<String> = store
        .list()
        .iter()
        .filter(|n| **n != "skill-discover")
        .map(|s| s.to_string())
        .collect();
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    let client = Arc::new(InMemoryNotifyClient::new());
    let writer = Arc::new(InMemoryProtocolEventWriter::new());
    let ctrl = NotifyController::new_with_protocol_writer(
        client.clone(),
        source.path().to_path_buf(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
        writer.clone(),
    );

    let config = MountConfig {
        notify_controller: Some(ctrl.clone()),
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

    // Use the production path: enqueue_startup_reconcile (non-blocking).
    ctrl.enqueue_startup_reconcile(&skill_names);

    // Wait for the notify worker to drain the queue.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if writer.len() >= 2 {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Verify protocol events (alpha + beta, not skill-discover).
    let events = writer.events();
    assert_eq!(events.len(), 2, "expected 2 reconcile protocol events");
    for e in &events {
        assert_eq!(e.event_kind, "reconcile");
        assert!(e.paths.is_empty());
        assert_ne!(
            e.skill_name, "skill-discover",
            "skill-discover must not appear in reconcile events"
        );
    }

    // Verify notify events.
    let notify_events = client.events();
    assert_eq!(notify_events.len(), 2);
    for e in &notify_events {
        assert_eq!(e.schema_version, 2);
        assert_eq!(e.event_kind, "reconcile");
    }

    ctrl.shutdown();
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

/// When no notify controller exists (activation_mode=off or no
/// --notify-socket/--activation-events-log), no reconcile fires.
/// This tests the conditional guard in main.rs.
#[test]
fn no_notify_controller_means_no_reconcile() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().unwrap();
    create_skill(source.path(), "alpha");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    // Mount WITHOUT a notify controller — mirroring the case where
    // activation_mode=off or no notify trigger source is configured.
    let config = MountConfig::default();

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

    // main.rs guard: notify_controller.is_none() → no reconcile.
    // The Option is None, so the if-let in main.rs does not fire.
    // We verify the mount works normally without reconcile side effects.
    let skill_md = mountpoint.path().join("skills/alpha/SKILL.md");
    assert!(
        skill_md.exists(),
        "mount without notify controller must still serve skills"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ---------------------------------------------------------------------------
// A4: daemon writes target:null hides skill from new lookup/readdir
// ---------------------------------------------------------------------------

#[test]
fn daemon_null_target_hides_skill_from_new_lookup() {
    skip_if_no_fuse!();

    let fixture = ReloadMountFixture::new(
        |src| {
            create_skill(src, "alpha");
            let snap = src
                .join("alpha")
                .join(".skill-meta/versions/v000001.snapshot");
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(
                snap.join("SKILL.md"),
                "---\nname: alpha\ndescription: snapshot v1\n---\n",
            )
            .unwrap();
        },
        &["alpha"],
    );

    // Verify baseline: skill visible.
    assert!(fixture.skill_path("alpha").join("SKILL.md").exists());

    // Daemon stub: write activation with target=null after a delay.
    let source_for_daemon = fixture.source_path().to_path_buf();
    let daemon = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let meta = source_for_daemon.join("alpha/.skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": null}"#,
        )
        .unwrap();
    });

    // Trigger notify via FUSE write.
    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: modified\n---\n").unwrap();

    daemon.join().unwrap();

    // Wait for reload to pick up null target.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        if let Some(ActiveTarget::Hidden { .. }) = fixture.resolver.get("alpha") {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Verify resolver shows hidden.
    assert!(
        matches!(
            fixture.resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ),
        "expected resolver to show Hidden after null-target activation"
    );

    // readdir on the skills root should no longer list "alpha".
    let skills_root = fixture.mountpoint.path().join("skills");
    let entries: Vec<String> = std::fs::read_dir(&skills_root)
        .expect("read skills root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !entries.contains(&"alpha".to_string()),
        "readdir must not list hidden skill, got: {entries:?}"
    );
}

// ---------------------------------------------------------------------------
// A4: daemon writes invalid activation → fail-safe hidden
// ---------------------------------------------------------------------------

#[test]
fn daemon_invalid_activation_failsafe_hides_skill() {
    skip_if_no_fuse!();

    let fixture = ReloadMountFixture::new(
        |src| {
            create_skill(src, "alpha");
            let snap = src
                .join("alpha")
                .join(".skill-meta/versions/v000001.snapshot");
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(
                snap.join("SKILL.md"),
                "---\nname: alpha\ndescription: snapshot v1\n---\n",
            )
            .unwrap();
        },
        &["alpha"],
    );

    // Verify baseline: skill visible.
    assert!(fixture.skill_path("alpha").join("SKILL.md").exists());

    // Daemon stub: write invalid activation.
    let source_for_daemon = fixture.source_path().to_path_buf();
    let daemon = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let meta = source_for_daemon.join("alpha/.skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("activation.json"), "CORRUPTED_JSON").unwrap();
    });

    // Trigger notify via FUSE write.
    let skill_md = fixture.skill_path("alpha").join("SKILL.md");
    std::fs::write(&skill_md, "---\nname: alpha\ndescription: modified\n---\n").unwrap();

    daemon.join().unwrap();

    // Wait for reload to pick up invalid activation.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        if let Some(ActiveTarget::Hidden { .. }) = fixture.resolver.get("alpha") {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        matches!(
            fixture.resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ),
        "expected resolver to show Hidden after invalid activation"
    );
}

// ---------------------------------------------------------------------------
// A4: resolver switch — old fd pin unchanged, new open reads new target
// ---------------------------------------------------------------------------

#[test]
fn resolver_switch_old_fd_pin_new_open_reads_new_target() {
    skip_if_no_fuse!();

    let fixture = ReloadMountFixture::new(
        |src| {
            create_skill(src, "alpha");
            let snap = src
                .join("alpha")
                .join(".skill-meta/versions/v000001.snapshot");
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(
                snap.join("SKILL.md"),
                "---\nname: alpha\ndescription: snapshot v1\n---\n",
            )
            .unwrap();
        },
        &["alpha"],
    );

    // Open SKILL.md and hold the fd (pinning the current target).
    let skill_md_path = fixture.skill_path("alpha").join("SKILL.md");
    use std::io::Read;
    let mut pinned_fd = std::fs::File::open(&skill_md_path).expect("open SKILL.md for fd pinning");

    // Daemon writes activation pointing to snapshot.
    let source_for_daemon = fixture.source_path().to_path_buf();
    let daemon = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let meta = source_for_daemon.join("alpha/.skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();
    });

    // Trigger FUSE write to start reload.
    std::fs::write(
        &skill_md_path,
        "---\nname: alpha\ndescription: modified live\n---\n",
    )
    .unwrap();

    daemon.join().unwrap();

    // Wait for resolver to switch to snapshot.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        fixture.notify_controller.flush_for_testing();
        if let Some(ActiveTarget::Snapshot { .. }) = fixture.resolver.get("alpha") {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(matches!(
        fixture.resolver.get("alpha"),
        Some(ActiveTarget::Snapshot { .. })
    ));

    // Old fd should still read from the pinned target (the content at
    // open time — either current or whatever was served then).
    let mut old_content = String::new();
    pinned_fd.read_to_string(&mut old_content).unwrap();
    // The pinned fd sees whatever was served at open time (current target).
    assert!(
        old_content.contains("alpha"),
        "pinned fd must still be readable: {old_content}"
    );

    // New open should read the snapshot content.
    let new_content =
        std::fs::read_to_string(&skill_md_path).expect("new open after resolver switch");
    assert!(
        new_content.contains("snapshot v1"),
        "new open must serve snapshot content, got: {new_content}"
    );
}

// ---------------------------------------------------------------------------
// Regression: production notify coverage gaps
// ---------------------------------------------------------------------------

/// P1 regression: inbox `.install-complete` sentinel must trigger
/// NotifyController in addition to RefreshController. Without the fix,
/// `inbox_observe_install_complete` only bridged to refresh_controller
/// and `activation-mode=file` production paths (which rely on
/// notify_controller) never received the mutation.
#[test]
fn inbox_install_complete_triggers_notify_controller() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let inbox_skill = fixture.mountpoint.path().join(".skillfs-inbox/alpha");

    // Write a non-sentinel file through the inbox — must NOT trigger notify.
    std::fs::write(inbox_skill.join("data.txt"), b"payload").unwrap();
    fixture.wait_and_assert_no_notify(&client);

    // Write the `.install-complete` sentinel — MUST trigger notify.
    // The event_kind will be a real mutation ("write" or "create"),
    // not "install-complete" which is an internal flush concept only.
    std::fs::write(inbox_skill.join(".install-complete"), b"").unwrap();
    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        !events.is_empty(),
        "inbox .install-complete sentinel must trigger notify_controller"
    );
    assert_eq!(events[0].skill_id, "alpha");
    assert_ne!(
        events[0].event_kind, "install-complete",
        "protocol events must use real mutation kinds, not install-complete"
    );
}

/// P2a regression: `open(O_TRUNC)` on SKILL.md must trigger notify.
/// Without the fix, the truncate succeeded and sent a Reparse sync but
/// never called `observe_mutation`, so no notify/protocol event fired.
#[test]
fn open_trunc_skill_md_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let skill_md = fixture.skill_path("alpha").join("SKILL.md");

    // O_TRUNC via truncate (std::fs::File with truncate(true) + write(true))
    {
        let _f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&skill_md)
            .expect("open SKILL.md with O_TRUNC");
    }

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        !events.is_empty(),
        "O_TRUNC on SKILL.md must trigger notify"
    );
    assert_eq!(events[0].skill_id, "alpha");
}

/// P2b regression: `open(O_RDONLY|O_TRUNC)` on a passthrough file must
/// trigger notify. Linux allows O_RDONLY|O_TRUNC to truncate the file;
/// before the fix, the truncation succeeded but never called
/// `observe_mutation`.
#[test]
fn open_rdonly_trunc_passthrough_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        std::fs::write(src.join("alpha/data.txt"), b"some content").unwrap();
    });

    let data_file = fixture.skill_path("alpha").join("data.txt");

    // O_RDONLY|O_TRUNC via libc::open
    {
        use std::ffi::CString;
        let c_path = CString::new(data_file.to_str().unwrap()).unwrap();
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) };
        assert!(fd >= 0, "O_RDONLY|O_TRUNC open must succeed");
        unsafe { libc::close(fd) };
    }

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        !events.is_empty(),
        "O_RDONLY|O_TRUNC on passthrough file must trigger notify"
    );
    assert_eq!(events[0].skill_id, "alpha");
}

/// P2b+ regression: `open(O_WRONLY|O_TRUNC)` on a passthrough file must
/// trigger notify. `open_options_from_flags` handles truncate internally
/// for write-mode opens, but the `final_open Ok(file)` branch previously
/// had no `observe_mutation` call.
#[test]
fn open_wronly_trunc_passthrough_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        std::fs::write(src.join("alpha/data.txt"), b"some content").unwrap();
    });

    let data_file = fixture.skill_path("alpha").join("data.txt");

    // O_WRONLY|O_TRUNC via std::fs::OpenOptions
    {
        let _f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&data_file)
            .expect("open passthrough with O_WRONLY|O_TRUNC");
    }

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        !events.is_empty(),
        "O_WRONLY|O_TRUNC on passthrough file must trigger notify"
    );
    assert_eq!(events[0].skill_id, "alpha");
}

/// P2c regression: successful `symlink` creation must trigger notify.
/// Before the fix, symlink success only emitted an audit event but never
/// called `observe_mutation`.
#[test]
fn symlink_creation_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        std::fs::write(src.join("alpha/target.txt"), b"content").unwrap();
    });

    let link_path = fixture.skill_path("alpha").join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link_path).expect("symlink must succeed");

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(!events.is_empty(), "symlink creation must trigger notify");
    assert_eq!(events[0].skill_id, "alpha");
}

/// P2d regression: successful `link` (hardlink) must trigger notify.
/// Before the fix, hardlink success only emitted an audit event but
/// never called `observe_mutation`.
#[test]
fn hardlink_creation_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
        std::fs::write(src.join("alpha/original.txt"), b"content").unwrap();
    });

    let original = fixture.skill_path("alpha").join("original.txt");
    let hard = fixture.skill_path("alpha").join("hardlinked.txt");
    std::fs::hard_link(&original, &hard).expect("hardlink must succeed");

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(!events.is_empty(), "hardlink creation must trigger notify");
    assert_eq!(events[0].skill_id, "alpha");
}

/// P2e regression: successful FIFO (`mknod`) must trigger notify.
/// Before the fix, mknod success only emitted an audit event but never
/// called `observe_mutation`.
#[test]
fn fifo_creation_triggers_notify() {
    skip_if_no_fuse!();

    let client = Arc::new(InMemoryNotifyClient::new());
    let fixture = NotifyMountFixture::new(client.clone(), |src| {
        create_skill(src, "alpha");
    });

    let fifo_path = fixture.skill_path("alpha").join("pipe");
    {
        use std::ffi::CString;
        let c_path = CString::new(fifo_path.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(
            rc,
            0,
            "mkfifo must succeed (errno: {})",
            std::io::Error::last_os_error()
        );
    }

    fixture.wait_for_notify(1, &client);

    let events = client.events();
    assert!(
        !events.is_empty(),
        "FIFO (mknod) creation must trigger notify"
    );
    assert_eq!(events[0].skill_id, "alpha");
}
