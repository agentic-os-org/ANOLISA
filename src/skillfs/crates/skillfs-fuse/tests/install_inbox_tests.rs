//! Package L1 install-inbox namespace integration tests.
//!
//! Coverage:
//!
//! 1. A hidden / new skill is writable through `/.skillfs-inbox/<skill>/...`.
//! 2. `/skills/<skill>` stays `ENOENT` until the resolver returns
//!    `current` / `fallback`.
//! 3. Writing `<inbox>/<skill>/.install-complete` enqueues exactly one
//!    debounced `scan -> resolve` for the candidate. Multi-file installs
//!    that don't touch `.install-complete` do not run scan/resolve.
//! 4. After resolve returns `current`, `/skills/<skill>` is visible;
//!    `hidden` keeps it absent; `fallback` serves the snapshot tree.
//! 5. `.skill-meta/**` writes through the inbox remain denied for ordinary
//!    installers and only the trusted-writer path may mutate them.
//!
//! These tests intentionally compose the existing demo refresh wiring so
//! the inbox path types ride on the same scan/resolve / `ActiveSkillResolver`
//! plumbing as `/skills/<skill>` does.

#![allow(clippy::too_many_arguments)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, ActiveTarget, FailedResolveBehavior, LedgerAdapter, LedgerResolveResult,
    RefreshController, SecurityEvent, SecurityEventWriter, StaticAdapterCall, StaticLedgerAdapter,
    TrustedWriterConfig,
};
use skillfs_fuse::{MountConfig, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available};

// ─────────────────────────────────────────────────────────────────────────────
// Test event writer + identity resolver helpers
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct CapturingWriter {
    inner: Mutex<Vec<SecurityEvent>>,
}

impl CapturingWriter {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn events(&self) -> Vec<SecurityEvent> {
        self.inner.lock().clone()
    }

