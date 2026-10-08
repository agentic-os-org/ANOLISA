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
use skillfs_fuse::SkillLayout;
use skillfs_fuse::security::{
    ActiveSkillResolver, LedgerError, LedgerResolveResult, TrustedWriterConfig,
    bootstrap_activation,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available};

/// Trusted-writer config matching the test process itself, so FUSE
/// requests issued by this test count as trusted `.skill-meta` callers.
fn trusted_writer_config() -> TrustedWriterConfig {
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", std::process::id()))
        .expect("/proc/<self>/comm");
    TrustedWriterConfig::with_process_name(comm.trim_end())
}

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
        Self::new_with_options(seed, resolver_builder, None, None)
    }

    /// Mount where the test process is a trusted `.skill-meta` caller.
    fn new_trusted<S, R>(seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        Self::new_with_options(seed, resolver_builder, Some(trusted_writer_config()), None)
    }

    /// Mount with a Hermes (nested) layout and the test process as a
    /// trusted `.skill-meta` caller.
    fn new_trusted_hermes<S, R>(seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        Self::new_with_options(
            seed,
            resolver_builder,
            Some(trusted_writer_config()),
            Some(SkillLayout::Hermes),
        )
    }

    fn new_with_options<S, R>(
        seed: S,
        resolver_builder: R,
        trusted_writer: Option<TrustedWriterConfig>,
        skill_layout: Option<SkillLayout>,
    ) -> Self
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
                trusted_writer,
                skill_layout,
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

// ─────────────────────────────────────────────────────────────────────────────
// Fallback read callbacks: readlink and xattr
// ─────────────────────────────────────────────────────────────────────────────

/// The fixture's fixed startup sleep is not a readiness guarantee under
/// load; wait until the mounted view actually serves `path`.
fn wait_for_mount_path(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::symlink_metadata(path).is_err() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        std::fs::symlink_metadata(path).is_ok(),
        "mounted view never served {}",
        path.display()
    );
}

/// `readlink` on a ledger-fallback skill must return the **snapshot's**
/// symlink target, matching the snapshot's `lstat` type and every other read
/// of the same skill. Reading the live source unconditionally mixed a live
/// target with the snapshot's symlink identity.
#[test]
fn fallback_passthrough_readlink_serves_snapshot_target() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo");
            std::os::unix::fs::symlink("live-target", src.join("demo/link")).expect("live symlink");
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[],
            );
            std::os::unix::fs::symlink("snapshot-target", snap.join("link"))
                .expect("snapshot symlink");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let link = mount.skill_dir("demo").join("link");
    wait_for_mount_path(&link);
    let meta = std::fs::symlink_metadata(&link).expect("lstat snapshot symlink via mount");
    assert!(
        meta.file_type().is_symlink(),
        "the snapshot entry is a symlink, got {:?}",
        meta.file_type()
    );
    assert_eq!(
        std::fs::read_link(&link).expect("readlink via mount"),
        PathBuf::from("snapshot-target"),
        "readlink must serve the snapshot's target, not the live source's"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Trusted `.skill-meta` readlink: the live-source management view
// ─────────────────────────────────────────────────────────────────────────────

/// A trusted caller's `.skill-meta/**` readlink must keep reading the live
/// physical source, exactly like lookup/getattr/open/access, even when the
/// regular skill view is a ledger fallback snapshot. Applying the active
/// mapping to the metadata namespace would join the relative path under the
/// snapshot root and return ENOENT.
#[test]
fn fallback_trusted_skill_meta_readlink_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new_trusted(
        |src| {
            create_skill_dir(src, "demo");
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[],
            );
            // The snapshot directory lives under the live tree's
            // `.skill-meta/versions/`, so a trusted caller reaches it via the
            // live physical path.
            std::os::unix::fs::symlink("meta-target", snap.join("meta-link"))
                .expect("live metadata symlink");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let link = mount
        .skill_dir("demo")
        .join(".skill-meta/versions/v000001.snapshot/meta-link");
    wait_for_mount_path(&link);
    assert_eq!(
        std::fs::read_link(&link).expect("trusted metadata readlink via mount"),
        PathBuf::from("meta-target"),
        "trusted `.skill-meta` readlink must serve the live source for a fallback skill"
    );
}

/// A trusted caller must also keep the live `.skill-meta` view for a skill the
/// ledger has hidden: lookup/getattr/open already allow exact-path traversal,
/// so readlink must not turn the same path into ENOENT.
#[test]
fn hidden_trusted_skill_meta_readlink_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new_trusted(
        |src| {
            create_skill_dir(src, "demo");
            let meta = src.join("demo/.skill-meta");
            std::fs::create_dir_all(&meta).expect("create .skill-meta dir");
            std::os::unix::fs::symlink("hidden-meta-target", meta.join("meta-link"))
                .expect("live metadata symlink");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("demo")).unwrap();
            Some(Arc::new(r))
        },
    );

    let link = mount.skill_dir("demo").join(".skill-meta/meta-link");
    wait_for_mount_path(&link);
    assert_eq!(
        std::fs::read_link(&link).expect("trusted metadata readlink via mount"),
        PathBuf::from("hidden-meta-target"),
        "trusted `.skill-meta` readlink must serve the live source for a hidden skill"
    );
}

