//! Integration tests: scan-loaded staging content stays out of the
//! ordinary skill views.
//!
//! The staging contract (I2) keeps installer staging roots private: they
//! are "hidden from `/skills` readdir/opendir" and "must not participate
//! in ... skill-discover". The store loader enforces no staging rule of
//! its own — it only skips dot-prefixed top-level directories — and the
//! staging validator accepts non-dot patterns (it rejects only sensitive
//! namespaces, `/`, `..` and stray `*`). A staging root pre-seeded on
//! disk (an installer that stages before the mount, or the residue of a
//! crashed install) is therefore classified as a category and its nested
//! skills are scanned into the store under their bare leaf names. The
//! name-level staging filters in the `/skills` listing and the
//! `skill-discover` simple listing then see only the leaf, and the
//! staged half-install surfaces in the ordinary views.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::parser;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, FailedResolveBehavior, LedgerAdapter, LedgerResolveResult,
    RefreshController, SecurityEvent, SecurityEventWriter, StagingConfig, StagingMatcher,
    StagingPattern, StaticLedgerAdapter,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

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

fn write_manifest_skill(dir: &Path, name: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test\n---\nbody\n"),
    )
    .expect("write SKILL.md");
}

/// Mounts a flat workspace with a non-dot `Exact("pip-staging")` staging
/// pattern and a pre-seeded staging category on disk.
struct StagingScanMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    shared: SharedSkillStore,
    handle: Option<MountHandle>,
}

impl StagingScanMount {
    fn new(seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");

        let staging_config = StagingConfig {
            patterns: vec![StagingPattern::Exact("pip-staging".to_string())],
            ..StagingConfig::default()
        };
        let matcher = Arc::new(StagingMatcher::new(staging_config));

        let config = MountConfig {
            staging_matcher: Some(matcher),
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared.clone(),
            MountOptions::default(),
            false,
            config,
        )
        .expect("mount flat workspace with exact staging pattern");

        Self {
            source,
            mountpoint,
            shared,
            handle: Some(handle),
        }
    }

    fn mount_path(&self, rel: &str) -> PathBuf {
        self.mountpoint.path().join(rel)
    }
}