    fn wait_for_n(&self, n: usize, timeout: Duration) -> Vec<SecurityEvent> {
        let start = std::time::Instant::now();
        loop {
            let events = self.events();
            if events.len() >= n {
                return events;
            }
            if start.elapsed() >= timeout {
                return events;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl SecurityEventWriter for CapturingWriter {
    fn emit(&self, event: &SecurityEvent) {
        self.inner.lock().push(event.clone());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ledger adapter helpers
// ─────────────────────────────────────────────────────────────────────────────

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

fn fallback_result(skill: &str, snapshot_segment: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "deny",
            "decision": "fallback",
            "currentVersion": "v000003",
            "trustedVersion": "{snapshot_segment}",
            "target": ".skill-meta/versions/{snapshot_segment}",
            "targetKind": "relative_to_skill_dir",
            "reason": "current version has high-risk findings"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("fallback json")
}

fn hidden_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "none",
            "decision": "hidden",
            "reason": "no certified version yet"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

fn write_snapshot(source: &Path, skill: &str, version: &str, skill_md: &str) {
    let dir = source
        .join(skill)
        .join(".skill-meta/versions")
        .join(version);
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("write snapshot SKILL.md");
}

fn sorted_dir(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    entries
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// Combined test: a brand-new skill is invisible at `/skills/<skill>`,
/// the inbox path lets the installer create the directory, write
/// SKILL.md (without triggering a refresh), and the
/// `.install-complete` sentinel finally enqueues exactly one
/// `scan -> resolve` pair which flips the resolver to `current`. After
/// the flip, `/skills/<skill>` becomes visible.
#[test]
fn inbox_install_flow_hidden_then_install_complete_then_current() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "anchor");

    let adapter = StaticLedgerAdapter::new();
    adapter.insert("fresh-skill", current_result("fresh-skill"));
    let logged: Arc<StaticLedgerAdapter> = Arc::new(adapter);
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = logged.clone();

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // 1) `/skills/<fresh-skill>` is not yet visible — the resolver has
    //    no entry for it.
    let listing_before = sorted_dir(&mountpoint.path().join("skills"));
    assert!(
        !listing_before.contains(&"fresh-skill".to_string()),
        "/skills/fresh-skill must not exist before scan/resolve, got {listing_before:?}"
    );
    let direct = std::fs::metadata(mountpoint.path().join("skills/fresh-skill"));
    assert!(
        direct.is_err(),
        "direct stat of /skills/fresh-skill must fail before scan/resolve"
    );
    let kind = direct.unwrap_err().raw_os_error();
    assert_eq!(kind, Some(libc::ENOENT));

    // 2) The inbox is visible at the FUSE root.
    let root_listing = sorted_dir(mountpoint.path());
    assert!(
        root_listing.contains(&".skillfs-inbox".to_string()),
        "/.skillfs-inbox must be visible at root, got {root_listing:?}"
    );

    // 3) Create the candidate skill through the inbox. This lands as
    //    `source/fresh-skill` and a store placeholder.
    let inbox_skill = mountpoint.path().join(".skillfs-inbox/fresh-skill");
    std::fs::create_dir(&inbox_skill).expect("mkdir inbox skill");
    assert!(
        source.path().join("fresh-skill").is_dir(),
        "inbox mkdir must create source/<skill>"
    );

    // 4) Write SKILL.md through the inbox. This must NOT trigger a
    //    scan/resolve yet (multi-file installs would otherwise bounce
    //    the controller).
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: fresh-skill\ndescription: candidate\n---\n",
    )
    .expect("write SKILL.md through inbox");
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        events.events().is_empty(),
        "non-sentinel inbox writes must not trigger scan/resolve, got {:?}",
        events.events()
    );
    assert_eq!(
        logged.calls(),
        Vec::<StaticAdapterCall>::new(),
        "no scan/resolve for plain inbox writes"
    );
    // /skills/fresh-skill must still be ENOENT — the resolver hasn't
    // installed a target yet.
    let still_missing = std::fs::metadata(mountpoint.path().join("skills/fresh-skill"));
    assert_eq!(
        still_missing.unwrap_err().raw_os_error(),
        Some(libc::ENOENT),
    );

    // 5) Sentinel write triggers exactly one scan -> resolve pair.
    std::fs::write(inbox_skill.join(".install-complete"), b"")
        .expect("write install-complete sentinel");
    let evs = events.wait_for_n(1, Duration::from_millis(2000));
    assert_eq!(
        evs.len(),
        1,
        "install-complete must produce exactly one demo event, got {evs:?}"
    );
    assert_eq!(evs[0].skill, "fresh-skill");
    assert_eq!(evs[0].ledger_action, "scan -> resolve");
    assert_eq!(evs[0].skillfs_decision, "current");
    let calls = logged.calls();
    assert_eq!(
        calls,
        vec![
            StaticAdapterCall::Scan {
                skill_name: "fresh-skill".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "fresh-skill".to_string()
            },
        ],
        "exactly one scan + one resolve must run for the candidate"
    );

    // 6) After resolve=current, /skills/fresh-skill becomes visible.
    //    Wait briefly for kernel attribute caches to settle and
    //    re-issue lookup.
    std::thread::sleep(Duration::from_millis(1500));
    let visible_md = mountpoint.path().join("skills/fresh-skill/SKILL.md");
    let _meta = std::fs::metadata(&visible_md).expect("/skills/<skill>/SKILL.md must exist");

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
}

/// `resolve` returning `hidden` keeps `/skills/<skill>` absent even
/// after install-complete; the inbox stays writable so the installer
/// can repair the candidate.
#[test]
fn resolve_hidden_keeps_skills_invisible_but_inbox_writable() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "anchor");

    let adapter = StaticLedgerAdapter::new();
    adapter.insert("hidden-skill", hidden_result("hidden-skill"));
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_skill = mountpoint.path().join(".skillfs-inbox/hidden-skill");
    std::fs::create_dir(&inbox_skill).expect("mkdir inbox skill");
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: hidden-skill\ndescription: candidate\n---\n",
    )
    .expect("write SKILL.md");
    std::fs::write(inbox_skill.join(".install-complete"), b"").expect("write sentinel");
    let _ = events.wait_for_n(1, Duration::from_millis(2000));
    std::thread::sleep(Duration::from_millis(1500));

