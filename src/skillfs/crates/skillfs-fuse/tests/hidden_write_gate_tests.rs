//! Integration tests: the I4/H3 hidden-skill write gate is enforced by
//! every mutating callback.
//!
//! The gate (`SkillFs::should_reject_hidden_write`) is complete in
//! `rename` (`SkillMd` arm) and `setattr`, but historically drifted:
//! `unlink` lacked the flat `SkillMd` arm, `rmdir`/`rename` lacked the
//! `SkillDir` arms, `create` lacked the flat `SkillMd` arm, and
//! `symlink`/`link` had no gate at all. A ledger flip to `hidden` does
//! not evict the kernel's warm dentries (1s entry timeout), so the
//! stale-dentry window let callers delete / move / link a hidden
//! skill's manifest and directory through the mount.
//!
//! Each scenario warms the dentries while the skill resolves `current`,
//! flips the resolver to `hidden` in place, and then mutates
//! immediately — exactly the stale-dentry race the gate must close.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{ActiveSkillResolver, LedgerResolveResult};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::create_skill_dir;

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

/// Normal-mode mount whose resolver stays reachable so a test can flip
/// a skill to `hidden` after dentries are warm.
struct FlipMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    resolver: Arc<ActiveSkillResolver>,
    _handle: MountHandle,
}

impl FlipMount {
    fn new_current(skill: &str) -> Self {
        Self::new_current_multi(&[skill])
    }

    fn new_current_multi(skills: &[&str]) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        let resolver = Arc::new(ActiveSkillResolver::new(source.path().to_path_buf()));
        for skill in skills {
            create_skill_dir(source.path(), skill);
            std::fs::write(source.path().join(skill).join("target.txt"), b"target\n")
                .expect("seed target.txt");
            resolver
                .set_from_resolve(&current_result(skill))
                .expect("seed current");
        }

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            resolver,
            _handle: handle,
        }
    }

    fn flip_hidden(&self, skill: &str) {
        self.resolver
            .set_from_resolve(&hidden_result(skill))
            .expect("flip to hidden");
    }

    fn skill(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }

    fn source_skill(&self, name: &str) -> PathBuf {
        self.source.path().join(name)
    }
}

/// Warm the kernel dentries for the skill dir and its manifest while
/// the skill still resolves `current`.
fn warm_dentries(fx: &FlipMount, skill: &str) {
    let md = fx.skill(skill).join("SKILL.md");
    let body = std::fs::read_to_string(&md).expect("warm read of manifest");
    assert!(body.contains(skill), "manifest body sanity: {body:?}");
    std::fs::metadata(fx.skill(skill)).expect("warm stat of skill dir");
    std::fs::metadata(fx.skill(skill).join("target.txt")).expect("warm stat of target.txt");
}

#[test]
fn hidden_manifest_unlink_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_manifest_unlink_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let err = std::fs::remove_file(fx.skill("hidden-skill").join("SKILL.md"))
        .expect_err("unlink of hidden manifest must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden manifest unlink must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("hidden-skill").join("SKILL.md").is_file(),
        "physical manifest must survive the rejected unlink"
    );
}

#[test]
fn hidden_skill_dir_rmdir_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_skill_dir_rmdir_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    // The physical dir is non-empty (SKILL.md + target.txt). The
    // unguarded path would surface the physical ENOTEMPTY; the gate
    // must hide the skill entirely with ENOENT.
    let err = std::fs::remove_dir(fx.skill("hidden-skill"))
        .expect_err("rmdir of hidden skill dir must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden skill dir rmdir must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("hidden-skill").is_dir(),
        "physical skill dir must survive the rejected rmdir"
    );
}

#[test]
fn hidden_skill_dir_rename_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_skill_dir_rename_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let err = std::fs::rename(fx.skill("hidden-skill"), fx.skill("hidden-moved"))
        .expect_err("rename of hidden skill dir must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden skill dir rename must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("hidden-skill").join("SKILL.md").is_file(),
        "physical skill dir must not move"
    );
    assert!(
        !fx.source_skill("hidden-moved").exists(),
        "no rename target may appear"
    );
}

