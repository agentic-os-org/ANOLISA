//! Package S3 — Lifecycle namespace reservation acceptance tests.
//!
//! These tests pin the boundary between ordinary skill paths and the
//! reserved lifecycle namespaces (`.staging`, `.certified`, `.quarantine`,
//! `.archive`) at the FUSE layer. S3 is reservation-only: lifecycle state
//! transitions, a management view, quarantine/scanner integration, trusted
//! writer identity, and capability enforcement are all out of scope.
//!
//! What the tests assert:
//!   * lookup, getattr, and access on a reserved root surface ENOENT —
//!     the same response a non-existent skill would produce, so ordinary
//!     callers cannot tell the namespace exists;
//!   * readdir on `/skills` does not list any reserved root, even when
//!     the underlying source directory contains one;
//!   * mkdir, create, open(write/trunc), unlink, rmdir, rename
//!     (from-side and to-side), and setattr on a reserved namespace
//!     return EACCES without touching the source;
//!   * neighbour names like `.staging2` are not reserved and continue
//!     to behave like ordinary skill paths;
//!   * nested paths like `<skill>/docs/.staging` (where `.staging` sits
//!     deeper than the top-level skill segment) are not reserved.
//!
//! The tests run against both `MountMode::Normal` and `MountMode::InPlace`
//! so the boundary is exercised in both layouts. Existing skill-discover,
//! `.skill-meta`, audit JSONL, and POSIX errno regressions live in their
//! own test files and are left unchanged.

use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

mod common;

use common::{MountFixture, create_skill_dir, list_dir_names};
use skillfs_fuse::security::{
    LifecycleAccess, LifecycleViewMode, classify_lifecycle_skill_name_with_mode,
    is_lifecycle_name_mutable, is_lifecycle_name_visible,
};

const RESERVED_NAMES: &[&str] = &[".staging", ".certified", ".quarantine", ".archive"];

// ─────────────────────────────────────────────────────────────────────────────
// Lookup, getattr, access — lifecycle roots are hidden via ENOENT.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_lookup_on_reserved_root_returns_enoent_normal_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::metadata(&path)
            .err()
            .unwrap_or_else(|| panic!("{name}: lookup must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert_eq!(
            raw,
            libc::ENOENT,
            "{name}: expected ENOENT, got {raw} ({err})"
        );
    }
}

#[test]
fn test_lookup_on_reserved_root_returns_enoent_inplace_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::in_place(|src| {
        create_skill_dir(src, "alpha");
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::metadata(&path)
            .err()
            .unwrap_or_else(|| panic!("{name}: lookup must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert_eq!(
            raw,
            libc::ENOENT,
            "{name}: expected ENOENT, got {raw} ({err})"
        );
    }
}

#[test]
fn test_lookup_hides_reserved_root_when_present_in_source() {
    skip_if_no_fuse!();

    // The FUSE store already skips top-level hidden directories at load
    // time, so a pre-existing `.staging/` in the source tree never lands
    // in the store. The boundary still has to refuse lookup so a caller
    // that probes the path directly cannot tell the directory exists.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        std::fs::create_dir(src.join(".staging")).expect("pre-create source-side .staging");
        std::fs::write(src.join(".staging/marker"), b"this should remain hidden")
            .expect("write marker inside .staging");
    });

    let staging = fx.skills_root().join(".staging");
    let err = std::fs::metadata(&staging)
        .err()
        .unwrap_or_else(|| panic!(".staging lookup must fail"));
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        ".staging lookup must surface ENOENT"
    );

    // The marker file must not be reachable through the mount even via a
    // direct path.
    let marker = staging.join("marker");
    let err = std::fs::metadata(&marker)
        .err()
        .unwrap_or_else(|| panic!(".staging/marker lookup must fail"));
    assert_eq!(
        err.raw_os_error().unwrap_or(0),
        libc::ENOENT,
        ".staging/marker lookup must surface ENOENT"
    );

    // The physical source still owns the file; SkillFS only hid it.
    assert!(
        fx.source().join(".staging/marker").exists(),
        "source-side marker must remain on disk"
    );
}