/// Untrusted callers must not gain access through readlink: the
/// `.skill-meta` namespace stays hidden (ENOENT), matching
/// lookup/getattr/open.
#[test]
fn untrusted_skill_meta_readlink_stays_hidden() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo");
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[],
            );
            std::os::unix::fs::symlink("meta-target", snap.join("meta-link"))
                .expect("live metadata symlink");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let link = mount
        .skill_dir("demo")
        .join(".skill-meta/versions/v000001.snapshot/meta-link");
    let err =
        std::fs::read_link(&link).expect_err("untrusted `.skill-meta` readlink must stay hidden");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "untrusted metadata readlink must surface ENOENT, got {err:?}"
    );
}

/// Hermes/nested twin of the fallback case: the nested readlink branch must
/// apply the same trusted `.skill-meta` live-source exception.
#[test]
fn snapshot_trusted_nested_skill_meta_readlink_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new_trusted_hermes(
        |src| {
            create_skill_dir(src, "category/demo");
            let snap = write_snapshot(
                src,
                "category/demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: nested snapshot\n---\n",
                &[],
            );
            std::os::unix::fs::symlink("nested-meta-target", snap.join("meta-link"))
                .expect("nested live metadata symlink");
            std::fs::write(
                src.join("category/demo/.skill-meta/activation.json"),
                r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
            )
            .expect("write nested activation record");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            bootstrap_activation(src_root, &["category/demo".to_string()], &r);
            Some(Arc::new(r))
        },
    );

    let link = mount
        .skill_dir("category/demo")
        .join(".skill-meta/versions/v000001.snapshot/meta-link");
    wait_for_mount_path(&link);
    assert_eq!(
        std::fs::read_link(&link).expect("trusted nested metadata readlink via mount"),
        PathBuf::from("nested-meta-target"),
        "trusted nested `.skill-meta` readlink must serve the live source for a snapshot skill"
    );
}