    // /skills/hidden-skill must remain absent.
    let listing = sorted_dir(&mountpoint.path().join("skills"));
    assert!(
        !listing.contains(&"hidden-skill".to_string()),
        "/skills/hidden-skill must remain hidden, got {listing:?}"
    );
    match resolver.get("hidden-skill") {
        Some(ActiveTarget::Hidden { .. }) => {}
        other => panic!("expected resolver to mark hidden-skill as hidden, got {other:?}"),
    }

    // The inbox is still writable — installers can keep editing.
    std::fs::write(inbox_skill.join("note.txt"), b"repair attempt")
        .expect("inbox must stay writable for hidden skills");
    let body = std::fs::read_to_string(inbox_skill.join("note.txt")).expect("read inbox file");
    assert_eq!(body, "repair attempt");

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
}

/// `resolve` returning `fallback` exposes the trusted snapshot at
/// `/skills/<skill>/SKILL.md` while the inbox keeps mapping to the live
/// source candidate dir, so the installer can repair the live source
/// without disturbing the snapshot the runtime is reading from.
#[test]
fn resolve_fallback_serves_snapshot_via_skills_inbox_keeps_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "demo-weather");
    write_snapshot(
        source.path(),
        "demo-weather",
        "v000001.snapshot",
        "---\nname: demo-weather\ndescription: trusted snapshot\n---\n\n# trusted body\n",
    );

    let adapter = StaticLedgerAdapter::new();
    adapter.insert(
        "demo-weather",
        fallback_result("demo-weather", "v000001.snapshot"),
    );
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("demo-weather"))
        .expect("seed current");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // Repair the live source via the inbox: write a clearly-marked
    // candidate SKILL.md, then sentinel-trigger scan/resolve. The
    // adapter returns `fallback`, so `/skills/demo-weather/SKILL.md`
    // must serve the snapshot — never the live candidate the
    // installer just wrote.
    let inbox_skill = mountpoint.path().join(".skillfs-inbox/demo-weather");
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: demo-weather\ndescription: live candidate (must NOT be served)\n---\n",
    )
    .expect("write live candidate via inbox");
    std::fs::write(inbox_skill.join(".install-complete"), b"").expect("write sentinel");
    let _ = events.wait_for_n(1, Duration::from_millis(2000));
    std::thread::sleep(Duration::from_millis(1500));

    let visible_md =
        std::fs::read_to_string(mountpoint.path().join("skills/demo-weather/SKILL.md"))
            .expect("read /skills/<skill>/SKILL.md");
    assert!(
        visible_md.contains("trusted body"),
        "/skills must serve the snapshot, got {visible_md:?}"
    );
    assert!(
        !visible_md.contains("live candidate"),
        "/skills must not leak the live inbox candidate"
    );

    // The inbox sees the live source, not the snapshot.
    let inbox_md =
        std::fs::read_to_string(inbox_skill.join("SKILL.md")).expect("read inbox SKILL.md");
    assert!(
        inbox_md.contains("live candidate"),
        "inbox must keep mapping to the live source, got {inbox_md:?}"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
}