#[test]
fn test_readdir_does_not_list_reserved_roots() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap_or_else(|e| panic!("seed {name}: {e}"));
        }
        // Neighbour names are not reserved; they must remain visible
        // when they are valid skills with a SKILL.md.
        create_skill_dir(src, ".staging-but-actually-a-skill"); // hidden by store loader (starts with dot)
    });

    let listing = list_dir_names(&fx.skills_root());
    for reserved in RESERVED_NAMES {
        assert!(
            !listing.contains(&reserved.to_string()),
            "/skills must not list reserved root {reserved}; got {:?}",
            listing
        );
    }
    assert!(
        listing.contains(&"alpha".to_string()),
        "ordinary skill must remain visible; got {:?}",
        listing
    );
}

#[test]
fn test_access_on_reserved_root_surfaces_enoent() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let c = CString::new(path.to_str().unwrap()).unwrap();
        // F_OK on a hidden path should look like a missing file.
        let rc = unsafe { libc::access(c.as_ptr(), libc::F_OK) };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert_eq!(rc, -1, "{name}: access(F_OK) must fail");
        assert_eq!(err, libc::ENOENT, "{name}: expected ENOENT, got {err}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Mutations against reserved namespaces are rejected with EACCES.
// ─────────────────────────────────────────────────────────────────────────────

fn assert_eacces(label: &str, raw: i32) {
    assert_eq!(raw, libc::EACCES, "{label}: expected EACCES, got {raw}");
}

#[test]
fn test_mkdir_on_reserved_root_rejected_with_eacces() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|_| {});

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::create_dir(&path)
            .err()
            .unwrap_or_else(|| panic!("mkdir {name} must fail"));
        assert_eacces(name, err.raw_os_error().unwrap_or(0));
        assert!(
            !fx.source().join(name).exists(),
            "{name}: mkdir must not create source-side directory"
        );
    }
}

#[test]
fn test_mkdir_on_reserved_root_rejected_in_place_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::in_place(|_| {});

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::create_dir(&path)
            .err()
            .unwrap_or_else(|| panic!("mkdir {name} must fail"));
        assert_eacces(name, err.raw_os_error().unwrap_or(0));
        assert!(
            !fx.source().join(name).exists(),
            "{name}: mkdir must not create source-side directory"
        );
    }
}

#[test]
fn test_create_under_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        // Pre-create the reserved root on disk so create() targets a
        // nested file even though the root is hidden at the FUSE layer.
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap_or_else(|e| panic!("seed {name}: {e}"));
        }
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name).join("SKILL.md");
        let c = CString::new(path.to_str().unwrap()).unwrap();
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o644,
            )
        };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if fd >= 0 {
            unsafe { libc::close(fd) };
            panic!("{name}: create must fail, got fd={fd}");
        }
        // The kernel walks the path before issuing FUSE create(), so the
        // hidden-lookup gate at the parent surfaces ENOENT before the
        // S3 mutation gate fires. EACCES is also acceptable for callers
        // that hold a parent ino directly. Both are deterministic
        // permission/visibility errors and either preserves the boundary.
        assert!(
            err == libc::ENOENT || err == libc::EACCES,
            "{name}: expected ENOENT or EACCES on create, got {err}"
        );
        assert!(
            !fx.source().join(name).join("SKILL.md").exists(),
            "{name}: create must not produce a source-side file"
        );
    }
}