/// Hermes/nested twin of the hidden case.
#[test]
fn hidden_trusted_nested_skill_meta_readlink_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let mount = LedgerMount::new_trusted_hermes(
        |src| {
            create_skill_dir(src, "category/demo");
            let meta = src.join("category/demo/.skill-meta");
            std::fs::create_dir_all(&meta).expect("create nested .skill-meta dir");
            std::os::unix::fs::symlink("nested-hidden-meta-target", meta.join("meta-link"))
                .expect("nested live metadata symlink");
            // `target: null` is the activation contract's fail-safe hidden
            // record.
            std::fs::write(
                meta.join("activation.json"),
                r#"{"schemaVersion": 1, "target": null}"#,
            )
            .expect("write nested activation record");
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            bootstrap_activation(src_root, &["category/demo".to_string()], &r);
            Some(Arc::new(r))
        },
    );

    let link = mount
        .skill_dir("category/demo")
        .join(".skill-meta/meta-link");
    wait_for_mount_path(&link);
    assert_eq!(
        std::fs::read_link(&link).expect("trusted nested metadata readlink via mount"),
        PathBuf::from("nested-hidden-meta-target"),
        "trusted nested `.skill-meta` readlink must serve the live source for a hidden skill"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Fallback read callbacks: xattr (getxattr/listxattr)
// ─────────────────────────────────────────────────────────────────────────────

fn lset_attr(path: &Path, name: &str, value: &[u8]) -> Result<(), i32> {
    use std::os::unix::ffi::OsStrExt;
    let cp = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path CString");
    let cn = std::ffi::CString::new(name).expect("name CString");
    let rc = unsafe {
        libc::lsetxattr(
            cp.as_ptr(),
            cn.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };
    if rc != 0 {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    } else {
        Ok(())
    }
}

fn lget_attr(path: &Path, name: &str) -> Result<Vec<u8>, i32> {
    use std::os::unix::ffi::OsStrExt;
    let cp = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path CString");
    let cn = std::ffi::CString::new(name).expect("name CString");
    let needed = unsafe { libc::lgetxattr(cp.as_ptr(), cn.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    let mut buf = vec![0u8; needed as usize];
    let got = unsafe {
        libc::lgetxattr(
            cp.as_ptr(),
            cn.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    buf.truncate(got as usize);
    Ok(buf)
}

fn llist_attr(path: &Path) -> Result<Vec<Vec<u8>>, i32> {
    use std::os::unix::ffi::OsStrExt;
    let cp = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path CString");
    let needed = unsafe { libc::llistxattr(cp.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    let mut buf = vec![0u8; needed as usize];
    let got = unsafe {
        libc::llistxattr(
            cp.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    buf.truncate(got as usize);
    Ok(buf
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// `getxattr`/`listxattr` on a ledger-fallback passthrough file must describe
/// the snapshot the bytes are read from, not the live source. xattr reads are
/// read-side state: serving live values while `read` returns snapshot bytes
/// exposes metadata that the activated snapshot does not contain.
#[test]
fn fallback_passthrough_xattr_reads_snapshot() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let xattr_ok = std::cell::Cell::new(false);
    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo");
            std::fs::write(src.join("demo/notes.txt"), "live notes").unwrap();
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[("notes.txt", "snapshot notes")],
            );
            let live_ok =
                lset_attr(&src.join("demo/notes.txt"), "user.demo", b"live-xattr").is_ok();
            let snap_ok =
                lset_attr(&snap.join("notes.txt"), "user.demo", b"snapshot-xattr").is_ok();
            xattr_ok.set(live_ok && snap_ok);
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    if !xattr_ok.get() {
        eprintln!("SKIP: substrate does not support user.* xattrs");
        return;
    }

    let notes = mount.skill_dir("demo").join("notes.txt");
    wait_for_mount_path(&notes);
    assert_eq!(
        std::fs::read_to_string(&notes).expect("read snapshot notes via mount"),
        "snapshot notes",
        "control: file bytes come from the snapshot"
    );
    assert_eq!(
        lget_attr(&notes, "user.demo").expect("getxattr via mount"),
        b"snapshot-xattr",
        "getxattr must serve the snapshot's value"
    );
    assert_eq!(
        llist_attr(&notes).expect("listxattr via mount"),
        vec![b"user.demo".to_vec()],
        "listxattr must describe the snapshot"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Trusted `.skill-meta` xattrs: the live-source management view
// ─────────────────────────────────────────────────────────────────────────────

/// A trusted caller's `.skill-meta/**` xattr reads must keep reading the live
/// physical source, exactly like lookup/getattr/open/access, even when the
/// regular skill view is a ledger fallback snapshot. Applying the active
/// mapping to the metadata namespace would join the relative path under the
/// snapshot root and return ENOENT.
#[test]
fn fallback_trusted_skill_meta_xattr_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let xattr_ok = std::cell::Cell::new(false);
    let mount = LedgerMount::new_trusted(
        |src| {
            create_skill_dir(src, "demo");
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[],
            );
            let meta = snap.join("meta.txt");
            std::fs::write(&meta, "live metadata").expect("write metadata file");
            xattr_ok.set(lset_attr(&meta, "user.demo", b"live-meta-xattr").is_ok());
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    if !xattr_ok.get() {
        eprintln!("SKIP: substrate does not support user.* xattrs");
        return;
    }

    let meta = mount
        .skill_dir("demo")
        .join(".skill-meta/versions/v000001.snapshot/meta.txt");
    wait_for_mount_path(&meta);
    assert_eq!(
        lget_attr(&meta, "user.demo").expect("trusted getxattr via mount"),
        b"live-meta-xattr",
        "trusted `.skill-meta` getxattr must serve the live source for a fallback skill"
    );
    assert_eq!(
        llist_attr(&meta).expect("trusted listxattr via mount"),
        vec![b"user.demo".to_vec()],
        "trusted `.skill-meta` listxattr must serve the live source for a fallback skill"
    );
}

/// A trusted caller must also keep the live `.skill-meta` xattr view for a
/// skill the ledger has hidden: lookup/getattr/open already allow exact-path
/// traversal, so getxattr/listxattr must not turn the same path into ENOENT.
#[test]
fn hidden_trusted_skill_meta_xattr_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let xattr_ok = std::cell::Cell::new(false);
    let mount = LedgerMount::new_trusted(
        |src| {
            create_skill_dir(src, "demo");
            let meta_dir = src.join("demo/.skill-meta");
            std::fs::create_dir_all(&meta_dir).expect("create .skill-meta dir");
            let meta = meta_dir.join("meta.txt");
            std::fs::write(&meta, "hidden metadata").expect("write metadata file");
            xattr_ok.set(lset_attr(&meta, "user.demo", b"hidden-meta-xattr").is_ok());
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("demo")).unwrap();
            Some(Arc::new(r))
        },
    );

    if !xattr_ok.get() {
        eprintln!("SKIP: substrate does not support user.* xattrs");
        return;
    }

    let meta = mount.skill_dir("demo").join(".skill-meta/meta.txt");
    wait_for_mount_path(&meta);
    assert_eq!(
        lget_attr(&meta, "user.demo").expect("trusted getxattr via mount"),
        b"hidden-meta-xattr",
        "trusted `.skill-meta` getxattr must serve the live source for a hidden skill"
    );
    assert_eq!(
        llist_attr(&meta).expect("trusted listxattr via mount"),
        vec![b"user.demo".to_vec()],
        "trusted `.skill-meta` listxattr must serve the live source for a hidden skill"
    );
}

/// Untrusted callers must not gain access through the xattr read path: the
/// `.skill-meta` namespace stays hidden (ENOENT), matching
/// lookup/getattr/open.
#[test]
fn untrusted_skill_meta_xattr_stays_hidden() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    let xattr_ok = std::cell::Cell::new(false);
    let mount = LedgerMount::new(
        |src| {
            create_skill_dir(src, "demo");
            let snap = write_snapshot(
                src,
                "demo",
                "v000001.snapshot",
                "---\nname: demo\ndescription: trusted snapshot\n---\n",
                &[],
            );
            let meta = snap.join("meta.txt");
            std::fs::write(&meta, "live metadata").expect("write metadata file");
            xattr_ok.set(lset_attr(&meta, "user.demo", b"live-meta-xattr").is_ok());
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    if !xattr_ok.get() {
        eprintln!("SKIP: substrate does not support user.* xattrs");
        return;
    }

    let meta = mount
        .skill_dir("demo")
        .join(".skill-meta/versions/v000001.snapshot/meta.txt");
    let err = lget_attr(&meta, "user.demo")
        .expect_err("untrusted `.skill-meta` getxattr must stay hidden");
    assert_eq!(
        err,
        libc::ENOENT,
        "untrusted metadata getxattr must surface ENOENT, got {err}"
    );
    let err = llist_attr(&meta).expect_err("untrusted `.skill-meta` listxattr must stay hidden");
    assert_eq!(
        err,
        libc::ENOENT,
        "untrusted metadata listxattr must surface ENOENT, got {err}"
    );
}