/// Ordinary installers cannot mutate `.skill-meta/**` through the
/// inbox. Only the configured trusted-writer process may, exactly the
/// same way `/skills/<skill>/.skill-meta/**` is gated.
#[test]
fn skill_meta_under_inbox_is_denied_for_ordinary_installers() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "demo-weather");

    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_meta = mountpoint
        .path()
        .join(".skillfs-inbox/demo-weather/.skill-meta");
    let mkdir_err = std::fs::create_dir(&inbox_meta).expect_err("inbox .skill-meta mkdir denied");
    assert_eq!(mkdir_err.raw_os_error(), Some(libc::EACCES));

    // Defense-in-depth: a deeper `.skill-meta` write also fails.
    let deeper = mountpoint
        .path()
        .join(".skillfs-inbox/demo-weather/.skill-meta/manifest.json");
    let write_err =
        std::fs::write(&deeper, b"{}").expect_err("inbox .skill-meta deep write must be denied");
    assert!(
        matches!(
            write_err.raw_os_error(),
            Some(libc::EACCES) | Some(libc::ENOENT)
        ),
        "expected EACCES/ENOENT, got {write_err:?}"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

/// The trusted-writer bypass that already covers
/// `/skills/<skill>/.skill-meta/**` in D1.5-mvp also covers the inbox
/// path so the configured ledger writer can keep writing manifests
/// while installers go through the inbox.
#[test]
fn skill_meta_under_inbox_is_allowed_for_trusted_writer() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // Resolve the test process's own `comm` from `/proc/self/comm` —
    // the trusted-writer gate compares against this, and the test
    // process is the same one writing through the mount.
    let comm_path = "/proc/self/comm";
    let raw_comm = match std::fs::read_to_string(comm_path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("SKIP: cannot read /proc/self/comm");
            return;
        }
    };
    let trusted_name = raw_comm.trim().to_string();
    if trusted_name.is_empty() {
        eprintln!("SKIP: /proc/self/comm is empty");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "demo-weather");
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let trusted = TrustedWriterConfig::with_process_name(trusted_name.clone());
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            refresh_controller: None,
            trusted_writer: Some(trusted),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // The bypass must let the trusted writer create
    // `.skill-meta/versions/v000001.snapshot/SKILL.md` through the
    // inbox path — the same kind of manifest write the ledger does
    // out-of-band today.
    let inbox_meta_dir = mountpoint
        .path()
        .join(".skillfs-inbox/demo-weather/.skill-meta/versions/v000001.snapshot");
    std::fs::create_dir_all(&inbox_meta_dir)
        .expect("trusted writer must be allowed to create .skill-meta dirs through inbox");
    std::fs::write(
        inbox_meta_dir.join("SKILL.md"),
        "---\nname: demo-weather\n---\n",
    )
    .expect("trusted writer must be allowed to write under .skill-meta through inbox");

    // Sanity check — the byte hit the live source candidate dir.
    let physical = source
        .path()
        .join("demo-weather/.skill-meta/versions/v000001.snapshot/SKILL.md");
    let content = std::fs::read_to_string(&physical).expect("snapshot SKILL.md on disk");
    assert!(content.contains("name: demo-weather"));

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// P1 regressions
// ─────────────────────────────────────────────────────────────────────────────

/// L1 contract regression: `touch /.skillfs-inbox/<name>` must not
/// create a regular file in the source root. The candidate at
/// `<inbox>/<name>` is a directory slot owned by `mkdir`; a `create`
/// call against it must return `EISDIR` and leave the source tree
/// untouched.
#[test]
fn create_at_inbox_skill_slot_is_refused_and_does_not_create_source_file() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // `touch` (open with O_CREAT) at a candidate slot must fail with
    // EISDIR — the slot is reserved for `mkdir <name>`.
    let inbox_slot = mountpoint.path().join(".skillfs-inbox/new-skill");
    let create_err = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&inbox_slot)
        .expect_err("touch at inbox skill slot must fail");
    assert_eq!(
        create_err.raw_os_error(),
        Some(libc::EISDIR),
        "expected EISDIR, got {create_err:?}"
    );

    // The source root must NOT have grown a `new-skill` regular file.
    let physical = source.path().join("new-skill");
    assert!(
        std::fs::symlink_metadata(&physical).is_err(),
        "inbox-slot create must not produce source/new-skill, found: {:?}",
        std::fs::symlink_metadata(&physical),
    );

    // Sanity check: the legitimate `mkdir` flow at the same path still
    // works and lands as a directory.
    std::fs::create_dir(&inbox_slot).expect("mkdir at inbox slot");
    let meta = std::fs::symlink_metadata(source.path().join("new-skill"))
        .expect("source/new-skill exists after mkdir");
    assert!(meta.is_dir(), "mkdir must produce a directory");

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

