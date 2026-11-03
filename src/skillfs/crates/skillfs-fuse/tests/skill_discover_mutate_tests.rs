//! Integration tests: the `skill-discover` virtual namespace is read-only
//! for the namespace-mutation callbacks (`mkdir`/`unlink`/`rmdir`/`rename`).
//!
//! `write`/`open`/`setattr`/`symlink`/`link`/`mknod`/`xattr`/`access`
//! already refuse skill-discover paths with `EROFS` (see `write.rs`,
//! `link.rs`, `xattr.rs`, `meta.rs`). `mutate.rs` historically did not,
//! so a physical `source/skill-discover` directory — seeded by an
//! operator, left behind by a down-level layout, or reachable because
//! `resolve_physical_path` maps every skill-discover FUSE path onto
//! `source/skill-discover/...` — could be created, deleted, and moved
//! through the read-only virtual view.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::path::Path;

mod common;

use common::{MountFixture, create_skill_dir};

/// Seed a physical `source/skill-discover` backing tree. The FUSE layer
/// itself resolves every `/skills/skill-discover/...` path onto this
/// physical directory (`SkillFs::skill_physical_dir`), so unguarded
/// namespace mutations are physical mutations.
fn seed_discover_backing(src: &Path) {
    std::fs::create_dir_all(src.join("skill-discover/empty-sub")).expect("seed empty-sub");
    std::fs::create_dir_all(src.join("skill-discover/nested")).expect("seed nested");
    std::fs::write(src.join("skill-discover/notes.txt"), b"discover notes\n")
        .expect("seed notes.txt");
    std::fs::write(src.join("skill-discover/nested/inner.txt"), b"inner\n")
        .expect("seed nested/inner.txt");
}

#[test]
fn skill_discover_namespace_mutations_return_erofs() {
    if !common::fuse_available() {
        eprintln!("SKIP skill_discover_namespace_mutations_return_erofs: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        seed_discover_backing(src);
    });

    let discover = fx.skill_path("skill-discover");

    // mkdir inside the read-only namespace.
    let err = std::fs::create_dir(discover.join("subdir")).expect_err("mkdir must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "mkdir in skill-discover must be EROFS, got {err:?}"
    );

    // unlink inside the read-only namespace.
    let err = std::fs::remove_file(discover.join("notes.txt")).expect_err("unlink must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "unlink in skill-discover must be EROFS, got {err:?}"
    );

    // rmdir inside the read-only namespace.
    let err = std::fs::remove_dir(discover.join("empty-sub")).expect_err("rmdir must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rmdir in skill-discover must be EROFS, got {err:?}"
    );

    // rename inside the read-only namespace.
    let err = std::fs::rename(discover.join("notes.txt"), discover.join("moved.txt"))
        .expect_err("rename must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename inside skill-discover must be EROFS, got {err:?}"
    );

    // rename moving the namespace root out from under the virtual view.
    let err = std::fs::rename(discover.clone(), fx.skill_path("hijacked"))
        .expect_err("rename of skill-discover must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename of skill-discover must be EROFS, got {err:?}"
    );

    // rename whose *target* is the namespace root (the kernel usually
    // refuses dir-over-nonempty-dir first; the daemon must refuse too).
    let err = std::fs::rename(fx.skill_path("test-skill"), discover.clone())
        .expect_err("rename onto skill-discover must be refused");
    assert!(
        matches!(
            err.raw_os_error(),
            Some(libc::EROFS) | Some(libc::ENOTEMPTY)
        ),
        "rename onto skill-discover must be refused (EROFS or ENOTEMPTY), got {err:?}"
    );

    // mkdir of the namespace root itself (kernel refuses with EEXIST
    // through the always-present virtual dentry; the daemon must refuse
    // too when it is reached).
    let err =
        std::fs::create_dir(discover.clone()).expect_err("mkdir of skill-discover must be refused");
    assert!(
        matches!(err.raw_os_error(), Some(libc::EROFS) | Some(libc::EEXIST)),
        "mkdir of skill-discover must be refused (EROFS or EEXIST), got {err:?}"
    );

    // The physical source tree is untouched by every refused operation.
    assert!(
        fx.source().join("skill-discover/notes.txt").is_file(),
        "physical discover notes.txt must survive"
    );
    assert!(
        fx.source().join("skill-discover/empty-sub").is_dir(),
        "physical discover empty-sub must survive"
    );
    assert!(
        fx.source()
            .join("skill-discover/nested/inner.txt")
            .is_file(),
        "physical discover nested/inner.txt must survive"
    );
    assert!(
        !fx.source().join("skill-discover/subdir").exists(),
        "no directory may be created inside the read-only namespace"
    );
    assert!(
        !fx.source().join("skill-discover/moved.txt").exists(),
        "no rename target may appear inside the read-only namespace"
    );
    assert!(
        !fx.source().join("hijacked").exists(),
        "the discover backing dir must not be moved out of place"
    );
    assert!(
        fx.source().join("test-skill/SKILL.md").is_file(),
        "the control skill must be untouched"
    );
}

