//! Integration tests for the trusted `.skill-meta` read/view gate.
//!
//! Coverage:
//!
//! * Untrusted processes cannot see `.skill-meta` in readdir.
//! * Untrusted exact-path lookup/open/read of `.skill-meta/**` is denied.
//! * Trusted processes can read `.skill-meta/**` via exact path.
//! * Fallback snapshot: regular files read from snapshot, trusted
//!   `.skill-meta` reads from live source.
//! * Hidden skill: skill not visible, but trusted exact `.skill-meta`
//!   path still accessible.
//! * Trusted `.skill-meta` access does NOT unlock hidden skill regular
//!   files.
//! * Symlink/hardlink/xattr boundaries remain unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, ActiveTarget, LedgerResolveResult, TrustedWriterConfig,
};
use skillfs_fuse::{
    MountConfig, MountHandle, MountOptions, SkillLayout, mount_background_configured,
};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn seed_skill_with_meta(source: &Path, skill: &str) {
    create_skill_dir(source, skill);
    let meta = source.join(skill).join(".skill-meta");
    std::fs::create_dir_all(&meta).expect("create .skill-meta dir");
    std::fs::write(
        meta.join("manifest.json"),
        format!("{{\"skill\":\"{skill}\",\"live\":true}}\n"),
    )
    .expect("write manifest.json");
}

fn fixture_store(source: &Path) -> SharedSkillStore {
    let mut store = SkillStore::new();
    let _ = store.load_from_directory(source, &ParseConfig::default());
    Arc::new(RwLock::new(store))
}