/// Whole-skill rename of a current skill to a not-yet-existing target
/// directory must keep working: the new name has no resolver entry and
/// a missing entry resolves `Hidden`, so the target-side gate must not
/// mistake the free name for an existing hidden skill. The rename has
/// to pass, land on the physical source, and re-key the store.
#[test]
fn current_skill_dir_rename_to_free_target_succeeds() {
    if !common::fuse_available() {
        eprintln!("SKIP current_skill_dir_rename_to_free_target_succeeds: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("live-skill");
    warm_dentries(&fx, "live-skill");

    std::fs::rename(fx.skill("live-skill"), fx.skill("live-renamed"))
        .expect("whole-skill rename to a free target must pass the hidden gate");

    // The physical directory moved on the source tree.
    assert!(
        !fx.source_skill("live-skill").exists(),
        "the old physical skill dir must be gone after the rename"
    );
    assert!(
        fx.source_skill("live-renamed").join("SKILL.md").is_file(),
        "the manifest must live under the new physical name"
    );
    assert!(
        fx.source_skill("live-renamed").join("target.txt").is_file(),
        "the skill payload must move with the directory"
    );

    // The store re-keyed under the new name: once the resolver installs
    // a Current target for it (the post-rename resolve cycle), the new
    // path serves the moved content and the old path is gone.
    fx.resolver
        .set_from_resolve(&current_result("live-renamed"))
        .expect("seed current for renamed skill");
    let body = std::fs::read_to_string(fx.skill("live-renamed").join("SKILL.md"))
        .expect("read manifest under the new name");
    assert!(
        body.contains("live-skill"),
        "manifest body sanity after rename: {body:?}"
    );
    let old = std::fs::metadata(fx.skill("live-skill"));
    assert_eq!(
        old.unwrap_err().raw_os_error(),
        Some(libc::ENOENT),
        "the old skill path must be gone through the mount"
    );
}

/// Renaming a current skill ONTO an existing hidden skill stays
/// rejected: that target really exists on disk, so the hidden gate
/// must keep refusing it with `ENOENT` and nothing may move.
#[test]
fn current_skill_dir_rename_onto_existing_hidden_target_is_rejected() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP current_skill_dir_rename_onto_existing_hidden_target_is_rejected: FUSE not available"
        );
        return;
    }
    let fx = FlipMount::new_current_multi(&["live-skill", "occupied-skill"]);
    warm_dentries(&fx, "live-skill");
    warm_dentries(&fx, "occupied-skill");
    fx.flip_hidden("occupied-skill");

    let err = std::fs::rename(fx.skill("live-skill"), fx.skill("occupied-skill"))
        .expect_err("rename onto an existing hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "rename onto an existing hidden target must be ENOENT, got {err:?}"
    );
    assert!(
        fx.source_skill("live-skill").join("SKILL.md").is_file(),
        "the current source skill must not move"
    );
    assert!(
        fx.source_skill("occupied-skill").join("SKILL.md").is_file(),
        "the hidden target skill must stay in place"
    );
}

#[test]
fn hidden_manifest_create_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_manifest_create_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");

    // Remove the manifest while the skill is still current — the
    // standard repair flow — then flip. Recreating the manifest must be
    // rejected while hidden.
    std::fs::remove_file(fx.skill("hidden-skill").join("SKILL.md")).expect("unlink while current");
    fx.flip_hidden("hidden-skill");

    let err = std::fs::write(fx.skill("hidden-skill").join("SKILL.md"), b"resurrect\n")
        .expect_err("create of hidden manifest must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden manifest create must be ENOENT, got {err:?}"
    );
    assert!(
        !fx.source_skill("hidden-skill").join("SKILL.md").exists(),
        "no physical manifest may be created for the hidden skill"
    );
}

#[test]
fn hidden_symlink_into_hidden_skill_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_symlink_into_hidden_skill_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let dst = fx.skill("hidden-skill").join("new-link");
    let err = std::os::unix::fs::symlink("target.txt", &dst)
        .expect_err("symlink into hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "symlink into hidden skill must be ENOENT, got {err:?}"
    );
    assert!(
        !fx.source_skill("hidden-skill").join("new-link").exists(),
        "no physical symlink may appear inside the hidden skill"
    );
}

#[test]
fn hidden_hardlink_into_hidden_skill_is_rejected() {
    if !common::fuse_available() {
        eprintln!("SKIP hidden_hardlink_into_hidden_skill_is_rejected: FUSE not available");
        return;
    }
    let fx = FlipMount::new_current("hidden-skill");
    warm_dentries(&fx, "hidden-skill");
    fx.flip_hidden("hidden-skill");

    let src = fx.skill("hidden-skill").join("target.txt");
    let dst = fx.skill("hidden-skill").join("link.txt");
    let err =
        std::fs::hard_link(&src, &dst).expect_err("hardlink into hidden skill must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hardlink into hidden skill must be ENOENT, got {err:?}"
    );
    assert!(
        !fx.source_skill("hidden-skill").join("link.txt").exists(),
        "no physical hardlink may appear inside the hidden skill"
    );
}

#[test]
fn current_skill_mutations_still_passthrough() {
    if !common::fuse_available() {
        eprintln!("SKIP current_skill_mutations_still_passthrough: FUSE not available");
        return;
    }
    // Control: the same flows keep working for a skill that resolves
    // current, so the completed gate cannot overreach.
    let fx = FlipMount::new_current("live-skill");
    warm_dentries(&fx, "live-skill");

    std::fs::write(fx.skill("live-skill").join("created.txt"), b"created\n")
        .expect("create passthrough file in current skill");
    std::os::unix::fs::symlink("target.txt", fx.skill("live-skill").join("new-link"))
        .expect("symlink into current skill");
    std::fs::hard_link(
        fx.skill("live-skill").join("target.txt"),
        fx.skill("live-skill").join("link.txt"),
    )
    .expect("hardlink into current skill");
    std::fs::rename(
        fx.skill("live-skill").join("created.txt"),
        fx.skill("live-skill").join("renamed.txt"),
    )
    .expect("rename inside current skill");
    std::fs::remove_file(fx.skill("live-skill").join("SKILL.md"))
        .expect("unlink manifest of current skill");
    std::fs::write(
        fx.skill("live-skill").join("SKILL.md"),
        b"---\nname: live-skill\n---\n",
    )
    .expect("recreate manifest of current skill");

    assert!(fx.source_skill("live-skill").join("renamed.txt").is_file());
    assert!(fx.source_skill("live-skill").join("new-link").is_symlink());
    assert!(fx.source_skill("live-skill").join("link.txt").is_file());
}
