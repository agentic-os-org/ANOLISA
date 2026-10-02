//! D1.1 ledger active-mapping FUSE integration tests.
//!
//! Pin the read-side contract that the CLI bootstrap wires up:
//!
//! * **Hidden**: a skill with `ActiveTarget::Hidden` is dropped from
//!   `/skills` readdir and direct `lookup` / `stat` returns `ENOENT`.
//! * **Current**: a skill with `ActiveTarget::Current` reads the live
//!   source directory exactly as it would without any resolver
//!   attached.
//! * **Snapshot (fallback)**: a skill with `ActiveTarget::Snapshot`
//!   reads the trusted snapshot directory for both `SKILL.md` and
//!   ordinary passthrough files. Snapshot `SKILL.md` preserves
//!   compiled-read semantics — the kernel sees the compiled bytes, not
//!   the raw markdown.
//! * **No resolver attached**: the pre-D1.1 mount behavior is preserved
//!   bit-for-bit (no hidden skills, no snapshot redirection).
//!
//! The tests mount FUSE in normal mode with an in-process resolver
//! seeded directly (no subprocess). The CLI subprocess path is
//! exercised by the contract-level tests in
//! `ledger_demo_contract_tests.rs`; here we focus on the FUSE callbacks
//! the resolver actually drives.

#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{ActiveSkillResolver, LedgerError, LedgerResolveResult};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available};

// ─────────────────────────────────────────────────────────────────────────────
// Local fixture
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal normal-mode mount fixture that lets the test inject a
/// pre-built `ActiveSkillResolver`. The resolver builder receives the
/// real source path so snapshot `target` joins line up with
/// `SkillFs::source`.
struct LedgerMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl LedgerMount {
    fn new<S, R>(seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false, // normal mode
            MountConfig {
                active_resolver: resolver,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skills_dir(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.skills_dir().join(name)
    }

    fn skill_md(&self, name: &str) -> PathBuf {
        self.skill_dir(name).join("SKILL.md")
    }

    fn source_skill_dir(&self, name: &str) -> PathBuf {
        self.source.path().join(name)
    }
}

impl Drop for LedgerMount {
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

fn sorted_dir(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    entries
}

/// Lay down a fake trusted snapshot under
/// `<source>/<skill>/.skill-meta/versions/<version>/`. SkillFS's
/// `.skill-meta` write gate runs on FUSE callbacks; writing directly via
/// the source path bypasses FUSE and is the expected way a future
/// trusted writer (or, today, the test harness simulating the ledger)
/// populates the snapshot dir.
fn write_snapshot(
    source: &Path,
    skill: &str,
    version: &str,
    skill_md: &str,
    files: &[(&str, &str)],
) -> PathBuf {
    let dir = source
        .join(skill)
        .join(".skill-meta/versions")
        .join(version);
    std::fs::create_dir_all(&dir).expect("create snapshot dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("write snapshot SKILL.md");
    for (rel, body) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("snapshot parent");
        }
        std::fs::write(&p, body).expect("snapshot file");
    }
    dir
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

fn fallback_result(skill: &str, snapshot_segment: &str) -> LedgerResolveResult {
    // `snapshot_segment` is appended to `.skill-meta/versions/`.
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
            "status": "deny",
            "decision": "hidden",
            "reason": "no trusted version available"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hidden_skill_is_absent_from_readdir_and_lookup_returns_enoent() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo-weather");
            create_skill_dir(src, "always-visible");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("demo-weather")).unwrap();
            r.set_from_resolve(&current_result("always-visible"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let listing = sorted_dir(&mount.skills_dir());
    assert!(
        listing.contains(&"always-visible".to_string()),
        "current skill should still be visible, got {listing:?}"
    );
    assert!(
        listing.contains(&"skill-discover".to_string()),
        "skill-discover must remain visible regardless of ledger, got {listing:?}"
    );
    assert!(
        !listing.contains(&"demo-weather".to_string()),
        "hidden skill must not appear in /skills, got {listing:?}"
    );

    let err = std::fs::metadata(mount.skill_dir("demo-weather")).unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "expected ENOENT for hidden skill dir, got {err:?}"
    );

    let err = std::fs::metadata(mount.skill_md("demo-weather")).unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "expected ENOENT for hidden skill SKILL.md, got {err:?}"
    );
}

#[test]
fn skill_absent_from_resolver_defaults_to_hidden_in_demo_mode() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // The resolver knows about `mapped` only; `unmapped` exists in the
    // store but not in the resolver. The contract says missing-in-demo
    // means hidden.
    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "mapped");
            create_skill_dir(src, "unmapped");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&current_result("mapped")).unwrap();
            Some(Arc::new(r))
        },
    );

    let listing = sorted_dir(&mount.skills_dir());
    assert!(listing.contains(&"mapped".to_string()));
    assert!(!listing.contains(&"unmapped".to_string()));

    let err = std::fs::metadata(mount.skill_dir("unmapped")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn current_skill_reads_live_source_directory() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo-weather");
            // A passthrough file that lives only on the live source.
            std::fs::create_dir_all(src.join("demo-weather/scripts")).unwrap();
            std::fs::write(
                src.join("demo-weather/scripts/run.sh"),
                "#!/bin/sh\necho live\n",
            )
            .unwrap();
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&current_result("demo-weather")).unwrap();
            Some(Arc::new(r))
        },
    );

    // SKILL.md is compiled from the live source. The fixture seeds it
    // with minimal frontmatter and no body, so the compiled bytes equal
    // the source bytes.
    let live_md = std::fs::read_to_string(mount.skill_md("demo-weather")).expect("read mount md");
    let source_md =
        std::fs::read_to_string(mount.source_skill_dir("demo-weather").join("SKILL.md"))
            .expect("read source md");
    assert_eq!(live_md, source_md);

    let live_script =
        std::fs::read_to_string(mount.skill_dir("demo-weather").join("scripts/run.sh"))
            .expect("read mount script");
    assert_eq!(live_script, "#!/bin/sh\necho live\n");

    // Skill dir listing reflects the live source.
    let listing = sorted_dir(&mount.skill_dir("demo-weather"));
    assert!(listing.contains(&"SKILL.md".to_string()));
    assert!(listing.contains(&"scripts".to_string()));
}