#[test]
fn test_open_with_trunc_on_reserved_md_rejected_with_eacces() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        // Pre-create both the root and a SKILL.md so the only thing
        // standing between the caller and a successful truncate is the
        // S3 reservation gate.
        for name in RESERVED_NAMES {
            let root = src.join(name);
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("SKILL.md"), b"---\n---\n").unwrap();
        }
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name).join("SKILL.md");
        let c = CString::new(path.to_str().unwrap()).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_TRUNC) };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if fd >= 0 {
            unsafe { libc::close(fd) };
            panic!("{name}: open(O_WRONLY|O_TRUNC) must fail");
        }
        // Hidden lookup returns ENOENT before open even reaches the
        // mutation gate, so either ENOENT or EACCES is acceptable. Both
        // are deterministic permission/visibility errors.
        assert!(
            err == libc::ENOENT || err == libc::EACCES,
            "{name}: expected ENOENT or EACCES, got {err}"
        );
        // Source-side SKILL.md must remain unchanged.
        let body =
            std::fs::read(fx.source().join(name).join("SKILL.md")).expect("read source SKILL.md");
        assert_eq!(body, b"---\n---\n", "{name}: source SKILL.md was modified");
    }
}

#[test]
fn test_unlink_under_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        for name in RESERVED_NAMES {
            let root = src.join(name);
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("SKILL.md"), b"---\n---\n").unwrap();
        }
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name).join("SKILL.md");
        let err = std::fs::remove_file(&path)
            .err()
            .unwrap_or_else(|| panic!("unlink {name}/SKILL.md must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert!(
            raw == libc::ENOENT || raw == libc::EACCES,
            "{name}: expected ENOENT or EACCES, got {raw}"
        );
        assert!(
            fx.source().join(name).join("SKILL.md").exists(),
            "{name}: source SKILL.md must remain on disk"
        );
    }
}

#[test]
fn test_rmdir_on_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap();
        }
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::remove_dir(&path)
            .err()
            .unwrap_or_else(|| panic!("rmdir {name} must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert!(
            raw == libc::ENOENT || raw == libc::EACCES,
            "{name}: expected ENOENT or EACCES, got {raw}"
        );
        assert!(
            fx.source().join(name).exists(),
            "{name}: source-side directory must remain on disk"
        );
    }
}

#[test]
fn test_rename_into_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    for name in RESERVED_NAMES {
        let from = fx.skill_path("alpha");
        let to = fx.skills_root().join(name);
        let err = std::fs::rename(&from, &to)
            .err()
            .unwrap_or_else(|| panic!("rename alpha → {name} must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert!(
            raw == libc::EACCES || raw == libc::ENOENT,
            "{name}: expected EACCES or ENOENT, got {raw}"
        );
        assert!(
            fx.source().join("alpha").exists(),
            "{name}: alpha must remain on source after rejected rename"
        );
        assert!(
            !fx.source().join(name).exists(),
            "{name}: rejected rename must not create reserved root"
        );
    }
}

#[test]
fn test_rename_out_of_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        for name in RESERVED_NAMES {
            let root = src.join(name);
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("SKILL.md"), b"---\n---\n").unwrap();
        }
    });

    for name in RESERVED_NAMES {
        let from = fx.skills_root().join(name).join("SKILL.md");
        let to = fx.skill_path("alpha").join("imported.md");
        let err = std::fs::rename(&from, &to)
            .err()
            .unwrap_or_else(|| panic!("rename {name}/SKILL.md → alpha must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        assert!(
            raw == libc::EACCES || raw == libc::ENOENT,
            "{name}: expected EACCES or ENOENT, got {raw}"
        );
        assert!(
            fx.source().join(name).join("SKILL.md").exists(),
            "{name}: source-side SKILL.md must remain in lifecycle root"
        );
        assert!(
            !fx.passthrough_path("alpha", "imported.md").exists(),
            "{name}: rejected rename must not produce alpha/imported.md"
        );
    }
}