impl Drop for StagingScanMount {
    fn drop(&mut self) {
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

/// The on-disk shape under test: a real flat skill, a real categorized
/// skill, and an installer staging category pre-seeded with a
/// half-installed skill.
fn seed_workspace_with_category(src: &Path, staging_root: &str) {
    write_manifest_skill(src, "zeta");
    let alpha = src.join("alpha");
    std::fs::create_dir_all(&alpha).expect("create category");
    write_manifest_skill(&alpha, "notes");
    // The staging root: no SKILL.md of its own, a nested skill inside —
    // exactly the category shape the loader scans into the store.
    let staging = src.join(staging_root);
    std::fs::create_dir_all(&staging).expect("create staging root");
    write_manifest_skill(&staging, "web");
    std::fs::write(staging.join("notes.txt"), "staging\n").expect("probe file");
}

fn seed_workspace(src: &Path) {
    seed_workspace_with_category(src, "pip-staging");
}

fn list_dir_names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .expect("read dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// A pre-seeded staging category's nested skills must stay out of the
/// ordinary `/skills` listing: staging roots are installer-private
/// workspaces, not skills. The staging root itself is already hidden by
/// the name-level filter; its scanned content must not surface under
/// the bare leaf name either.
#[test]
fn scan_loaded_staging_content_stays_out_of_the_skills_listing() {
    skip_if_no_fuse!();

    let m = StagingScanMount::new(seed_workspace);
    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(m.mount_path("skills/zeta/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/zeta/SKILL.md"
    );

    let entries = list_dir_names(&m.mount_path("skills"));
    assert!(
        !entries.iter().any(|n| n == "web"),
        "a skill staged inside a pre-seeded staging root must not appear in /skills, got: {entries:?}"
    );
    assert!(
        !entries.iter().any(|n| n == "pip-staging"),
        "the staging root itself must stay hidden from /skills, got: {entries:?}"
    );
    // Reverse: ordinary flat and categorized skills stay listed.
    assert!(
        entries.iter().any(|n| n == "zeta"),
        "an ordinary flat skill must stay listed, got: {entries:?}"
    );
    assert!(
        entries.iter().any(|n| n == "notes"),
        "an ordinary categorized skill must stay listed, got: {entries:?}"
    );
}

/// `skill-discover` broadcasts the store listing to every consumer; the
/// I2 contract keeps staging roots out of it. The staged half-install
/// must not be advertised there, while ordinary skills stay listed.
#[test]
fn scan_loaded_staging_content_stays_out_of_skill_discover() {
    skip_if_no_fuse!();

    let m = StagingScanMount::new(seed_workspace);
    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(m.mount_path("skills/zeta/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/zeta/SKILL.md"
    );

    let discover =
        std::fs::read_to_string(m.mount_path("skills/skill-discover/SKILL.md")).expect("discover");
    assert!(
        !discover.contains("| web |"),
        "a skill staged inside a pre-seeded staging root must not be broadcast by skill-discover, got:\n{discover}"
    );
    assert!(
        discover.contains("| zeta |"),
        "an ordinary flat skill must stay discoverable, got:\n{discover}"
    );
    assert!(
        discover.contains("| notes |"),
        "an ordinary categorized skill must stay discoverable, got:\n{discover}"
    );
}

/// kongche-jbw follow-up on #7742 (interaction with the merged #8037):
/// the reparse refresh of a pre-seeded staging category's manifest goes
/// through `upsert_checked`, which used to insert with an empty category
/// and so wiped the scanned category record — the staging-private view
/// filters match that record, so after an exact flat-path edit of
/// `/skills/web/SKILL.md` (a path the observe dispatcher does not drop:
/// its staging check sees the bare leaf `web`, not the staging root) the
/// refresh re-parsed the manifest and the staged skill became listable
/// and discoverable again until the next remount. The refresh must keep
/// the category provenance.
#[test]
fn flat_manifest_edit_refresh_keeps_the_staging_category_hidden() {
    skip_if_no_fuse!();

    let m = StagingScanMount::new(seed_workspace);
    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(m.mount_path("skills/zeta/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/zeta/SKILL.md"
    );

    // Precondition (#8037): the staged half-install is out of both views.
    let entries_before = list_dir_names(&m.mount_path("skills"));
    assert!(
        !entries_before.iter().any(|n| n == "web"),
        "precondition: the staged skill must start hidden from /skills, got: {entries_before:?}"
    );
    let discover_before = std::fs::read_to_string(m.mount_path("skills/skill-discover/SKILL.md"))
        .expect("discover before the edit");
    assert!(
        !discover_before.contains("| web |"),
        "precondition: the staged skill must start hidden from skill-discover, got:\n{discover_before}"
    );

    // Exact flat-path edit: the flat /skills/web slot resolves to the
    // stored entry's manifest (pip-staging/web/SKILL.md), and the
    // observe dispatcher's staging check matches the bare leaf only, so
    // the edit reaches the reparse worker.
    let web_md = m.mount_path("skills/web/SKILL.md");
    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(&web_md)
        .expect("open the staged manifest through the flat mount");
    use std::io::Write as _;
    md.write_all(b"refreshed through the flat mount\n")
        .expect("append the staged manifest through the flat mount");
    drop(md);

    // The refresh ran once the store entry carries the appended body.
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.shared
                .read()
                .get("web")
                .is_some_and(|e| e.body.contains("refreshed through the flat mount"))
        }),
        "the flat-path manifest edit must reach the reparse worker, body is {:?}",
        m.shared.read().get("web").map(|e| e.body.clone())
    );

    // Both views must keep hiding the staged skill after the refresh.
    let entries = list_dir_names(&m.mount_path("skills"));
    assert!(
        !entries.iter().any(|n| n == "web"),
        "a refreshed staging entry must stay out of /skills, got: {entries:?}"
    );
    assert!(
        entries.iter().any(|n| n == "zeta"),
        "an ordinary flat skill must stay listed, got: {entries:?}"
    );
    let discover = std::fs::read_to_string(m.mount_path("skills/skill-discover/SKILL.md"))
        .expect("discover after the edit");
    assert!(
        !discover.contains("| web |"),
        "a refreshed staging entry must stay out of skill-discover, got:\n{discover}"
    );
    assert!(
        discover.contains("| zeta |"),
        "an ordinary flat skill must stay discoverable, got:\n{discover}"
    );
}

/// The same scan-loaded leak through a non-dot `PrefixStar` staging
/// pattern: the pattern names the top-level staging root, the store key
/// is the nested skill's bare leaf, so the name-level filters miss it.
#[test]
fn prefix_star_staging_content_stays_out_of_the_skill_views() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().expect("source tempdir");
    seed_workspace_with_category(source.path(), "build-tmp");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::PrefixStar("build-".to_string())],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let config = MountConfig {
        staging_matcher: Some(matcher),
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
    .expect("mount flat workspace with prefix-star staging pattern");

    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(mountpoint.path().join("skills/zeta/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/zeta/SKILL.md"
    );

    let entries = list_dir_names(&mountpoint.path().join("skills"));
    assert!(
        !entries.iter().any(|n| n == "web"),
        "a skill staged inside a pre-seeded staging category must not appear in /skills, got: {entries:?}"
    );
    let discover =
        std::fs::read_to_string(mountpoint.path().join("skills/skill-discover/SKILL.md"))
            .expect("discover");
    assert!(
        !discover.contains("| web |"),
        "a skill staged inside a pre-seeded staging category must not be broadcast by skill-discover, got:\n{discover}"
    );

    drop(handle);
    let mp = mountpoint.path().to_path_buf();
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mp.to_string_lossy()])
        .output();
}

/// Discover rendering must not stall behind a queued store writer.
///
/// `get_skill_discover_content` holds the store read guard across the
/// whole render. The staging predicate used to acquire that same read
/// lock a second time for every listed name; parking_lot's RwLock is
/// fair, so once a writer queues between the two acquisitions — the
/// background sync worker's `store.write().upsert` reparse refresh is
/// exactly such a writer — the nested read blocks behind the queued
/// writer while the writer waits for the outer read to drop, and
/// reading or statting the discover manifest stalls FUSE request
/// processing for good. Render discover through a real configured
/// mount while a writer refreshes an ordinary skill through the same
/// shared store the sync worker uses; every read must complete.
#[test]
fn discover_rendering_survives_a_queued_store_writer() {
    skip_if_no_fuse!();

    // A wide ordinary store stretches the render — the outer guard is
    // held for the whole listing, giving the writer a real window to
    // queue behind it — while the pre-seeded staging category keeps
    // the render on the per-name category lookup path.
    let m = StagingScanMount::new(|src| {
        seed_workspace_with_category(src, "pip-staging");
        for i in 0..40 {
            write_manifest_skill(src, &format!("ord-{i:02}"));
        }
    });
    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(m.mount_path("skills/zeta/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/zeta/SKILL.md"
    );

    // The writer mirrors the sync worker's refresh: re-parse an
    // ordinary skill, then take the write lock just to upsert it.
    let zeta_md = m.source.path().join("zeta/SKILL.md");
    let writer_store = m.shared.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_writer = stop.clone();
    let writer = std::thread::spawn(move || {
        while !stop_writer.load(Ordering::Relaxed) {
            let entry = parser::parse_skill_file(&zeta_md).expect("re-parse zeta");
            writer_store.write().upsert(entry);
        }
    });

    let discover_path = m.mount_path("skills/skill-discover/SKILL.md");
    let (done_tx, done_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for read in 0..500u32 {
            let content = std::fs::read_to_string(&discover_path)
                .unwrap_or_else(|e| panic!("discover read {read} failed: {e}"));
            assert!(
                content.contains("| zeta |"),
                "discover render lost the ordinary zeta skill on read {read}"
            );
            if read + 1 == 500 {
                done_tx.send(()).expect("report completion");
            }
        }
    });

    match done_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => {
            stop.store(true, Ordering::Relaxed);
            writer.join().expect("writer thread panicked");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::Relaxed);
            panic!(
                "reading the discover manifest stalled behind a queued store \
                 writer: the render held the read guard and re-acquired the \
                 lock behind the writer"
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let payload = reader.join().expect_err("reader must have panicked");
            std::panic::resume_unwind(payload);
        }
    }
}

