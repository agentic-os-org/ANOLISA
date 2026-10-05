//! Integration tests: `create` rejects virtual directory slots.
//!
//! `mknod_impl` and `symlink_impl` explicitly reject virtual paths with
//! `EROFS` (only passthrough leaves may host a new entry), but
//! `create_impl` had no such gate. A `touch /skills/<new>` parsed as a
//! `SkillDir` slot, fell through to `resolve_physical_path` (which maps
//! a skill dir onto `source/<name>`), and materialized a plain regular
//! file at `source/<new>`: `create` reported it as a `RegularFile`, but
//! the later `lookup`/`getattr` of the same path answered `ENOENT`
//! (store-miss) or `Directory` — a type-confused entry that shadows the
//! skill-directory slot with an invisible regular file.
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{InMemoryEventSink, SkillEventAction, SkillEventKind, SkillEventSink};
use skillfs_fuse::{
    MountConfig, MountHandle, MountOptions, SkillLayout, mount_background_configured,
};

use common::{MountFixture, create_skill_dir};

/// RAII fixture that mounts SkillFS with a recording event sink (and an
/// optional Hermes layout) so rejection audits can be asserted. Mirrors
/// the relevant subset of `common::MountFixture`.
struct AuditedMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    _handle: MountHandle,
}

impl AuditedMount {
    fn new(seed: impl FnOnce(&Path), sink: Arc<dyn SkillEventSink>) -> Self {
        Self::with_layout(seed, sink, None)
    }

    fn hermes(seed: impl FnOnce(&Path), sink: Arc<dyn SkillEventSink>) -> Self {
        Self::with_layout(seed, sink, Some(SkillLayout::Hermes))
    }

    fn with_layout(
        seed: impl FnOnce(&Path),
        sink: Arc<dyn SkillEventSink>,
        layout: Option<SkillLayout>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());

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
                event_sink: Some(sink),
                skill_layout: layout,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            _handle: handle,
        }
    }

    fn skills_root(&self) -> std::path::PathBuf {
        self.mountpoint.path().join("skills")
    }
}

#[test]
fn create_at_virtual_skill_dir_slot_is_erofs() {
    if !common::fuse_available() {
        eprintln!("SKIP create_at_virtual_skill_dir_slot_is_erofs: FUSE not available");
        return;
    }

    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
    });

    let new_skill = fx.skills_root().join("brand-new-skill");

    // `touch /skills/brand-new-skill` — the kernel lookup of the name
    // misses the store (ENOENT, negative dentry), so the FUSE create
    // fires with the path parsed as a SkillDir slot.
    let err = std::fs::write(&new_skill, b"payload\n")
        .expect_err("create at /skills/<new> must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual skill-dir slot must be EROFS, got {err:?}"
    );

    // No phantom regular file may be materialized on the source side,
    // and the slot must stay invisible (the pre-fix behavior answered
    // ENOENT from lookup after create succeeded, because the store
    // never learned the name).
    assert!(
        !fx.source().join("brand-new-skill").exists(),
        "no regular file may be materialized at the source skill-dir slot"
    );
    assert!(
        std::fs::metadata(&new_skill).is_err(),
        "the rejected slot must not become visible"
    );

    // The same refusal holds for the reserved virtual namespace name.
    // (The kernel usually answers first with EISDIR through the
    // always-present virtual directory dentry; the daemon-side guard
    // answers EROFS when it is the one reached.)
    let err = std::fs::write(fx.skills_root().join("skill-discover"), b"payload\n")
        .expect_err("create at /skills/skill-discover must be rejected");
    assert!(
        matches!(err.raw_os_error(), Some(libc::EROFS) | Some(libc::EISDIR)),
        "create at the skill-discover slot must be refused (EROFS or EISDIR), got {err:?}"
    );
    assert!(
        !fx.source().join("skill-discover").exists(),
        "no regular file may be materialized at the skill-discover slot"
    );
}