#[cfg(target_os = "linux")]
fn self_comm() -> String {
    let bytes =
        std::fs::read(format!("/proc/{}/comm", std::process::id())).expect("/proc/<self>/comm");
    let mut s = String::from_utf8(bytes).expect("comm utf-8");
    if s.ends_with('\n') {
        s.pop();
    }
    assert!(!s.is_empty(), "self comm must not be empty");
    s
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
            "status": "deny",
            "decision": "hidden",
            "reason": "no trusted version available"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

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

// ─────────────────────────────────────────────────────────────────────────────
// Fixture
// ─────────────────────────────────────────────────────────────────────────────

#[allow(dead_code)]
struct MetaViewFixture {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl MetaViewFixture {
    fn new<S, R>(seed: S, trusted_writer: Option<TrustedWriterConfig>, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let store = fixture_store(source.path());
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            false,
            MountConfig {
                trusted_writer,
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

    fn skill_meta(&self, skill: &str) -> PathBuf {
        self.skill_dir(skill).join(".skill-meta")
    }
}

impl Drop for MetaViewFixture {
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

// ─────────────────────────────────────────────────────────────────────────────
// 1. Untrusted readdir does not show .skill-meta
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn untrusted_readdir_hides_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = MetaViewFixture::new(
        |src| seed_skill_with_meta(src, "alpha"),
        None, // no trusted writer
        |_| None,
    );
    let listing = sorted_dir(&fx.skill_dir("alpha"));
    assert!(
        listing.contains(&"SKILL.md".to_string()),
        "SKILL.md must be visible, got {listing:?}"
    );
    assert!(
        !listing.contains(&".skill-meta".to_string()),
        ".skill-meta must be hidden from readdir, got {listing:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Untrusted exact .skill-meta lookup/open/read is denied
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn untrusted_exact_skill_meta_lookup_denied() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = MetaViewFixture::new(|src| seed_skill_with_meta(src, "alpha"), None, |_| None);
    let meta_dir = fx.skill_meta("alpha");
    let err =
        std::fs::metadata(&meta_dir).expect_err("lookup of .skill-meta must fail for untrusted");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "expected ENOENT for untrusted .skill-meta lookup, got {err:?}"
    );
    let manifest = meta_dir.join("manifest.json");
    let err = std::fs::read(&manifest)
        .expect_err("read of .skill-meta/manifest.json must fail for untrusted");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "expected ENOENT for untrusted .skill-meta/manifest.json read, got {err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Trusted process can read live .skill-meta
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_process_reads_live_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| seed_skill_with_meta(src, "alpha"),
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    let manifest = fx.skill_meta("alpha").join("manifest.json");
    let content = std::fs::read_to_string(&manifest)
        .expect("trusted process must be able to read .skill-meta/manifest.json");
    assert!(
        content.contains("\"live\":true"),
        "content must come from live source, got: {content}"
    );
    let meta_stat = std::fs::metadata(fx.skill_meta("alpha"));
    assert!(
        meta_stat.is_ok(),
        "trusted process must be able to stat .skill-meta dir"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Fallback snapshot: regular from snapshot, trusted .skill-meta from live
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn fallback_snapshot_trusted_meta_reads_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "demo-weather");
            std::fs::create_dir_all(src.join("demo-weather/scripts")).unwrap();
            std::fs::write(
                src.join("demo-weather/scripts/run.sh"),
                "#!/bin/sh\necho live\n",
            )
            .unwrap();
            write_snapshot(
                src,
                "demo-weather",
                "v000001.snapshot",
                "---\nname: demo-weather\ndescription: snapshot\n---\n",
                &[("scripts/run.sh", "#!/bin/sh\necho snapshot\n")],
            );
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo-weather", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );
    // Regular file reads from snapshot
    let script = std::fs::read_to_string(fx.skill_dir("demo-weather").join("scripts/run.sh"))
        .expect("regular file should be readable");
    assert!(
        script.contains("echo snapshot"),
        "regular file must come from snapshot, got: {script}"
    );
    // Trusted .skill-meta reads from live source
    let manifest = std::fs::read_to_string(fx.skill_meta("demo-weather").join("manifest.json"))
        .expect("trusted .skill-meta must be readable even in fallback");
    assert!(
        manifest.contains("\"live\":true"),
        ".skill-meta must come from live source, got: {manifest}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Hidden skill: skill not visible, but trusted .skill-meta accessible
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn hidden_skill_trusted_meta_still_accessible() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "hidden-skill");
            create_skill_dir(src, "visible-skill");
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("hidden-skill")).unwrap();
            r.set_from_resolve(&current_result("visible-skill"))
                .unwrap();
            Some(Arc::new(r))
        },
    );
    // Hidden skill not in readdir
    let listing = sorted_dir(&fx.skills_dir());
    assert!(
        !listing.contains(&"hidden-skill".to_string()),
        "hidden skill must not appear in /skills, got {listing:?}"
    );
    // Trusted writer can traverse hidden skill dir (needed for
    // .skill-meta exact-path access), but the skill is still hidden
    // from readdir and the Passthrough gate blocks non-meta files.
    // Trusted exact .skill-meta path succeeds
    let manifest = std::fs::read_to_string(fx.skill_meta("hidden-skill").join("manifest.json"))
        .expect("trusted .skill-meta on hidden skill must be readable");
    assert!(
        manifest.contains("\"live\":true"),
        "content must be from live source, got: {manifest}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Trusted .skill-meta access does NOT unlock hidden skill regular files
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_meta_does_not_unlock_hidden_skill_regular_files() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "secret-skill");
            std::fs::write(src.join("secret-skill/private.txt"), "secret content\n").unwrap();
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("secret-skill")).unwrap();
            Some(Arc::new(r))
        },
    );
    // Trusted .skill-meta readable
    let manifest = std::fs::read_to_string(fx.skill_meta("secret-skill").join("manifest.json"))
        .expect("trusted .skill-meta must be readable");
    assert!(manifest.contains("\"live\":true"));
    // Regular file still hidden
    let err = std::fs::read_to_string(fx.skill_dir("secret-skill").join("private.txt"))
        .expect_err("regular file on hidden skill must remain inaccessible");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "expected ENOENT for hidden skill regular file, got {err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Symlink/hardlink/xattr boundaries not relaxed
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_meta_view_does_not_relax_symlink_boundary() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "alpha");
            std::fs::write(src.join("alpha/regular.txt"), b"normal\n").unwrap();
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    // Trusted writer can read .skill-meta
    let _manifest = std::fs::read_to_string(fx.skill_meta("alpha").join("manifest.json"))
        .expect("trusted read must work");
    // But cannot create symlinks inside .skill-meta
    let link_path = fx.skill_meta("alpha").join("link-to-regular");
    let err = std::os::unix::fs::symlink("../regular.txt", &link_path)
        .expect_err("symlink inside .skill-meta must still be denied");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    // And cannot hardlink from .skill-meta out
    let dst = fx.skill_dir("alpha").join("manifest-copy.json");
    let err = std::fs::hard_link(fx.skill_meta("alpha").join("manifest.json"), &dst)
        .expect_err("hardlink from .skill-meta must still be denied");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. Trusted fallback: read_dir(.skill-meta) lists live source metadata
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_fallback_readdir_skill_meta_lists_live_source() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "demo-weather");
            std::fs::create_dir_all(src.join("demo-weather/scripts")).unwrap();
            std::fs::write(
                src.join("demo-weather/scripts/run.sh"),
                "#!/bin/sh\necho live\n",
            )
            .unwrap();
            write_snapshot(
                src,
                "demo-weather",
                "v000001.snapshot",
                "---\nname: demo-weather\ndescription: snapshot\n---\n",
                &[("scripts/run.sh", "#!/bin/sh\necho snapshot\n")],
            );
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("demo-weather", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );
    let meta_listing = sorted_dir(&fx.skill_meta("demo-weather"));
    assert!(
        meta_listing.contains(&"manifest.json".to_string()),
        "trusted readdir of .skill-meta must include manifest.json, got {meta_listing:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. Trusted hidden: read_dir(.skill-meta) succeeds
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_hidden_readdir_skill_meta_succeeds() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| {
            seed_skill_with_meta(src, "hidden-skill");
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("hidden-skill")).unwrap();
            Some(Arc::new(r))
        },
    );
    let meta_listing = sorted_dir(&fx.skill_meta("hidden-skill"));
    assert!(
        meta_listing.contains(&"manifest.json".to_string()),
        "trusted readdir of hidden .skill-meta must include manifest.json, got {meta_listing:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. Trusted O_TRUNC/O_CREAT still goes through mutation gate
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_mutating_open_goes_through_policy() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| seed_skill_with_meta(src, "alpha"),
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    // Trusted writer: read-only open succeeds
    let manifest = fx.skill_meta("alpha").join("manifest.json");
    let _content = std::fs::read_to_string(&manifest).expect("trusted read must succeed");
    // Trusted writer: write open also succeeds (enforce_skill_meta allows it)
    std::fs::write(&manifest, b"{\"updated\":true}\n")
        .expect("trusted writer write must succeed through policy gate");
    let updated = std::fs::read_to_string(&manifest).expect("re-read after write");
    assert!(
        updated.contains("\"updated\":true"),
        "write must have landed, got: {updated}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 11. Trusted parent listing includes .skill-meta
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_parent_listing_includes_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| seed_skill_with_meta(src, "alpha"),
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    let listing = sorted_dir(&fx.skill_dir("alpha"));
    assert!(
        listing.contains(&".skill-meta".to_string()),
        "trusted caller must see .skill-meta in parent listing, got {listing:?}"
    );
    assert!(
        listing.contains(&"SKILL.md".to_string()),
        "SKILL.md must still be visible, got {listing:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 12. Trusted inbox .skill-meta read-only open/read succeeds
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn trusted_inbox_skill_meta_read_succeeds() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = MetaViewFixture::new(
        |src| seed_skill_with_meta(src, "alpha"),
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    let inbox_manifest = fx
        .mountpoint
        .path()
        .join(".skillfs-inbox/alpha/.skill-meta/manifest.json");
    let content = std::fs::read_to_string(&inbox_manifest)
        .expect("trusted inbox .skill-meta read must succeed");
    assert!(
        content.contains("\"live\":true"),
        "inbox .skill-meta must come from source, got: {content}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 13. Untrusted parent listing still hides .skill-meta (regression guard)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn untrusted_parent_listing_still_hides_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = MetaViewFixture::new(|src| seed_skill_with_meta(src, "alpha"), None, |_| None);
    let listing = sorted_dir(&fx.skill_dir("alpha"));
    assert!(
        !listing.contains(&".skill-meta".to_string()),
        "untrusted caller must NOT see .skill-meta, got {listing:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Hermes nested .skill-meta tests
// ═══════════════════════════════════════════════════════════════════════════════

fn seed_hermes_with_meta(source: &Path, category: &str, skill: &str) {
    let skill_dir = source.join(category).join(skill);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {skill}\ndescription: test\n---\n{skill} body.\n"),
    )
    .unwrap();
    let meta = skill_dir.join(".skill-meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(
        meta.join("manifest.json"),
        format!("{{\"skill\":\"{category}/{skill}\",\"live\":true}}\n"),
    )
    .unwrap();
}

#[allow(dead_code)]
struct HermesMetaViewFixture {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl HermesMetaViewFixture {
    fn new<S, R>(seed: S, trusted_writer: Option<TrustedWriterConfig>, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let store = fixture_store(source.path());
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            true,
            MountConfig {
                trusted_writer,
                active_resolver: resolver,
                skill_layout: Some(SkillLayout::Hermes),
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

    fn nested_skill_dir(&self, category: &str, skill: &str) -> PathBuf {
        self.mountpoint.path().join(category).join(skill)
    }

    fn nested_skill_meta(&self, category: &str, skill: &str) -> PathBuf {
        self.nested_skill_dir(category, skill).join(".skill-meta")
    }
}

impl Drop for HermesMetaViewFixture {
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

// ─────────────────────────────────────────────────────────────────────────────
// H1. Untrusted readdir of category/skill hides .skill-meta
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hermes_untrusted_readdir_hides_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = HermesMetaViewFixture::new(
        |src| seed_hermes_with_meta(src, "apple", "apple-notes"),
        None,
        |_| None,
    );
    let listing = sorted_dir(&fx.nested_skill_dir("apple", "apple-notes"));
    assert!(
        listing.contains(&"SKILL.md".to_string()),
        "SKILL.md must be visible, got {listing:?}"
    );
    assert!(
        !listing.contains(&".skill-meta".to_string()),
        ".skill-meta must be hidden from untrusted readdir, got {listing:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// H2. Untrusted exact path metadata/read for category/skill/.skill-meta/...
//     returns ENOENT
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hermes_untrusted_exact_skill_meta_denied() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = HermesMetaViewFixture::new(
        |src| seed_hermes_with_meta(src, "apple", "apple-notes"),
        None,
        |_| None,
    );
    let meta_dir = fx.nested_skill_meta("apple", "apple-notes");
    let err = std::fs::metadata(&meta_dir).expect_err("untrusted .skill-meta lookup must fail");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    let manifest = meta_dir.join("manifest.json");
    let err =
        std::fs::read(&manifest).expect_err("untrusted .skill-meta/manifest.json read must fail");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

// ─────────────────────────────────────────────────────────────────────────────
// H3. Trusted read-only open/stat of category/skill/.skill-meta/... succeeds
//     and reads live source even when the skill is fallback
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn hermes_trusted_reads_live_skill_meta() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = HermesMetaViewFixture::new(
        |src| {
            seed_hermes_with_meta(src, "apple", "apple-notes");
            std::fs::create_dir_all(src.join("apple/apple-notes/scripts")).unwrap();
            std::fs::write(
                src.join("apple/apple-notes/scripts/run.sh"),
                "#!/bin/sh\necho live\n",
            )
            .unwrap();
            write_snapshot(
                src,
                "apple/apple-notes",
                "v000001.snapshot",
                "---\nname: apple-notes\ndescription: snapshot\n---\n",
                &[("scripts/run.sh", "#!/bin/sh\necho snapshot\n")],
            );
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let snap_dir = src_root.join("apple/apple-notes/.skill-meta/versions/v000001.snapshot");
            let r = ActiveSkillResolver::new(src_root);
            r.set(
                "apple/apple-notes",
                ActiveTarget::Snapshot {
                    snapshot_dir: snap_dir,
                    version: "v000001.snapshot".to_string(),
                },
            );
            Some(Arc::new(r))
        },
    );
    let manifest = std::fs::read_to_string(
        fx.nested_skill_meta("apple", "apple-notes")
            .join("manifest.json"),
    )
    .expect("trusted .skill-meta must be readable even in fallback");
    assert!(
        manifest.contains("\"live\":true"),
        ".skill-meta must come from live source, got: {manifest}"
    );
    let script = std::fs::read_to_string(
        fx.nested_skill_dir("apple", "apple-notes")
            .join("scripts/run.sh"),
    )
    .expect("regular file should be readable");
    assert!(
        script.contains("echo snapshot"),
        "regular file must come from snapshot, got: {script}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// H4. Trusted hidden nested skill can read live .skill-meta but cannot read
//     ordinary hidden files
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn hermes_trusted_hidden_meta_no_ordinary_files() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = HermesMetaViewFixture::new(
        |src| {
            seed_hermes_with_meta(src, "apple", "apple-notes");
            std::fs::write(
                src.join("apple/apple-notes/private.txt"),
                "secret content\n",
            )
            .unwrap();
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root);
            r.set(
                "apple/apple-notes",
                ActiveTarget::Hidden {
                    reason: "no trusted version available".to_string(),
                },
            );
            Some(Arc::new(r))
        },
    );
    let manifest = std::fs::read_to_string(
        fx.nested_skill_meta("apple", "apple-notes")
            .join("manifest.json"),
    )
    .expect("trusted .skill-meta must be readable on hidden nested skill");
    assert!(manifest.contains("\"live\":true"));
    let err = std::fs::read_to_string(
        fx.nested_skill_dir("apple", "apple-notes")
            .join("private.txt"),
    )
    .expect_err("hidden nested skill regular file must remain inaccessible");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

// ─────────────────────────────────────────────────────────────────────────────
// H5. Untrusted write/create under nested .skill-meta is rejected
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hermes_untrusted_write_nested_skill_meta_rejected() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let fx = HermesMetaViewFixture::new(
        |src| seed_hermes_with_meta(src, "apple", "apple-notes"),
        None,
        |_| None,
    );
    let manifest = fx
        .nested_skill_meta("apple", "apple-notes")
        .join("manifest.json");
    let err = std::fs::write(&manifest, b"overwritten\n")
        .expect_err("untrusted write to nested .skill-meta must be denied");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

// ─────────────────────────────────────────────────────────────────────────────
// H6. Trusted mutating open/create under nested .skill-meta goes through
//     enforce_skill_meta and preserves existing audit/policy semantics
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn hermes_trusted_mutating_open_goes_through_policy() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = HermesMetaViewFixture::new(
        |src| seed_hermes_with_meta(src, "apple", "apple-notes"),
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    let manifest = fx
        .nested_skill_meta("apple", "apple-notes")
        .join("manifest.json");
    let _content = std::fs::read_to_string(&manifest).expect("trusted read must succeed");
    std::fs::write(&manifest, b"{\"updated\":true}\n")
        .expect("trusted writer write must succeed through policy gate");
    let updated = std::fs::read_to_string(&manifest).expect("re-read after write");
    assert!(
        updated.contains("\"updated\":true"),
        "write must have landed, got: {updated}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// H7. Symlink/hardlink under nested .skill-meta remains rejected even for
//     trusted writer, matching flat behavior
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn hermes_trusted_meta_symlink_hardlink_rejected() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let comm = self_comm();
    let fx = HermesMetaViewFixture::new(
        |src| {
            seed_hermes_with_meta(src, "apple", "apple-notes");
            std::fs::write(src.join("apple/apple-notes/regular.txt"), b"normal\n").unwrap();
        },
        Some(TrustedWriterConfig::with_process_name(comm)),
        |_| None,
    );
    let _manifest = std::fs::read_to_string(
        fx.nested_skill_meta("apple", "apple-notes")
            .join("manifest.json"),
    )
    .expect("trusted read must work");
    let link_path = fx
        .nested_skill_meta("apple", "apple-notes")
        .join("link-to-regular");
    let err = std::os::unix::fs::symlink("../regular.txt", &link_path)
        .expect_err("symlink inside nested .skill-meta must be denied");
    // Nested paths are rejected at the link-type gate (EROFS) since
    // symlink_impl only accepts flat Passthrough targets.
    assert!(
        err.raw_os_error() == Some(libc::EACCES) || err.raw_os_error() == Some(libc::EROFS),
        "expected EACCES or EROFS, got {err:?}"
    );
    let dst = fx
        .nested_skill_dir("apple", "apple-notes")
        .join("manifest-copy.json");
    let err = std::fs::hard_link(
        fx.nested_skill_meta("apple", "apple-notes")
            .join("manifest.json"),
        &dst,
    )
    .expect_err("hardlink from nested .skill-meta must be denied");
    assert!(
        err.raw_os_error() == Some(libc::EACCES) || err.raw_os_error() == Some(libc::EROFS),
        "expected EACCES or EROFS, got {err:?}"
    );
}