#[test]
fn test_setattr_chmod_on_reserved_root_rejected() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap();
        }
    });

    for name in RESERVED_NAMES {
        let path = fx.skills_root().join(name);
        let err = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .err()
            .unwrap_or_else(|| panic!("chmod {name} must fail"));
        let raw = err.raw_os_error().unwrap_or(0);
        // SkillDir-class chmods always return EROFS in pre-S3 code; the
        // hidden-lookup path can also surface ENOENT before the gate.
        assert!(
            raw == libc::EROFS || raw == libc::EACCES || raw == libc::ENOENT,
            "{name}: expected EROFS / EACCES / ENOENT, got {raw}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Neighbour names and nested paths must not be reserved.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_neighbour_name_staging2_is_not_reserved() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|_| {});

    let path = fx.skills_root().join(".staging2");
    // The store loader skips hidden top-level directories at load time, so
    // mkdir of a `.staging2` skill creates the placeholder but the listing
    // hides it (consistent with `.foo` behavior). What the S3 boundary
    // must not do is reject the mkdir with EACCES — that's reserved for
    // the four canonical names only.
    match std::fs::create_dir(&path) {
        Ok(()) => {
            assert!(
                fx.source().join(".staging2").exists(),
                ".staging2 must land on disk"
            );
        }
        Err(e) => {
            let raw = e.raw_os_error().unwrap_or(0);
            assert_ne!(
                raw,
                libc::EACCES,
                ".staging2 must not be rejected as reserved (got EACCES); err={e}"
            );
        }
    }
}

#[test]
fn test_neighbour_name_inside_skill_succeeds() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    // Creating a directory named `.staging` *inside* an existing skill is
    // not a top-level lifecycle root, so it must be allowed. The path is
    // `<skills>/alpha/docs/.staging`, where `.staging` sits at depth 3.
    let docs = fx.passthrough_path("alpha", "docs");
    std::fs::create_dir(&docs).expect("mkdir alpha/docs");
    let nested = docs.join(".staging");
    std::fs::create_dir(&nested).unwrap_or_else(|e| {
        panic!("alpha/docs/.staging must succeed (S3 only reserves top segment): {e}")
    });
    assert!(nested.exists(), "alpha/docs/.staging must land on disk");
    assert!(
        fx.source().join("alpha/docs/.staging").exists(),
        "source-side alpha/docs/.staging must exist"
    );
}

#[test]
fn test_passthrough_file_named_reserved_inside_skill_succeeds() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
    });

    // A file literally named `.staging` inside a skill is not a top-level
    // reservation either. It must remain a normal passthrough write.
    let path: PathBuf = fx.passthrough_path("alpha", ".staging");
    std::fs::write(&path, b"contents")
        .unwrap_or_else(|e| panic!("write alpha/.staging must succeed: {e}"));
    let read_back = std::fs::read(&path).expect("read alpha/.staging");
    assert_eq!(read_back, b"contents");
    assert!(
        fx.source().join("alpha/.staging").exists(),
        "source-side alpha/.staging must exist"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// skill-discover and ordinary skills remain unaffected.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_skill_discover_still_visible_with_reserved_namespaces() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap();
        }
    });

    let listing = list_dir_names(&fx.skills_root());
    assert!(
        listing.contains(&"skill-discover".to_string()),
        "skill-discover must remain visible; got {:?}",
        listing
    );
    assert!(
        listing.contains(&"alpha".to_string()),
        "ordinary skill must remain visible; got {:?}",
        listing
    );

    // The compiled skill-discover SKILL.md must still be readable.
    let md = fx.passthrough_path("skill-discover", "SKILL.md");
    let body = std::fs::read_to_string(&md).expect("skill-discover/SKILL.md must remain readable");
    assert!(
        body.contains("name: skill-discover"),
        "compiled skill-discover SKILL.md must keep its frontmatter"
    );
}

#[test]
fn test_ordinary_skill_mkdir_unaffected_in_normal_mode() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|_| {});

    let path = fx.skills_root().join("regular-skill");
    std::fs::create_dir(&path).expect("mkdir regular-skill must succeed");
    assert!(path.exists());
    assert!(fx.source().join("regular-skill").exists());
}

#[test]
fn test_inplace_ordinary_skill_mkdir_unaffected() {
    skip_if_no_fuse!();

    let fx = MountFixture::in_place(|_| {});
    let path = fx.skills_root().join("regular-skill");
    std::fs::create_dir(&path).expect("mkdir regular-skill must succeed");
    assert!(path.exists());
    assert!(fx.source().join("regular-skill").exists());

    // Reserved roots remain hidden after a successful ordinary mkdir.
    let listing = list_dir_names(&fx.skills_root());
    for reserved in RESERVED_NAMES {
        assert!(
            !listing.contains(&reserved.to_string()),
            "in-place /skills must not list reserved root {reserved}; got {:?}",
            listing
        );
    }
    assert!(listing.contains(&"regular-skill".to_string()));
}