/// `create` (or `mkdir`) of the inbox virtual root itself must be
/// refused with `EEXIST`, never fall through to the physical source
/// path.
#[test]
fn create_of_inbox_virtual_root_is_refused() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let source = tempfile::tempdir().expect("source");
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_root = mountpoint.path().join(".skillfs-inbox");
    // `mkdir` of the always-present inbox root surfaces as EEXIST.
    let mkdir_err = std::fs::create_dir(&inbox_root).expect_err("inbox root mkdir must fail");
    assert_eq!(mkdir_err.raw_os_error(), Some(libc::EEXIST));

    // The source root must not grow a stray `.skillfs-inbox` directory
    // either — the inbox is purely virtual.
    let physical = source.path().join(".skillfs-inbox");
    assert!(
        std::fs::symlink_metadata(&physical).is_err(),
        "inbox virtual root must not be created in source, got {:?}",
        std::fs::symlink_metadata(&physical),
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

/// Inbox listing / lookup / mutation must never expose dot-prefixed
/// or otherwise non-skill source-root entries (`.git`,
/// `.skill-meta`, `.skillfs-inbox`, `.staging`, `Alpha`, …). The
/// inbox is the install / repair entrance for SkillFS skills, not a
/// passthrough to the entire source root.
#[test]
fn inbox_skill_name_filter_rejects_non_skill_source_entries() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "alpha");
    // Pre-seed a few directories / files in source that the inbox must
    // NOT surface. A genuine skill name (`alpha`) is also seeded above
    // so the positive case stays observable.
    for noise_dir in [
        ".git",
        ".skill-meta",
        ".skillfs-inbox",
        ".staging",
        ".certified",
        ".quarantine",
        ".archive",
        ".cache",
        "Alpha",
        "foo_bar",
    ] {
        std::fs::create_dir_all(source.path().join(noise_dir)).expect("seed noise dir");
    }
    std::fs::write(source.path().join("skillfs-views.toml"), b"# views").expect("seed views file");

    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_root = mountpoint.path().join(".skillfs-inbox");
    let listing = sorted_dir(&inbox_root);
    let allowed: std::collections::HashSet<&str> = ["alpha"].iter().copied().collect();
    for entry in &listing {
        assert!(
            allowed.contains(entry.as_str()),
            "inbox listing leaked non-skill entry {entry:?}, full listing: {listing:?}"
        );
    }
    assert!(
        listing.contains(&"alpha".to_string()),
        "inbox listing must still include valid kebab-case skills"
    );

    // Direct lookup of each rejected name must surface ENOENT, and
    // mkdir of the same name must fail deterministically without
    // creating source/<name> by side effect.
    for blocked in [
        ".git",
        ".skill-meta",
        ".skillfs-inbox",
        ".staging",
        ".certified",
        ".quarantine",
        ".archive",
        ".cache",
        "Alpha",
        "foo_bar",
    ] {
        let probe = inbox_root.join(blocked);
        let stat_err = std::fs::symlink_metadata(&probe)
            .expect_err(&format!("lookup of {blocked} via inbox must fail"));
        assert_eq!(
            stat_err.raw_os_error(),
            Some(libc::ENOENT),
            "expected ENOENT for inbox/{blocked}, got {stat_err:?}"
        );

        let mkdir_err = std::fs::create_dir(&probe)
            .expect_err(&format!("mkdir at /.skillfs-inbox/{blocked} must fail"));
        let mkdir_errno = mkdir_err.raw_os_error();
        assert!(
            matches!(mkdir_errno, Some(libc::EACCES) | Some(libc::ENOENT)),
            "expected EACCES/ENOENT for inbox mkdir of {blocked}, got {mkdir_errno:?}"
        );
    }

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// N1/D1.6 canonical skill identity through the inbox
// ─────────────────────────────────────────────────────────────────────────────