/// Creating an entry at a reserved lifecycle root (`/skills/.staging`,
/// `/skills/.certified`, …) must keep the historical lifecycle answer —
/// `EACCES` plus a `PolicyDenied`/`Rejected` audit record (and the
/// policy_denied metric) — instead of the virtual-slot catch-all's
/// `EROFS`/`Create` record that used to fire first.
#[test]
fn create_at_lifecycle_reserved_roots_keeps_eacces_and_policy_denied_audit() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP create_at_lifecycle_reserved_roots_keeps_eacces_and_policy_denied_audit: FUSE not available"
        );
        return;
    }

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::new(
        |src| {
            create_skill_dir(src, "test-skill");
        },
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    for reserved in [".staging", ".certified"] {
        let err = std::fs::write(mount.skills_root().join(reserved), b"payload\n")
            .expect_err("create at a reserved lifecycle root must be refused");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EACCES),
            "create at /skills/{reserved} must keep the lifecycle EACCES, got {err:?}"
        );
        assert!(
            !mount.source.path().join(reserved).exists(),
            "no physical entry may appear at /skills/{reserved}"
        );

        let events = sink.events();
        let denied: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == SkillEventKind::PolicyDenied
                    && e.action == Some(SkillEventAction::Rejected)
            })
            .filter(|e| e.skill_name.as_deref() == Some(reserved))
            .collect();
        assert_eq!(
            denied.len(),
            1,
            "exactly one PolicyDenied/Rejected audit must record /skills/{reserved}, got {denied:?}"
        );
        let ev = denied[0];
        assert_eq!(ev.errno, Some(libc::EACCES));
        let detail = ev.detail.as_deref().unwrap_or_default();
        assert!(
            detail.contains(&format!("lifecycle={reserved}")),
            "the PolicyDenied detail must name the reserved namespace, got {detail:?}"
        );

        // The virtual-slot catch-all must not have fired for the
        // reserved root: no Create/Rejected/EROFS record may exist.
        let slot_records: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == SkillEventKind::Create
                    && e.action == Some(SkillEventAction::Rejected)
                    && e.errno == Some(libc::EROFS)
                    && e.skill_name.as_deref() == Some(reserved)
            })
            .collect();
        assert!(
            slot_records.is_empty(),
            "the virtual-slot EROFS record must not shadow the lifecycle denial for /skills/{reserved}, got {slot_records:?}"
        );
    }
}

/// The virtual-slot rejection must be attributable and class-tagged: a
/// Hermes category-slot creation logs a `Create`/`Rejected`/`EROFS`
/// event carrying the category name, a stable `class=virtual_dir_slot`
/// label, and the path — not an anonymous record.
#[test]
fn create_at_virtual_hermes_category_slot_emits_attributed_classed_audit() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP create_at_virtual_hermes_category_slot_emits_attributed_classed_audit: FUSE not available"
        );
        return;
    }

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::hermes(
        |src| {
            std::fs::create_dir_all(src.join("apple").join("apple-notes")).expect("seed category");
        },
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    let err = std::fs::write(mount.skills_root().join("fresh-category"), b"payload\n")
        .expect_err("create at a category slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual category slot must be EROFS, got {err:?}"
    );

    let events = sink.events();
    let record = events
        .iter()
        .find(|e| {
            e.kind == SkillEventKind::Create
                && e.action == Some(SkillEventAction::Rejected)
                && e.errno == Some(libc::EROFS)
        })
        .expect("a Create/Rejected/EROFS audit must be recorded for the category slot");
    assert_eq!(
        record.skill_name.as_deref(),
        Some("fresh-category"),
        "the category slot must be attributed to the category name, got {record:?}"
    );
    let detail = record.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("class=virtual_dir_slot"),
        "the rejection detail must carry the stable class label, got {detail:?}"
    );
    assert!(
        detail.contains("path="),
        "the rejection detail must carry the path, got {detail:?}"
    );
}