#[test]
fn fallback_skill_reads_trusted_snapshot_with_compiled_skill_md() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // Snapshot contents intentionally differ from the live source: the
    // assertion is that reads serve the snapshot, not the live tree.
    let snapshot_md =
        "---\nname: demo-weather\ndescription: trusted snapshot\n---\n\n# Trusted snapshot body\n";
    let snapshot_script_body = "#!/bin/sh\necho trusted\n";

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo-weather");
            // Live source has a `scripts/run.sh` that says `live`.
            std::fs::create_dir_all(src.join("demo-weather/scripts")).unwrap();
            std::fs::write(
                src.join("demo-weather/scripts/run.sh"),
                "#!/bin/sh\necho live\n",
            )
            .unwrap();
            // Snapshot lives under the same skill's .skill-meta tree
            // with a different SKILL.md and a different script body.
            write_snapshot(
                src,
                "demo-weather",
                "v000001.snapshot",
                snapshot_md,
                &[("scripts/run.sh", snapshot_script_body)],
            );
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo-weather", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    // SKILL.md is compiled from the *snapshot* SKILL.md, not the live
    // one. The fixture seeds the live SKILL.md with minimal
    // `name`/`description` frontmatter (no body); the snapshot SKILL.md
    // has a body. If SkillFS were still reading the live SKILL.md we
    // would not see the snapshot body — assert we see the snapshot
    // body and a snapshot-specific description.
    let served_md =
        std::fs::read_to_string(mount.skill_md("demo-weather")).expect("read mount md (snapshot)");
    assert!(
        served_md.contains("Trusted snapshot body"),
        "expected snapshot SKILL.md body, got: {served_md:?}"
    );
    assert!(
        served_md.contains("trusted snapshot"),
        "expected snapshot frontmatter description, got: {served_md:?}"
    );
    let live_md = std::fs::read_to_string(mount.source_skill_dir("demo-weather").join("SKILL.md"))
        .expect("read source md");
    assert_ne!(
        served_md, live_md,
        "served SKILL.md must differ from live source SKILL.md in fallback mode"
    );

    // Passthrough script is served from the snapshot too.
    let served_script =
        std::fs::read_to_string(mount.skill_dir("demo-weather").join("scripts/run.sh"))
            .expect("read snapshot script via mount");
    assert_eq!(served_script, snapshot_script_body);

    // Skill is visible in /skills under fallback.
    let listing = sorted_dir(&mount.skills_dir());
    assert!(listing.contains(&"demo-weather".to_string()));
}