/// A `SKILL.md` whose frontmatter declares a different `name:` than the
/// inbox candidate directory must never produce an alias under
/// `/skills/<declaredName>`. The provider may report the mismatch via
/// `declaredName` and use it as a security signal — here, the provider
/// returns `decision=hidden`. Result: `/skills/weather` is hidden,
/// `/skills/calculator` is `ENOENT`, and the candidate's source
/// directory remains keyed by `weather`.
#[test]
fn inbox_candidate_with_declared_name_mismatch_keeps_canonical_identity() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "anchor");

    // Provider returns the canonical skillName=weather plus
    // declaredName=calculator and decision=hidden.
    let mismatch_json = r#"{
        "schemaVersion": 1,
        "skillName": "weather",
        "declaredName": "calculator",
        "status": "deny",
        "decision": "hidden",
        "reason": "frontmatter name disagrees with directory"
    }"#;
    let parsed = LedgerResolveResult::from_json_str(mismatch_json).expect("parse");

    let adapter = StaticLedgerAdapter::new();
    adapter.insert("weather", parsed);
    let logged: Arc<StaticLedgerAdapter> = Arc::new(adapter);
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = logged.clone();

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // Install the candidate through the inbox: directory `weather`,
    // SKILL.md whose frontmatter declares `name: calculator`.
    let inbox_skill = mountpoint.path().join(".skillfs-inbox/weather");
    std::fs::create_dir(&inbox_skill).expect("mkdir inbox/weather");
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: calculator\ndescription: mismatched\n---\n",
    )
    .expect("write SKILL.md with mismatching frontmatter");
    std::fs::write(inbox_skill.join(".install-complete"), b"")
        .expect("write install-complete sentinel");

    let evs = events.wait_for_n(1, Duration::from_millis(2000));
    assert_eq!(evs.len(), 1, "expected exactly one demo event, got {evs:?}");
    assert_eq!(evs[0].skill, "weather");
    assert_eq!(evs[0].ledger_action, "scan -> resolve");

    // The decision pipeline ran for the canonical name only.
    assert_eq!(
        logged.calls(),
        vec![
            StaticAdapterCall::Scan {
                skill_name: "weather".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "weather".to_string()
            },
        ]
    );

    // Resolver mapping is keyed by the directory name, not declaredName.
    match resolver.get("weather") {
        Some(ActiveTarget::Hidden { .. }) => {}
        other => panic!("expected weather to be hidden, got {other:?}"),
    }
    assert!(
        resolver.get("calculator").is_none(),
        "/skills/calculator must NEVER materialize from declaredName"
    );

    std::thread::sleep(Duration::from_millis(1500));
    // /skills/weather is hidden.
    let listing = sorted_dir(&mountpoint.path().join("skills"));
    assert!(!listing.contains(&"weather".to_string()));
    assert!(!listing.contains(&"calculator".to_string()));
    let weather_err =
        std::fs::metadata(mountpoint.path().join("skills/weather")).expect_err("weather hidden");
    assert_eq!(weather_err.raw_os_error(), Some(libc::ENOENT));
    let calc_err = std::fs::metadata(mountpoint.path().join("skills/calculator"))
        .expect_err("declaredName never paths");
    assert_eq!(calc_err.raw_os_error(), Some(libc::ENOENT));

    // The physical inbox candidate still lives under source/weather.
    assert!(source.path().join("weather").is_dir());
    assert!(!source.path().join("calculator").exists());

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
}

