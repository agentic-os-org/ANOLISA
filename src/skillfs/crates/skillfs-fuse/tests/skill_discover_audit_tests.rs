//! Audit coverage for mutations refused against the read-only
//! `skill-discover` virtual namespace.
//!
//! #5085 (7e989e51a) routed mkdir/unlink/rmdir/rename through
//! `enforce_skill_discover_readonly` and `create` through its own
//! discover arm, both returning `EROFS`. #5386 (35753239c) then
//! established — and wired — the convention that every mutating
//! callback rejection against a gated skill leaves a `Rejected` audit
//! record, but only for the hidden-skill arms. The skill-discover arms
//! in the SAME callbacks returned silently, while the pre-existing
//! discover gates were never silent:
//!   - symlink  -> SymlinkAttempt/Rejected/EROFS, detail class=skill_discover
//!   - link     -> HardlinkAttempt/Rejected/EROFS, detail class=skill_discover
//!   - mknod    -> Create/Rejected/EROFS op-event
//!
//! So an audit consumer saw symlink/link/mknod probes against the
//! read-only namespace but mkdir/unlink/rmdir/rename/create probes
//! against the very same namespace left no trace.
//!
//! This file probes every mutating callback against skill-discover on a
//! real mount with a recording sink and asserts each rejection leaves
//! exactly one `Rejected`/`EROFS` record tagged `class=skill_discover`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{InMemoryEventSink, SkillEventAction, SkillEventKind, SkillEventSink};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use common::create_skill_dir;

/// (label, required detail substring, probe closure).
type Probe<'a> = (
    &'static str,
    &'static str,
    Box<dyn FnOnce() -> std::io::Result<()> + 'a>,
);

struct AuditedMount {
    _source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    sink: Arc<InMemoryEventSink>,
    _handle: MountHandle,
}

impl AuditedMount {
    fn new(seed: impl FnOnce(&Path)) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        let sink = Arc::new(InMemoryEventSink::new());
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                event_sink: Some(sink.clone() as Arc<dyn SkillEventSink>),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            _source: source,
            mountpoint,
            sink,
            _handle: handle,
        }
    }

    fn skill(&self, name: &str) -> PathBuf {
        self.mountpoint.path().join("skills").join(name)
    }
}

fn seed_discover_backing(src: &Path) {
    create_skill_dir(src, "test-skill");
    std::fs::create_dir_all(src.join("skill-discover/empty-sub")).expect("seed empty-sub");
    std::fs::write(src.join("skill-discover/notes.txt"), b"discover notes\n")
        .expect("seed notes.txt");
}

/// Every mutating probe against the skill-discover namespace must be
/// refused AND audited: exactly one Rejected record per probe, carrying
/// EROFS and the stable `class=skill_discover` label. The errno half
/// comes from #5085; the audit half is the convention #5386 wired for
/// hidden skills and that symlink/link/mknod already honor for
/// skill-discover.
#[test]
fn every_discover_rejection_is_audited() {
    if !common::fuse_available() {
        eprintln!("SKIP every_discover_rejection_is_audited: FUSE not available");
        return;
    }

    let fx = AuditedMount::new(seed_discover_backing);
    let discover = fx.skill("skill-discover");

    // (label, required detail substring, probe). Each probe must fail;
    // the assertion checks the errno where the kernel forwards it and
    // the audit record. `hardlink-out` links OUT to a virtual /skills
    // slot, so the pre-existing virtual-destination gate answers first
    // with `class=virtual_dst` — that control pins the record count and
    // errno, not the discover label.
    let probes: Vec<Probe<'_>> = vec![
        (
            "mkdir",
            "class=skill_discover",
            Box::new(|| std::fs::create_dir(discover.join("made-dir"))),
        ),
        (
            "unlink",
            "class=skill_discover",
            Box::new(|| std::fs::remove_file(discover.join("notes.txt"))),
        ),
        (
            "rmdir",
            "class=skill_discover",
            Box::new(|| std::fs::remove_dir(discover.join("empty-sub"))),
        ),
        (
            "rename-inside",
            "class=skill_discover",
            Box::new(|| std::fs::rename(discover.join("notes.txt"), discover.join("moved.txt"))),
        ),
        (
            "rename-root-away",
            "class=skill_discover",
            Box::new(|| std::fs::rename(discover.clone(), fx.skill("hijacked"))),
        ),
        (
            "create",
            "class=skill_discover",
            Box::new(|| std::fs::write(discover.join("new-file.txt"), b"x")),
        ),
        (
            "symlink",
            "class=skill_discover",
            Box::new(|| std::os::unix::fs::symlink("/etc/hostname", discover.join("link.txt"))),
        ),
        (
            "hardlink-out",
            "class=",
            Box::new(|| std::fs::hard_link(discover.join("notes.txt"), fx.skill("stolen.txt"))),
        ),
    ];

    let mut report = String::new();
    let mut missing = Vec::new();
    for (label, want_detail, probe) in probes {
        let start = fx.sink.events().len();
        let result = probe();
        let snapshot = fx.sink.events();
        let events: Vec<_> = snapshot[start..]
            .iter()
            .filter(|e| e.action == Some(SkillEventAction::Rejected))
            .collect();
        let errno_note = match &result {
            Ok(()) => "NOT-REFUSED".to_string(),
            Err(e) => format!("errno={:?}", e.raw_os_error()),
        };
        report.push_str(&format!(
            "  probe {label:<18} -> {errno_note:<16} rejected_events={}\n",
            events.len()
        ));
        if events.len() != 1 {
            missing.push(format!("{label} ({} Rejected records)", events.len()));
        } else if events[0].errno != Some(libc::EROFS) {
            missing.push(format!(
                "{label} (Rejected record errno={:?}, want EROFS)",
                events[0].errno
            ));
        } else if !events[0]
            .detail
            .as_deref()
            .unwrap_or("")
            .contains(want_detail)
        {
            missing.push(format!(
                "{label} (Rejected detail={:?}, want substring {want_detail:?})",
                events[0].detail
            ));
        }
    }

    eprintln!("discover audit probe report:\n{report}");
    assert!(
        missing.is_empty(),
        "every skill-discover mutation rejection must leave exactly one \
         Rejected/EROFS audit record tagged class=skill_discover (hidden \
         arms got this in #5386; symlink/link/mknod already emit for \
         skill-discover). Probes that do not:\n  {}\nfull report:\n{report}",
        missing.join("\n  ")
    );
}

/// The rejected records that DO fire must carry the stable
/// `class=skill_discover` label the symlink/link gates established.
#[test]
fn discover_rejections_carry_class_label() {
    if !common::fuse_available() {
        eprintln!("SKIP discover_rejections_carry_class_label: FUSE not available");
        return;
    }

    let fx = AuditedMount::new(seed_discover_backing);
    let discover = fx.skill("skill-discover");

    // symlink is known to emit; use it to pin the label convention.
    let err = std::os::unix::fs::symlink("/etc/hostname", discover.join("link.txt"))
        .expect_err("symlink into skill-discover must be refused");
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));

    let snapshot = fx.sink.events();
    let records: Vec<_> = snapshot
        .iter()
        .filter(|e| e.action == Some(SkillEventAction::Rejected))
        .collect();
    assert!(!records.is_empty(), "symlink rejection must be audited");
    for record in &records {
        let detail = record.detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("class=skill_discover"),
            "discover rejection must carry the stable class label: {record:?}"
        );
    }
    let _ = SkillEventKind::Create; // keep the import honest for readers
}