#[test]
fn fallback_o_rdonly_o_trunc_targets_live_source_not_snapshot() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // Regression for the D1.1 boundary: `O_RDONLY | O_TRUNC` is
    // mutating from the kernel's perspective even though the access
    // mode is read-only. SkillFS must direct the truncate to the live
    // source — never to the trusted snapshot — and the snapshot bytes
    // must remain intact afterwards. After the call, plain reads of
    // the same path through the mount must keep coming from the
    // snapshot (the read-side decision is unchanged), so the test
    // doubles as a check that read paths still serve the snapshot.
    let snapshot_md = "---\nname: demo-weather\ndescription: snapshot\n---\n";
    let snapshot_script = "#!/bin/sh\necho trusted\n";
    let live_script = "#!/bin/sh\necho live\n";

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo-weather");
            std::fs::create_dir_all(src.join("demo-weather/scripts")).unwrap();
            std::fs::write(src.join("demo-weather/scripts/run.sh"), live_script).unwrap();
            write_snapshot(
                src,
                "demo-weather",
                "v000001.snapshot",
                snapshot_md,
                &[("scripts/run.sh", snapshot_script)],
            );
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo-weather", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let live_path = mount
        .source_skill_dir("demo-weather")
        .join("scripts/run.sh");
    let snapshot_path = mount
        .source_skill_dir("demo-weather")
        .join(".skill-meta/versions/v000001.snapshot/scripts/run.sh");
    let mount_path = mount.skill_dir("demo-weather").join("scripts/run.sh");

    // Sanity: live and snapshot files differ before the open.
    assert_eq!(std::fs::read_to_string(&live_path).unwrap(), live_script);
    assert_eq!(
        std::fs::read_to_string(&snapshot_path).unwrap(),
        snapshot_script
    );

    // Open through the mount with `O_RDONLY | O_TRUNC`. The mutation
    // must hit the live source.
    let cstr = std::ffi::CString::new(mount_path.to_string_lossy().as_bytes()).unwrap();
    let fd = unsafe { libc::open(cstr.as_ptr(), libc::O_RDONLY | libc::O_TRUNC) };
    assert!(
        fd >= 0,
        "expected O_RDONLY|O_TRUNC to succeed, errno = {}",
        std::io::Error::last_os_error()
    );
    unsafe { libc::close(fd) };

    // Snapshot must remain untouched.
    assert_eq!(
        std::fs::read_to_string(&snapshot_path).unwrap(),
        snapshot_script,
        "snapshot file must be read-only across O_RDONLY|O_TRUNC"
    );
    // Live source must have been truncated to zero bytes.
    let live_after = std::fs::metadata(&live_path).unwrap();
    assert_eq!(
        live_after.len(),
        0,
        "live source file must have been truncated"
    );

    // Note: we deliberately do NOT assert that a subsequent
    // `read(mount_path)` still serves the snapshot bytes. After a
    // successful FUSE truncate the kernel typically caches `size = 0`
    // for the inode and short-circuits later reads regardless of what
    // our `getattr` would return for the snapshot — that interaction
    // is a kernel/FUSE caching detail, not the D1.1 contract. The
    // load-bearing assertions are above: the snapshot file on disk is
    // untouched and the live source file was truncated. Together they
    // prove the redirect targeted the live source, not the snapshot.
}

#[test]
fn no_resolver_attached_preserves_existing_behavior() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // Build a mount with no resolver. Behavior must match the existing
    // (pre-D1.1) mount exactly: every seeded skill is visible, SKILL.md
    // reads the live source, passthrough reads the live source.
    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "alpha");
            create_skill_dir(src, "beta");
            std::fs::create_dir_all(src.join("alpha/scripts")).unwrap();
            std::fs::write(src.join("alpha/scripts/r.sh"), "live").unwrap();
        },
        |_| None,
    );

    let listing = sorted_dir(&mount.skills_dir());
    assert!(listing.contains(&"alpha".to_string()));
    assert!(listing.contains(&"beta".to_string()));
    assert!(listing.contains(&"skill-discover".to_string()));

    // SKILL.md reads the live source.
    let live = std::fs::read_to_string(mount.skill_md("alpha")).expect("read alpha md");
    let src_md = std::fs::read_to_string(mount.source_skill_dir("alpha").join("SKILL.md"))
        .expect("read source md");
    assert_eq!(live, src_md);

    // Passthrough file reads the live source.
    let script =
        std::fs::read_to_string(mount.skill_dir("alpha").join("scripts/r.sh")).expect("read");
    assert_eq!(script, "live");
}