/// If the provider returns a resolve whose `skillName` does not match the
/// inbox candidate (`weather`), SkillFS rejects the result via
/// `validate_for_expected_skill`. Under the demo's default
/// `HideOnFailure` policy the canonical skill is hidden and
/// `/skills/calculator` never appears.
#[test]
fn inbox_resolve_with_wrong_skill_name_is_rejected_and_calculator_never_appears() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "anchor");

    // Provider mistakenly returns a resolve keyed by `calculator` for a
    // request that asked about `weather`. SkillFS must reject this as
    // a `SkillNameMismatch` failure.
    let bogus_json = r#"{
        "schemaVersion": 1,
        "skillName": "calculator",
        "status": "pass",
        "decision": "current",
        "currentVersion": "v000001",
        "trustedVersion": "v000001"
    }"#;
    let bogus = LedgerResolveResult::from_json_str(bogus_json).expect("parse");

    let adapter = StaticLedgerAdapter::new();
    adapter.insert("weather", bogus);
    let logged: Arc<StaticLedgerAdapter> = Arc::new(adapter);
    let adapter_for_ctrl: Arc<dyn LedgerAdapter> = logged.clone();

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter_for_ctrl,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_skill = mountpoint.path().join(".skillfs-inbox/weather");
    std::fs::create_dir(&inbox_skill).expect("mkdir inbox/weather");
    std::fs::write(
        inbox_skill.join("SKILL.md"),
        "---\nname: weather\ndescription: candidate\n---\n",
    )
    .expect("write SKILL.md");
    std::fs::write(inbox_skill.join(".install-complete"), b"")
        .expect("write install-complete sentinel");

    let evs = events.wait_for_n(1, Duration::from_millis(2000));
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].skill, "weather");
    assert_eq!(evs[0].ledger_action, "scan -> resolve failed");
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    let message = evs[0].message.as_deref().unwrap_or_default();
    assert!(
        message.contains("weather") && message.contains("calculator"),
        "demo event message must surface expected and actual names, got {message:?}"
    );

    // Resolver: weather is hidden, calculator never materializes.
    match resolver.get("weather") {
        Some(ActiveTarget::Hidden { .. }) => {}
        other => panic!("expected weather hidden under HideOnFailure, got {other:?}"),
    }
    assert!(resolver.get("calculator").is_none());

    std::thread::sleep(Duration::from_millis(1500));
    let listing = sorted_dir(&mountpoint.path().join("skills"));
    assert!(!listing.contains(&"weather".to_string()));
    assert!(!listing.contains(&"calculator".to_string()));
    let calc_err = std::fs::metadata(mountpoint.path().join("skills/calculator"))
        .expect_err("calculator must not exist");
    assert_eq!(calc_err.raw_os_error(), Some(libc::ENOENT));

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
}

/// Renaming a candidate skill into a non-kebab-case name (e.g.
/// `mv inbox/alpha inbox/.git`) must fail rather than silently
/// creating `source/.git` and dropping the candidate from inbox
/// listings.
#[test]
fn rename_into_invalid_inbox_name_is_refused() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "alpha");
    let mountpoint = tempfile::tempdir().expect("mount");
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        MountConfig {
            active_resolver: None,
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    let inbox_alpha = mountpoint.path().join(".skillfs-inbox/alpha");
    let inbox_git = mountpoint.path().join(".skillfs-inbox/.git");
    let err = std::fs::rename(&inbox_alpha, &inbox_git)
        .expect_err("rename to invalid inbox name must fail");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EACCES),
        "expected EACCES for rename to invalid inbox name, got {err:?}"
    );
    assert!(
        source.path().join("alpha").is_dir(),
        "source/alpha must still exist after refused rename"
    );
    assert!(
        std::fs::symlink_metadata(source.path().join(".git")).is_err(),
        "source/.git must not appear from a refused rename"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}