/// A ledger result writer that swallows events; the install flow under
/// test only needs the scan -> resolve side effects.
struct NoopEventWriter;

impl SecurityEventWriter for NoopEventWriter {
    fn emit(&self, _event: &SecurityEvent) {}
}

fn current_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "pass",
            "decision": "current",
            "currentVersion": "v000001",
            "trustedVersion": "v000001"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("current json")
}

/// An ordinary skill installed over a pre-scanned staging leaf name
/// must surface in both filtered views once it is the live entry.
///
/// `SkillStore::upsert` replaces the entry but used to leave the
/// category record of the entry it replaced behind: seed
/// `pip-staging/web/SKILL.md`, mount with the `pip-staging` Exact
/// pattern, then install the ordinary `web` skill through the inbox —
/// the mkdir/write path replaces the store entry with the completed
/// `source/web` skill, but `category_of("web")` kept reporting
/// `pip-staging`, so both new listing filters hid the ordinary skill
/// until remount, even though it was installed, resolved `current`
/// and directly readable.
#[test]
fn inbox_install_over_staging_leaf_name_becomes_visible() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().expect("source tempdir");
    write_manifest_skill(source.path(), "anchor");
    let staging = source.path().join("pip-staging");
    std::fs::create_dir_all(&staging).expect("create staging root");
    write_manifest_skill(&staging, "web");

    let adapter = StaticLedgerAdapter::new();
    adapter.insert("web", current_result("web"));
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        Arc::new(NoopEventWriter),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::Exact("pip-staging".to_string())],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let config = MountConfig {
        staging_matcher: Some(matcher),
        active_resolver: Some(resolver.clone()),
        refresh_controller: Some(ctrl.clone()),
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
    .expect("mount with inbox wiring and exact staging pattern");

    assert!(
        wait_for(Duration::from_secs(30), || {
            std::fs::read_to_string(mountpoint.path().join("skills/anchor/SKILL.md")).is_ok()
        }),
        "flat mount never served skills/anchor/SKILL.md"
    );

    // Pre-state: the staged half-install is hidden from both surfaces.
    let entries_before = list_dir_names(&mountpoint.path().join("skills"));
    assert!(
        !entries_before.iter().any(|n| n == "web"),
        "the pre-scanned staging half-install must stay hidden from /skills, got: {entries_before:?}"
    );
    let discover_before =
        std::fs::read_to_string(mountpoint.path().join("skills/skill-discover/SKILL.md"))
            .expect("discover before install");
    assert!(
        !discover_before.contains("| web |"),
        "the pre-scanned staging half-install must stay out of skill-discover, got:\n{discover_before}"
    );

    // Install the ordinary `web` skill through the inbox.
    let inbox_skill = mountpoint.path().join(".skillfs-inbox/web");
    std::fs::create_dir(&inbox_skill).expect("mkdir inbox web");
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: web\ndescription: ordinary installed skill\n---\nbody\n",
    )
    .expect("write SKILL.md through inbox");
    std::fs::write(inbox_skill.join(".install-complete"), b"")
        .expect("write install-complete sentinel");

    // The resolver flips to `current`: the completed skill is directly
    // readable. Both filtered surfaces must carry it now.
    assert!(
        wait_for(Duration::from_secs(15), || {
            std::fs::metadata(mountpoint.path().join("skills/web/SKILL.md")).is_ok()
        }),
        "the ordinary web install never activated at /skills/web"
    );

    let entries = list_dir_names(&mountpoint.path().join("skills"));
    assert!(
        entries.iter().any(|n| n == "web"),
        "an ordinary skill installed over a pre-scanned staging leaf name must be listed in /skills, got: {entries:?}"
    );
    let discover =
        std::fs::read_to_string(mountpoint.path().join("skills/skill-discover/SKILL.md"))
            .expect("discover after install");
    assert!(
        discover.contains("| web |"),
        "an ordinary skill installed over a pre-scanned staging leaf name must stay discoverable, got:\n{discover}"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let mp = mountpoint.path().to_path_buf();
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mp.to_string_lossy()])
        .output();
    ctrl.shutdown();
}