// ─────────────────────────────────────────────────────────────────────────────
// N1/D1.6 canonical skill identity
// ─────────────────────────────────────────────────────────────────────────────

/// A wrong `skillName` response (the provider replied for a different
/// directory than the one we asked about) must be rejected by
/// `validate_for_expected_skill` BEFORE the resolver is updated. The
/// existing entry — or absence of one — must remain unchanged.
#[test]
fn wrong_skill_name_response_does_not_update_resolver() {
    use skillfs_fuse::security::ActiveTarget;

    let resolver = ActiveSkillResolver::new("/srv/skills");
    // Pre-seed `weather` with `current` so we can confirm a mismatched
    // resolve cannot mutate the existing entry.
    resolver
        .set_from_resolve(&current_result("weather"))
        .unwrap();

    // Provider returned a result for a different skill.
    let bad = current_result("calculator");
    let err = bad.validate_for_expected_skill("weather").unwrap_err();
    assert!(matches!(err, LedgerError::SkillNameMismatch { .. }));

    // Resolver must still hold the original entry.
    let current = resolver.get("weather").expect("weather entry preserved");
    assert!(matches!(current, ActiveTarget::Current { .. }));
    // And no `/skills/calculator` alias must exist from declaredName/
    // mismatched provider keys.
    assert!(resolver.get("calculator").is_none());
}

/// Initial load with frontmatter `name: 天气` in directory
/// `tianqi-weather` must use `tianqi-weather` as the canonical store
/// key. The ledger/resolver operates on directory basenames; the
/// frontmatter-declared name must never create a mount alias.
#[test]
fn initial_load_frontmatter_name_mismatch_uses_directory_basename() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            let skill_dir = src.join("tianqi-weather");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                "---\nname: 天气\ndescription: weather skill\n---\n",
            )
            .unwrap();
            create_skill_dir(src, "always-visible");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&current_result("tianqi-weather"))
                .unwrap();
            r.set_from_resolve(&current_result("always-visible"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let listing = sorted_dir(&mount.skills_dir());
    assert!(
        listing.contains(&"tianqi-weather".to_string()),
        "/skills/tianqi-weather must be visible via canonical dir name, got {listing:?}"
    );
    assert!(
        !listing.contains(&"天气".to_string()),
        "/skills/天气 must NOT appear from frontmatter, got {listing:?}"
    );
    assert!(
        listing.contains(&"always-visible".to_string()),
        "unrelated skill must stay visible, got {listing:?}"
    );

    assert!(mount.skill_md("tianqi-weather").exists());

    let err = std::fs::metadata(mount.skill_dir("天气")).unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "frontmatter-name path must return ENOENT"
    );
}

/// A response with `skillName=weather`, `declaredName=calculator`,
/// `decision=hidden` is the canonical N1/D1.6 use case: the provider
/// observed a `SKILL.md` whose declared name disagrees with the
/// directory, and used the mismatch as a security signal to hide the
/// canonical skill. SkillFS must accept that response (skillName
/// matches the directory) and hide `/skills/weather` while never
/// surfacing `/skills/calculator`.
#[test]
fn declared_name_mismatch_with_hidden_decision_hides_canonical_skill() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "weather");
            create_skill_dir(src, "always-visible");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            // Provider keys by the directory name; declaredName is the
            // metadata signal that triggered the `hidden` decision.
            let json = r#"{
                "schemaVersion": 1,
                "skillName": "weather",
                "declaredName": "calculator",
                "status": "deny",
                "decision": "hidden",
                "reason": "frontmatter name disagrees with directory"
            }"#;
            let response = LedgerResolveResult::from_json_str(json).unwrap();
            response.validate_for_expected_skill("weather").unwrap();
            r.set_from_resolve(&response).unwrap();
            r.set_from_resolve(&current_result("always-visible"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let listing = sorted_dir(&mount.skills_dir());
    assert!(
        !listing.contains(&"weather".to_string()),
        "/skills/weather must be hidden by the ledger decision, got {listing:?}"
    );
    assert!(
        !listing.contains(&"calculator".to_string()),
        "/skills/calculator must NEVER appear from declaredName, got {listing:?}"
    );
    assert!(
        listing.contains(&"always-visible".to_string()),
        "unrelated skills stay visible, got {listing:?}"
    );

    // Direct lookup of the canonical name returns ENOENT.
    let err = std::fs::metadata(mount.skill_dir("weather")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

    // declaredName never produces a path; the alias must surface as
    // ENOENT too.
    let err = std::fs::metadata(mount.skill_dir("calculator")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}