/// The flat skill-dir slot rejection carries the same stable class and
/// path in its audit detail.
#[test]
fn create_at_virtual_skill_dir_slot_emits_classed_audit() {
    if !common::fuse_available() {
        eprintln!("SKIP create_at_virtual_skill_dir_slot_emits_classed_audit: FUSE not available");
        return;
    }

    let sink = Arc::new(InMemoryEventSink::new());
    let mount = AuditedMount::new(
        |src| {
            create_skill_dir(src, "test-skill");
        },
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    let err = std::fs::write(mount.skills_root().join("brand-new-skill"), b"payload\n")
        .expect_err("create at /skills/<new> must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual skill-dir slot must be EROFS, got {err:?}"
    );

    let record = sink
        .events()
        .into_iter()
        .find(|e| {
            e.kind == SkillEventKind::Create
                && e.action == Some(SkillEventAction::Rejected)
                && e.errno == Some(libc::EROFS)
        })
        .expect("a Create/Rejected/EROFS audit must be recorded for the skill-dir slot");
    assert_eq!(record.skill_name.as_deref(), Some("brand-new-skill"));
    let detail = record.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("class=virtual_dir_slot") && detail.contains("path="),
        "the rejection detail must carry the class label and path, got {detail:?}"
    );
}

#[test]
fn create_inside_skill_and_manifest_flows_still_work() {
    if !common::fuse_available() {
        eprintln!("SKIP create_inside_skill_and_manifest_flows_still_work: FUSE not available");
        return;
    }

    // Control: the file-capable leaves keep working — passthrough
    // files inside an existing skill, and the standard install flow of
    // mkdir + create SKILL.md.
    let fx = MountFixture::normal(|src| {
        create_skill_dir(src, "test-skill");
    });

    std::fs::write(fx.skill_path("test-skill").join("notes.txt"), b"notes\n")
        .expect("create passthrough file inside skill");
    assert!(fx.source().join("test-skill/notes.txt").is_file());

    std::fs::create_dir(fx.skills_root().join("fresh-skill")).expect("mkdir new skill dir");
    std::fs::write(
        fx.skill_path("fresh-skill").join("SKILL.md"),
        "---\nname: fresh-skill\ndescription: new\n---\n",
    )
    .expect("create SKILL.md manifest");
    assert!(fx.source().join("fresh-skill/SKILL.md").is_file());
}

#[test]
fn create_at_virtual_hermes_category_slot_is_erofs() {
    if !common::fuse_available() {
        eprintln!("SKIP create_at_virtual_hermes_category_slot_is_erofs: FUSE not available");
        return;
    }

    let fx = MountFixture::normal_hermes(|src| {
        std::fs::create_dir_all(src.join("apple").join("apple-notes")).expect("seed category");
        std::fs::write(
            src.join("apple").join("apple-notes").join("SKILL.md"),
            "---\nname: apple-notes\ndescription: nested\n---\n",
        )
        .expect("seed nested SKILL.md");
    });

    // `touch /skills/<new-category>` parses as a CategoryDir slot in a
    // normal-mode Hermes mount; unguarded create materialized a regular
    // file at `source/<new-category>`.
    let err = std::fs::write(fx.skills_root().join("fresh-category"), b"payload\n")
        .expect_err("create at a category slot must be rejected");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EROFS),
        "create at a virtual category slot must be EROFS, got {err:?}"
    );
    assert!(
        !fx.source().join("fresh-category").exists(),
        "no regular file may be materialized at the category slot"
    );

    // Control: an ordinary passthrough file directly under an existing
    // category (`apple/README.md`) is a file-capable leaf and keeps
    // working.
    std::fs::write(
        fx.skills_root().join("apple").join("README.md"),
        b"readme\n",
    )
    .expect("create category passthrough file");
    assert!(fx.source().join("apple/README.md").is_file());
}
