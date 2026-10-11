//! Integration tests: `rename` rejects file-to-virtual-slot moves.
//!
//! `create_impl` (ba68e1db2) refuses to materialize an entry at a virtual
//! directory slot (`Root`, `SkillsDir`, `SkillDir`, `CategoryDir`) with
//! `EROFS`, and `mknod_impl` / `symlink_impl` reject the same slots. But
//! `rename_impl` had no virtual-slot arm: renaming a passthrough FILE onto
//! `/skills/<fresh>` (a `SkillDir` slot) or `/skills/<fresh-category>` (a
//! Hermes `CategoryDir`) fell through to `resolve_physical_path`, which
//! maps those slots onto `source/<name>`, and the physical rename
//! materialized a plain regular file there — the exact type confusion the
//! create gate prevents. The file is then invisible through the mount
//! (lookup answers ENOENT from the store miss) or wrong-typed when the
//! store later learns the name as a directory.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

mod common;

use common::{MountFixture, create_skill_dir};

/// Renaming a passthrough file onto a fresh `/skills/<name>` slot must be
/// refused with `EROFS` — the same answer `create` gives at that slot —
/// and nothing may be materialized on the source side. On the pre-fix
/// main the rename succeeded (ok) and physically materialized a plain
/// regular file at `source/<fresh-skill>` (verified on a real FUSE mount
/// as part of the self-integration audit).
#[test]
fn rename_file_onto_virtual_skill_dir_slot_materializes_file() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP rename_file_onto_virtual_skill_dir_slot_materializes_file: FUSE not available"
        );
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "demo-skill");
    });

    let src_file = fx.passthrough_path("demo-skill", "notes.txt");
    std::fs::write(&src_file, b"payload\n").expect("create passthrough file through mount");

    let slot = fx.skills_root().join("fresh-skill");
    let err = std::fs::rename(&src_file, &slot)
        .expect_err("rename of a file onto a virtual skill-dir slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename onto /skills/<fresh> must be EROFS like create, got {err:?}"
    );

    // No plain regular file (or anything else) may appear at the slot,
    // and the moved file must survive untouched at its origin.
    assert!(
        !fx.source().join("fresh-skill").exists(),
        "no entry may be materialized at the source skill-dir slot"
    );
    assert!(
        fx.source().join("demo-skill/notes.txt").is_file(),
        "the source file must survive the rejected rename"
    );

    // Control: plain `create` at the same slot keeps answering EROFS.
    let err = std::fs::write(&slot, b"payload\n")
        .expect_err("create at /skills/<fresh> must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual skill-dir slot must stay EROFS, got {err:?}"
    );
}

/// The Hermes twin: renaming a nested passthrough file (or a category
/// passthrough file) onto a fresh top-level category slot must be refused
/// with `EROFS` — the slot is a `CategoryDir`, and the pre-fix rename
/// materialized a plain regular file at `source/<fresh-category>`.
#[test]
fn rename_file_onto_hermes_category_slot_materializes_file() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP rename_file_onto_hermes_category_slot_materializes_file: FUSE not available"
        );
        return;
    }

    let fx = MountFixture::normal_hermes(|src| {
        create_skill_dir(&src.join("apple"), "apple-notes");
        std::fs::write(src.join("apple").join("README.md"), b"readme\n")
            .expect("seed category passthrough file");
    });

    // Nested passthrough file -> fresh category slot.
    let nested = fx
        .skills_root()
        .join("apple")
        .join("apple-notes")
        .join("notes.txt");
    std::fs::write(&nested, b"payload\n").expect("create nested passthrough file");
    let err = std::fs::rename(&nested, fx.skills_root().join("fresh-category"))
        .expect_err("rename of a file onto a category slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename onto a fresh category slot must be EROFS like create, got {err:?}"
    );
    assert!(
        !fx.source().join("fresh-category").exists(),
        "no entry may be materialized at the source category slot"
    );

    // Category passthrough file -> a different fresh category slot.
    let cat_file = fx.skills_root().join("apple").join("README.md");
    let err = std::fs::rename(&cat_file, fx.skills_root().join("other-fresh-category"))
        .expect_err("rename of a category file onto a category slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "rename of a category file onto a fresh category slot must be EROFS, got {err:?}"
    );
    assert!(
        !fx.source().join("other-fresh-category").exists(),
        "no entry may be materialized at the second category slot"
    );
    assert!(
        fx.source().join("apple/README.md").is_file(),
        "the category file must survive the rejected rename"
    );

    // Control: plain `create` at a category slot keeps answering EROFS.
    let err = std::fs::write(fx.skills_root().join("create-control-category"), b"x\n")
        .expect_err("create at a category slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual category slot must stay EROFS, got {err:?}"
    );
}

/// Control: the whole-skill directory rename (`SkillDir` -> `SkillDir`)
/// is the store-sync install flow and must keep working — the virtual-slot
/// arm only rejects FILE sides moving onto a slot.
#[test]
fn whole_skill_dir_rename_to_free_target_still_works() {
    if !common::fuse_available() {
        eprintln!("SKIP whole_skill_dir_rename_to_free_target_still_works: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "demo-skill");
    });

    std::fs::rename(fx.skill_path("demo-skill"), fx.skill_path("demo-moved"))
        .expect("whole-skill rename to a free target must keep working");

    assert!(
        !fx.source().join("demo-skill").exists(),
        "the old physical skill dir must be gone after the rename"
    );
    assert!(
        fx.source().join("demo-moved/SKILL.md").is_file(),
        "the manifest must live under the new physical name"
    );
}

/// Control: ordinary file renames inside a skill (passthrough ->
/// passthrough) keep working; only moves whose TARGET is a virtual
/// directory slot are refused.
#[test]
fn file_rename_inside_skill_still_works() {
    if !common::fuse_available() {
        eprintln!("SKIP file_rename_inside_skill_still_works: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "demo-skill");
    });

    let src_file = fx.passthrough_path("demo-skill", "notes.txt");
    std::fs::write(&src_file, b"payload\n").expect("create passthrough file");
    std::fs::rename(&src_file, fx.passthrough_path("demo-skill", "renamed.txt"))
        .expect("rename inside a skill must keep working");
    assert!(
        fx.source().join("demo-skill/renamed.txt").is_file(),
        "the renamed file must land on the source"
    );
}