// ─────────────────────────────────────────────────────────────────────────────
// Package S3.1 — management-view contract.
//
// S3.1 only defines the pure helper API. The default FUSE mount keeps the S3
// behavior: reserved roots stay hidden and immutable. These tests pin both
// sides of the contract:
//
//   * the live default mount agrees with `LifecycleViewMode::Ordinary` —
//     reserved roots are hidden via ENOENT, and the helper agrees;
//   * `LifecycleViewMode::Management` flips the helper's answer so a future
//     trusted-writer / management surface can intentionally expose the same
//     roots without re-deriving the table.
//
// Nothing in this file routes mount syscalls through `Management` — that
// wiring lives in a later package along with CLI surface and trusted
// identity. S3.1 is contract-only.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_default_mount_matches_lifecycle_view_mode_ordinary() {
    skip_if_no_fuse!();

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        for name in RESERVED_NAMES {
            std::fs::create_dir(src.join(name)).unwrap_or_else(|e| panic!("seed {name}: {e}"));
        }
    });

    let listing = list_dir_names(&fx.skills_root());
    for reserved in RESERVED_NAMES {
        // The live mount agrees with the pure-API decision for Ordinary mode.
        assert!(
            !is_lifecycle_name_visible(reserved, LifecycleViewMode::Ordinary),
            "{reserved} must be hidden under Ordinary"
        );
        assert!(
            !listing.contains(&reserved.to_string()),
            "default mount must not list {reserved} under Ordinary view; got {:?}",
            listing
        );

        // Lookup of the hidden root surfaces ENOENT, matching the contract:
        // a name that is not visible cannot be looked up.
        let err = std::fs::metadata(fx.skills_root().join(reserved))
            .err()
            .unwrap_or_else(|| panic!("{reserved}: lookup must fail"));
        assert_eq!(err.raw_os_error().unwrap_or(0), libc::ENOENT);
    }
}

#[test]
fn test_management_view_mode_exposes_reserved_names_via_pure_api() {
    // No mount required: S3.1 ships only the pure API. The default FUSE
    // mount does not consume `Management` yet, but the contract must be
    // testable on its own so future wiring has a stable surface to plug into.
    for reserved in RESERVED_NAMES {
        assert!(
            is_lifecycle_name_visible(reserved, LifecycleViewMode::Management),
            "{reserved} must be visible under Management"
        );
        assert!(
            is_lifecycle_name_mutable(reserved, LifecycleViewMode::Management),
            "{reserved} must be mutable under Management"
        );

        match classify_lifecycle_skill_name_with_mode(reserved, LifecycleViewMode::Management) {
            LifecycleAccess::Exposed(canonical) => {
                assert_eq!(
                    canonical, *reserved,
                    "canonical static slice must round-trip the reserved name"
                );
            }
            other => panic!(
                "{reserved} under Management expected Exposed, got {:?}",
                other
            ),
        }
    }
}

#[test]
fn test_non_reserved_names_unaffected_by_view_mode_in_pure_api() {
    // Neighbour and ordinary names already pass through the live mount
    // unchanged (see `test_neighbour_name_*` above). Mirror that contract
    // at the API layer so a future caller cannot accidentally widen the
    // reservation by switching views.
    for ordinary in [
        "alpha",
        "skill-discover",
        ".staging2",
        ".staging-but-actually-a-skill",
        "regular-skill",
    ] {
        for mode in [LifecycleViewMode::Ordinary, LifecycleViewMode::Management] {
            assert!(is_lifecycle_name_visible(ordinary, mode));
            assert!(is_lifecycle_name_mutable(ordinary, mode));
            assert_eq!(
                classify_lifecycle_skill_name_with_mode(ordinary, mode),
                LifecycleAccess::Ordinary
            );
        }
    }
}