/// Cross-namespace renames touching skill-discover must answer `EROFS`,
/// not the cross-`/skills`/inbox `EXDEV` the L1 short-circuit would
/// return. With `EXDEV`, `mv` silently falls back to copy+unlink: the
/// copy leg mutates the read-only namespace (or leaves a partial
/// target inside it) and only the unlink leg then fails. The
/// skill-discover read-only gate therefore has to fire before the
/// cross-namespace judgment, for both directions.
#[test]
fn cross_namespace_skill_discover_renames_return_erofs_not_exdev() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP cross_namespace_skill_discover_renames_return_erofs_not_exdev: FUSE not available"
        );
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        seed_discover_backing(src);
        // A candidate skill being installed through the inbox: no
        // SKILL.md, so it has no /skills resolver entry and is only
        // reachable through `/.skillfs-inbox`.
        std::fs::create_dir_all(src.join("inbox-candidate")).expect("seed inbox candidate");
        std::fs::write(src.join("inbox-candidate/notes.txt"), b"candidate notes\n")
            .expect("seed candidate notes");
    });

    let discover = fx.skill_path("skill-discover");
    let inbox = fx.mountpoint().join(".skillfs-inbox");

    // Direction 1 (source side): /skills/skill-discover/x ->
    // /.skillfs-inbox/<target> must be EROFS.
    let err = std::fs::rename(discover.join("notes.txt"), inbox.join("discover-hijack"))
        .expect_err("rename out of skill-discover must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename from skill-discover into the inbox must be EROFS, got {err:?}"
    );

    // Same direction through `mv`: the EROFS must abort it before the
    // copy+unlink fallback leaves a partial target behind.
    let status = std::process::Command::new("mv")
        .arg(discover.join("notes.txt"))
        .arg(inbox.join("discover-hijack"))
        .status()
        .expect("spawn mv (discover -> inbox)");
    assert!(
        !status.success(),
        "mv from skill-discover into the inbox must fail"
    );

    // Direction 2 (target side): /.skillfs-inbox/<x> ->
    // /skills/skill-discover/<x> must be EROFS as well.
    let err = std::fs::rename(
        inbox.join("inbox-candidate"),
        discover.join("inbox-candidate"),
    )
    .expect_err("rename into skill-discover must be refused");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename from the inbox into skill-discover must be EROFS, got {err:?}"
    );

    let status = std::process::Command::new("mv")
        .arg(inbox.join("inbox-candidate"))
        .arg(discover.join("inbox-candidate"))
        .status()
        .expect("spawn mv (inbox -> discover)");
    assert!(
        !status.success(),
        "mv from the inbox into skill-discover must fail"
    );

    // No partial targets were left on either side, and both sources
    // survive untouched.
    assert!(
        !fx.source().join("discover-hijack").exists(),
        "mv fallback must not leave a partial target in the physical source"
    );
    assert!(
        !fx.source().join("skill-discover/inbox-candidate").exists(),
        "mv fallback must not leave a partial target inside the discover backing tree"
    );
    assert!(
        fx.source().join("skill-discover/notes.txt").is_file(),
        "physical discover notes.txt must survive both cross-namespace attempts"
    );
    assert!(
        fx.source().join("inbox-candidate/notes.txt").is_file(),
        "the inbox candidate must survive both cross-namespace attempts"
    );
}

#[test]
fn ordinary_skill_mutations_still_passthrough() {
    if !common::fuse_available() {
        eprintln!("SKIP ordinary_skill_mutations_still_passthrough: FUSE not available");
        return;
    }

    // Control: the same four callbacks keep passthrough semantics for an
    // ordinary skill directory, so the discover guard cannot overreach.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
        std::fs::create_dir_all(src.join("test-skill/workdir")).expect("seed workdir");
        std::fs::write(src.join("test-skill/notes.txt"), b"notes\n").expect("seed notes");
    });

    let skill = fx.skill_path("test-skill");

    std::fs::create_dir(skill.join("subdir")).expect("mkdir in ordinary skill");
    std::fs::rename(skill.join("notes.txt"), skill.join("moved.txt"))
        .expect("rename in ordinary skill");
    std::fs::remove_dir(skill.join("workdir")).expect("rmdir in ordinary skill");
    std::fs::remove_file(skill.join("moved.txt")).expect("unlink in ordinary skill");

    assert!(fx.source().join("test-skill/subdir").is_dir());
    assert!(!fx.source().join("test-skill/notes.txt").exists());
    assert!(!fx.source().join("test-skill/workdir").exists());
    assert!(!fx.source().join("test-skill/moved.txt").exists());
}
