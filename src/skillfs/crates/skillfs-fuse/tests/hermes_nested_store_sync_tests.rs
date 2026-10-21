//! Integration tests: Hermes nested manifest mutations sync the store.
//!
//! Every write-path mutation of a flat manifest — `write`, `create`,
//! `setattr`-truncate, open `O_TRUNC`, and a file-level rename — enqueues a
//! [`SyncEvent::Reparse`] so the background worker re-parses the manifest
//! into the shared store. The store holds Hermes nested skills too (keyed by
//! their directory leaf name, exactly like the categorized loader inserts
//! them), and the nested write paths share all the plumbing — but they never
//! enqueued the re-parse: the physical file changed while the store, and with
//! it the skill-discover document rendered from the store, kept serving the
//! pre-mutation manifest until remount.
//!
//! These tests require `/dev/fuse` and `fusermount3` (skipped gracefully
//! otherwise). The background mount runs the environment-profile probe loop
//! before the FUSE session comes up, which can take seconds on a loaded
//! host, so readiness is polled instead of assumed after a fixed delay.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{StagingConfig, StagingMatcher, StagingPattern};
use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

struct HermesMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    store: SharedSkillStore,
    _handle: skillfs_fuse::MountHandle,
}

impl HermesMount {
    /// Mount a Hermes workspace seeded by `seed` (which receives the source
    /// dir) and poll until `served` is readable through the mount.
    fn new(seed: impl FnOnce(&std::path::Path), served: &str) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let store: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let config = MountConfig {
            skill_layout: Some(SkillLayout::Hermes),
            ..MountConfig::default()
        };
        let _handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store.clone(),
            MountOptions::default(),
            true,
            config,
        )
        .expect("mount hermes workspace");

        let probe = mountpoint.path().join(served);
        assert!(
            wait_for(Duration::from_secs(30), || {
                std::fs::read_to_string(&probe).is_ok()
            }),
            "hermes mount never served {served}"
        );
        Self {
            source,
            mountpoint,
            store,
            _handle,
        }
    }

    fn mount_path(&self, rel: &str) -> PathBuf {
        self.mountpoint.path().join(rel)
    }

    fn entry_body(&self, key: &str) -> Option<String> {
        self.store.read().get(key).map(|e| e.body.clone())
    }

    /// [`Self::new`] with staging patterns attached: the nested send
    /// sites must suppress store synchronization for staging-private
    /// manifests the same way the observe dispatcher drops them.
    fn new_with_staging(
        seed: impl FnOnce(&std::path::Path),
        served: &str,
        patterns: Vec<StagingPattern>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let store: SharedSkillStore = Arc::new(RwLock::new(store));

        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let staging_config = StagingConfig {
            patterns,
            ..StagingConfig::default()
        };
        let config = MountConfig {
            skill_layout: Some(SkillLayout::Hermes),
            staging_matcher: Some(Arc::new(StagingMatcher::new(staging_config))),
            ..MountConfig::default()
        };
        let _handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store.clone(),
            MountOptions::default(),
            true,
            config,
        )
        .expect("mount hermes workspace with staging patterns");

        let probe = mountpoint.path().join(served);
        assert!(
            wait_for(Duration::from_secs(30), || {
                std::fs::read_to_string(&probe).is_ok()
            }),
            "hermes mount never served {served}"
        );
        Self {
            source,
            mountpoint,
            store,
            _handle,
        }
    }
}

#[test]
fn nested_manifest_appends_refresh_the_store_entry() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
        },
        "cloud/web/SKILL.md",
    );

    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("cloud/web/SKILL.md"))
        .expect("append nested SKILL.md");
    md.write_all(b"appended through the hermes mount\n")
        .expect("write nested SKILL.md");
    drop(md);

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("web")
                .is_some_and(|body| body.contains("appended through the hermes mount"))
        }),
        "nested manifest append must re-parse into the store (leaf key), body is still {:?}",
        m.entry_body("web")
    );
    assert!(
        std::fs::read_to_string(m.source.path().join("cloud/web/SKILL.md"))
            .expect("physical SKILL.md")
            .contains("appended through the hermes mount")
    );
}

#[test]
fn nested_manifest_creates_refresh_the_store_entry() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            // A plain category child directory: present on disk, but the
            // store has no entry for it until a manifest appears.
            std::fs::create_dir_all(src.join("cloud/newbie")).expect("plain category child");
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
        },
        "cloud/web/SKILL.md",
    );

    let mut md = std::fs::File::create(m.mount_path("cloud/newbie/SKILL.md"))
        .expect("create nested SKILL.md");
    md.write_all(b"---\nname: newbie\ndescription: created\n---\ncreated body\n")
        .expect("write created SKILL.md");
    drop(md);

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("newbie")
                .is_some_and(|body| body.contains("created body"))
        }),
        "nested manifest create must re-parse into the store (leaf key)"
    );
}

#[test]
fn nested_manifest_renames_drop_and_restore_the_store_entry() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
        },
        "cloud/web/SKILL.md",
    );

    let md = m.mount_path("cloud/web/SKILL.md");
    let disabled = m.mount_path("cloud/web/DISABLED.md");

    // Renaming the manifest away disables the skill: the store must forget
    // the leaf entry (identity-gated, like the directory-rename arm).
    std::fs::rename(&md, &disabled).expect("rename manifest away");
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.store.read().get("web").is_none()
        }),
        "renaming the nested manifest away must drop the store entry"
    );

    // Renaming it back re-enables the skill: the store must re-learn it.
    std::fs::rename(&disabled, &md).expect("rename manifest back");
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("web")
                .is_some_and(|body| body.contains("original body"))
        }),
        "renaming the nested manifest back must restore the store entry"
    );
}

/// kongche-jbw P2 (collision) on #7745: with both `alpha/web` and
/// `beta/web` nested manifests, discovery keeps `alpha/web`. Appending
/// through the mounted `beta/web/SKILL.md` enqueued the re-parse, but the
/// worker still upserted unchecked at this PR's baseline: the live `web`
/// entry switched to `beta/web`, and renaming the edited beta manifest
/// away then removed `web` entirely although the alpha manifest still
/// existed. The collision-aware consumer from #7742 (which this branch
/// now sits on) must keep the discovery winner through the whole
/// edit-then-rename-away sequence, matching a fresh scan at every step.
#[test]
fn editing_the_losing_nested_candidate_keeps_the_discovery_winner() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            for (dir, desc) in [("alpha/web", "alpha"), ("beta/web", "beta")] {
                std::fs::create_dir_all(src.join(dir)).expect("nested skill dir");
                std::fs::write(
                    src.join(dir).join("SKILL.md"),
                    format!("---\nname: web\ndescription: {desc}\n---\n{desc} body\n"),
                )
                .expect("SKILL.md");
            }
        },
        "alpha/web/SKILL.md",
    );

    let winner_md = m.source.path().join("alpha/web/SKILL.md");
    assert_eq!(
        m.store
            .read()
            .get("web")
            .expect("discovered owner")
            .source_path,
        winner_md,
        "discovery keeps the lexicographically smaller nested source"
    );

    // Append through the mounted LOSING candidate: the write itself must
    // land on disk, but the store key must stay with the discovery
    // winner - a fresh scan of the tree would keep alpha/web.
    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("beta/web/SKILL.md"))
        .expect("append the losing candidate");
    md.write_all(b"appended to the losing candidate\n")
        .expect("write the losing candidate");
    drop(md);
    assert!(
        std::fs::read_to_string(m.source.path().join("beta/web/SKILL.md"))
            .expect("physical SKILL.md")
            .contains("appended to the losing candidate"),
        "the write itself lands on disk"
    );

    std::thread::sleep(Duration::from_millis(1500));
    let entry = m.store.read().get("web").cloned();
    assert!(
        entry.as_ref().is_some_and(|e| e.source_path == winner_md),
        "editing the losing nested candidate must not steal the store key, got {:?}",
        entry.as_ref().map(|e| e.source_path.clone())
    );
    assert!(
        entry.is_some_and(|e| !e.body.contains("appended to the losing candidate")),
        "the loser's edit must not leak into the winner's entry"
    );

    // Renaming the edited loser away is the operator's disable move for
    // that candidate: the retained winner must survive it.
    std::fs::rename(
        m.mount_path("beta/web/SKILL.md"),
        m.mount_path("beta/web/DISABLED.md"),
    )
    .expect("rename the losing manifest away");
    std::thread::sleep(Duration::from_millis(1500));

    let retained = m.store.read().get("web").cloned();
    assert!(
        retained.is_some(),
        "renaming the losing manifest away must not remove the retained winner"
    );
    assert_eq!(
        retained.expect("retained winner").source_path,
        winner_md,
        "the discovery winner keeps the key through the whole sequence"
    );
}

/// kongche-jbw P2 (hidden directories, create arm) on #7745:
/// `NestedSkillMd` also classifies manifests under dot-prefixed
/// categories or leaves, so creating `cloud/.new/SKILL.md` or
/// `.hidden-parent/secret/SKILL.md` through the mount inserted `.new`
/// or `secret` into the store - all excluded by a fresh discovery scan,
/// and skill-discover renders the store directly. The nested reparse
/// sites must apply discovery's hidden-directory eligibility rule.
#[test]
fn hidden_nested_creates_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("cloud/.new")).expect("hidden leaf dir");
            std::fs::create_dir_all(src.join(".hidden-parent/secret"))
                .expect("hidden category dir");
        },
        "cloud/web/SKILL.md",
    );

    for path in ["cloud/.new/SKILL.md", ".hidden-parent/secret/SKILL.md"] {
        let mut md = std::fs::File::create(m.mount_path(path)).expect("create hidden SKILL.md");
        md.write_all(b"---\nname: leaked\ndescription: hidden\n---\nhidden body\n")
            .expect("write hidden SKILL.md");
        drop(md);
    }

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get(".new").is_none(),
        "a manifest under a hidden leaf must not enter the store"
    );
    assert!(
        m.store.read().get("secret").is_none(),
        "a manifest under a hidden category must not enter the store"
    );
    assert!(
        m.store.read().get("web").is_some(),
        "the visible control stays managed"
    );
}

/// kongche-jbw P2 (hidden directories, write arm) on #7745: editing an
/// existing hidden working copy (`cloud/.draft/SKILL.md`,
/// `.hidden-parent/secret/SKILL.md`) through the mount likewise inserted
/// `.draft` / `secret` into the live store while a fresh scan keeps
/// excluding them.
#[test]
fn hidden_nested_writes_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            for dir in ["cloud/.draft", ".hidden-parent/secret"] {
                std::fs::create_dir_all(src.join(dir)).expect("hidden skill dir");
                std::fs::write(
                    src.join(dir).join("SKILL.md"),
                    "---\nname: leaked\ndescription: hidden\n---\nhidden body\n",
                )
                .expect("hidden SKILL.md");
            }
        },
        "cloud/web/SKILL.md",
    );

    for path in ["cloud/.draft/SKILL.md", ".hidden-parent/secret/SKILL.md"] {
        let mut md = std::fs::OpenOptions::new()
            .append(true)
            .open(m.mount_path(path))
            .expect("append hidden SKILL.md");
        md.write_all(b"edited hidden working copy\n")
            .expect("write hidden SKILL.md");
        drop(md);
    }

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get(".draft").is_none(),
        "editing a hidden leaf must not enter the store"
    );
    assert!(
        m.store.read().get("secret").is_none(),
        "editing under a hidden category must not enter the store"
    );
    assert!(m.store.read().get("web").is_some());
}

/// kongche-jbw P2 (hidden directories, rename-onto arm) on #7745:
/// renaming a draft onto `SKILL.md` inside a hidden directory
/// (`cloud/.staged/DRAFT.md` -> `cloud/.staged/SKILL.md`, or under
/// `.hidden-parent/secret/`) enqueued the same re-parse and inserted the
/// hidden leaf; discovery keeps excluding it.
#[test]
fn hidden_nested_renames_onto_skill_md_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("cloud/.staged")).expect("hidden leaf dir");
            std::fs::write(
                src.join("cloud/.staged/DRAFT.md"),
                "---\nname: leaked\ndescription: hidden\n---\nhidden body\n",
            )
            .expect("draft manifest");
            std::fs::create_dir_all(src.join(".hidden-parent/secret"))
                .expect("hidden category dir");
            std::fs::write(
                src.join(".hidden-parent/secret/DISABLED.md"),
                "---\nname: leaked\ndescription: hidden\n---\nhidden body\n",
            )
            .expect("disabled manifest");
        },
        "cloud/web/SKILL.md",
    );

    std::fs::rename(
        m.mount_path("cloud/.staged/DRAFT.md"),
        m.mount_path("cloud/.staged/SKILL.md"),
    )
    .expect("rename onto SKILL.md in a hidden leaf");
    std::fs::rename(
        m.mount_path(".hidden-parent/secret/DISABLED.md"),
        m.mount_path(".hidden-parent/secret/SKILL.md"),
    )
    .expect("rename onto SKILL.md under a hidden category");

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get(".staged").is_none(),
        "a rename onto SKILL.md in a hidden leaf must not enter the store"
    );
    assert!(
        m.store.read().get("secret").is_none(),
        "a rename onto SKILL.md under a hidden category must not enter the store"
    );
    assert!(m.store.read().get("web").is_some());
}

/// kongche-jbw follow-up P2 (debounce coalescing, creation arm) on
/// #7745: nested candidates share their leaf name across categories, but
/// the sync worker debounced solely by that leaf. Creating
/// `alpha/newbie/SKILL.md` and then `beta/newbie/SKILL.md` within one
/// 50 ms batch overwrote the alpha event before either candidate reached
/// the collision check: the live store selected `beta/newbie` while a
/// fresh scan selects `alpha/newbie`. The batch must key every event by
/// its manifest source path so each distinct candidate reaches the
/// collision check.
#[test]
fn same_batch_creates_of_a_colliding_leaf_match_a_fresh_scan() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            // A served probe plus the two empty candidate directories:
            // neither manifest exists at discovery time; both are created
            // through the mount inside one debounce window.
            std::fs::create_dir_all(src.join("alpha/web")).expect("probe skill dir");
            std::fs::write(
                src.join("alpha/web/SKILL.md"),
                "---\nname: web\ndescription: probe\n---\nprobe body\n",
            )
            .expect("probe SKILL.md");
            std::fs::create_dir_all(src.join("alpha/newbie")).expect("alpha candidate dir");
            std::fs::create_dir_all(src.join("beta/newbie")).expect("beta candidate dir");
        },
        "alpha/web/SKILL.md",
    );

    assert!(
        m.store.read().get("newbie").is_none(),
        "no manifest exists yet, so discovery must not know the leaf"
    );

    // Both creates land inside one 50 ms debounce window: the two create
    // callbacks fire back-to-back, one FUSE round trip apart.
    let mut alpha = std::fs::File::create(m.mount_path("alpha/newbie/SKILL.md"))
        .expect("create the alpha candidate");
    let mut beta = std::fs::File::create(m.mount_path("beta/newbie/SKILL.md"))
        .expect("create the beta candidate");
    alpha
        .write_all(b"---\nname: newbie\ndescription: alpha\n---\nalpha body\n")
        .expect("write the alpha candidate");
    beta.write_all(b"---\nname: newbie\ndescription: beta\n---\nbeta body\n")
        .expect("write the beta candidate");
    drop(alpha);
    drop(beta);

    let winner_md = m.source.path().join("alpha/newbie/SKILL.md");
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.store
                .read()
                .get("newbie")
                .is_some_and(|e| e.source_path == winner_md)
        }),
        "same-batch creates of colliding candidates must keep the \
         fresh-scan winner alpha/newbie, got {:?}",
        m.store.read().get("newbie").map(|e| e.source_path.clone())
    );
    let entry = m.store.read().get("newbie").cloned();
    assert!(
        entry
            .as_ref()
            .is_some_and(|e| e.body.contains("alpha body")),
        "the retained winner serves its own body, got {:?}",
        entry.as_ref().map(|e| e.body.clone())
    );
}

/// kongche-jbw follow-up P2 (debounce coalescing, edit arm) on #7745:
/// with an existing discovered winner, appending to the winner and then
/// to the losing candidate in one 50 ms batch coalesced away the
/// winner's refresh - the live entry kept serving its pre-write body
/// while the manifest on disk had moved on. Both edits must reach the
/// store: the winner's re-parse applies, the loser still loses the
/// collision check.
#[test]
fn same_batch_winner_then_loser_edits_refresh_the_winner() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            for (dir, desc) in [("alpha/newbie", "alpha"), ("beta/newbie", "beta")] {
                std::fs::create_dir_all(src.join(dir)).expect("nested skill dir");
                std::fs::write(
                    src.join(dir).join("SKILL.md"),
                    format!("---\nname: newbie\ndescription: {desc}\n---\n{desc} body\n"),
                )
                .expect("SKILL.md");
            }
        },
        "alpha/newbie/SKILL.md",
    );

    let winner_md = m.source.path().join("alpha/newbie/SKILL.md");
    assert_eq!(
        m.store
            .read()
            .get("newbie")
            .expect("discovered owner")
            .source_path,
        winner_md,
        "discovery keeps the lexicographically smaller nested source"
    );

    // Winner edit then loser edit, one FUSE round trip apart: both write
    // callbacks land in the same 50 ms debounce batch.
    let mut alpha = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("alpha/newbie/SKILL.md"))
        .expect("open the winner");
    let mut beta = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("beta/newbie/SKILL.md"))
        .expect("open the loser");
    alpha
        .write_all(b"winner refreshed in the same batch\n")
        .expect("edit the winner");
    beta.write_all(b"loser edited in the same batch\n")
        .expect("edit the loser");
    drop(alpha);
    drop(beta);
    assert!(
        std::fs::read_to_string(m.source.path().join("beta/newbie/SKILL.md"))
            .expect("physical loser SKILL.md")
            .contains("loser edited in the same batch"),
        "the loser's edit lands on disk"
    );

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("newbie").is_some_and(|body| {
                body.contains("winner refreshed in the same batch")
                    && !body.contains("loser edited in the same batch")
            })
        }),
        "a same-batch winner-then-loser edit must refresh the winner's \
         body and keep the loser's edit out; body is {:?}",
        m.entry_body("newbie")
    );
    assert_eq!(
        m.store
            .read()
            .get("newbie")
            .expect("kept winner")
            .source_path,
        winner_md,
        "the winner keeps the key through the same-batch edits"
    );
}

/// kongche-jbw follow-up on #7745 (non-dot staging configuration): the
/// nested send sites enqueue independently of `observe_mutation`, whose
/// staging check they therefore bypassed — with an Exact "pip-staging"
/// pattern, creating a nested manifest inside the staging root inserted
/// the private leaf into the shared store (and with it skill-discover).
/// The send sites must apply the matcher to both the category and the
/// leaf: the physical write still lands, only the store sync is
/// suppressed.
#[test]
fn exact_staging_nested_creates_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new_with_staging(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("pip-staging/new-skill")).expect("staging leaf dir");
        },
        "cloud/web/SKILL.md",
        vec![StagingPattern::Exact("pip-staging".to_string())],
    );

    let mut md = std::fs::File::create(m.mount_path("pip-staging/new-skill/SKILL.md"))
        .expect("create staged SKILL.md through the mount");
    md.write_all(b"---\nname: new-skill\ndescription: staged\n---\nstaged body\n")
        .expect("write staged SKILL.md");
    drop(md);

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get("new-skill").is_none(),
        "a manifest created inside a staging root must not enter the store"
    );
    assert!(
        m.source
            .path()
            .join("pip-staging/new-skill/SKILL.md")
            .exists(),
        "the physical write must be preserved"
    );
    assert!(
        m.store.read().get("web").is_some(),
        "the ordinary-category control stays managed"
    );
}

/// The refresh arm of the same leak: a pre-seeded staging root's nested
/// manifest (scanned into the store under its staging category, hidden
/// from the views by #8037) must not re-parse into the live store when
/// edited through the mount — the store entry keeps serving the
/// scanned body while the physical file carries the edit, exactly like
/// a fresh discovery scan of a staging root that syncs nothing.
#[test]
fn exact_staging_nested_manifest_refreshes_are_suppressed() {
    skip_if_no_fuse!();

    let m = HermesMount::new_with_staging(
        |src| {
            std::fs::create_dir_all(src.join("cloud/notes")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/notes/SKILL.md"),
                "---\nname: notes\ndescription: nested\n---\nordinary body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("pip-staging/web")).expect("staging leaf dir");
            std::fs::write(
                src.join("pip-staging/web/SKILL.md"),
                "---\nname: web\ndescription: staged\n---\nstaged original\n",
            )
            .expect("seed staged SKILL.md");
        },
        "cloud/notes/SKILL.md",
        vec![StagingPattern::Exact("pip-staging".to_string())],
    );

    // Edit the staged manifest through the mount.
    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("pip-staging/web/SKILL.md"))
        .expect("open staged manifest through the mount");
    md.write_all(b"staged edit through the mount\n")
        .expect("append staged manifest");
    drop(md);

    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        m.entry_body("web").as_deref(),
        Some("staged original\n"),
        "a staging-private manifest must not re-parse into the store"
    );
    assert!(
        std::fs::read_to_string(m.source.path().join("pip-staging/web/SKILL.md"))
            .expect("physical staged manifest")
            .contains("staged edit through the mount"),
        "the physical write must be preserved"
    );

    // Ordinary-category control: the same edit shape under an ordinary
    // category still refreshes the store entry.
    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("cloud/notes/SKILL.md"))
        .expect("open ordinary manifest through the mount");
    md.write_all(b"ordinary edit through the mount\n")
        .expect("append ordinary manifest");
    drop(md);
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("notes")
                .is_some_and(|b| b.contains("ordinary edit through the mount"))
        }),
        "an ordinary-category manifest edit must still refresh the store, body is {:?}",
        m.entry_body("notes")
    );
}

/// The PrefixStar arm: `build-*` names a whole family of non-dot
/// staging roots; a nested manifest created under one of them must stay
/// out of the store while the physical write lands.
#[test]
fn prefix_star_staging_nested_creates_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new_with_staging(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("build-deps/tool")).expect("staging leaf dir");
        },
        "cloud/web/SKILL.md",
        vec![StagingPattern::PrefixStar("build-".to_string())],
    );

    let mut md = std::fs::File::create(m.mount_path("build-deps/tool/SKILL.md"))
        .expect("create staged SKILL.md through the mount");
    md.write_all(b"---\nname: tool\ndescription: staged\n---\nstaged body\n")
        .expect("write staged SKILL.md");
    drop(md);

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get("tool").is_none(),
        "a manifest created inside a PrefixStar staging root must not enter the store"
    );
    assert!(
        m.source.path().join("build-deps/tool/SKILL.md").exists(),
        "the physical write must be preserved"
    );
    assert!(
        m.store.read().get("web").is_some(),
        "the ordinary-category control stays managed"
    );
}

/// The PrefixStar refresh arm, mirroring the Exact one.
#[test]
fn prefix_star_staging_nested_manifest_refreshes_are_suppressed() {
    skip_if_no_fuse!();

    let m = HermesMount::new_with_staging(
        |src| {
            std::fs::create_dir_all(src.join("cloud/notes")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/notes/SKILL.md"),
                "---\nname: notes\ndescription: nested\n---\nordinary body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("build-deps/tool")).expect("staging leaf dir");
            std::fs::write(
                src.join("build-deps/tool/SKILL.md"),
                "---\nname: tool\ndescription: staged\n---\nstaged original\n",
            )
            .expect("seed staged SKILL.md");
        },
        "cloud/notes/SKILL.md",
        vec![StagingPattern::PrefixStar("build-".to_string())],
    );

    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.mount_path("build-deps/tool/SKILL.md"))
        .expect("open staged manifest through the mount");
    md.write_all(b"staged edit through the mount\n")
        .expect("append staged manifest");
    drop(md);

    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        m.entry_body("tool").as_deref(),
        Some("staged original\n"),
        "a staging-private manifest must not re-parse into the store"
    );
    assert!(
        std::fs::read_to_string(m.source.path().join("build-deps/tool/SKILL.md"))
            .expect("physical staged manifest")
            .contains("staged edit through the mount"),
        "the physical write must be preserved"
    );
    assert!(
        m.store.read().get("notes").is_some(),
        "the ordinary-category control stays managed"
    );
}

/// kongche-jbw final follow-up P2 on #7745 (deletion path): the
/// create-side registration teaches the store a newly created nested
/// manifest, but the successful unlink path removed store entries only
/// for flat `SkillMd`. Unlinking the freshly created marker left the
/// leaf in the live store, so store-backed discovery kept advertising a
/// skill whose manifest no longer exists — a fresh scan has no such
/// entry. The deletion must remove the entry the deleted manifest
/// backs, ownership-gated like the flat arm.
#[test]
fn created_nested_manifests_leave_the_store_when_unlinked() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/newbie")).expect("plain category child");
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
        },
        "cloud/web/SKILL.md",
    );

    let mut md = std::fs::File::create(m.mount_path("cloud/newbie/SKILL.md"))
        .expect("create nested SKILL.md");
    md.write_all(b"---\nname: newbie\ndescription: created\n---\ncreated body\n")
        .expect("write created SKILL.md");
    drop(md);

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("newbie")
                .is_some_and(|body| body.contains("created body"))
        }),
        "nested manifest create must re-parse into the store (leaf key)"
    );

    std::fs::remove_file(m.mount_path("cloud/newbie/SKILL.md"))
        .expect("unlink nested SKILL.md through the mount");

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.store.read().get("newbie").is_none()
        }),
        "unlinking the created nested manifest must remove its store entry, body is {:?}",
        m.entry_body("newbie")
    );

    let mut fresh = SkillStore::new();
    fresh.load_from_directory(m.source.path(), &ParseConfig::default());
    assert!(
        fresh.get("newbie").is_none(),
        "a fresh scan has no newbie entry; the live store must agree"
    );
    assert!(
        m.store.read().get("web").is_some(),
        "the ordinary control skill must stay in the store"
    );
}

/// The ownership gate of the deletion path: the store keys nested
/// skills by their leaf name, so unlinking a losing same-leaf candidate
/// must not drop the retained owner — its manifest is untouched and a
/// fresh scan re-discovers it.
#[test]
fn unlinked_losing_nested_candidate_keeps_the_retained_owner() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("alpha/web")).expect("discovered owner dir");
            std::fs::write(
                src.join("alpha/web/SKILL.md"),
                "---\nname: web\ndescription: owner\n---\nowner body\n",
            )
            .expect("seed owner SKILL.md");
            std::fs::create_dir_all(src.join("beta/web")).expect("losing candidate dir");
            std::fs::write(
                src.join("beta/web/SKILL.md"),
                "---\nname: web\ndescription: loser\n---\nloser body\n",
            )
            .expect("seed losing SKILL.md");
        },
        "alpha/web/SKILL.md",
    );

    assert!(
        m.entry_body("web")
            .is_some_and(|b| b.contains("owner body")),
        "discovery keeps the lexicographically smaller nested source"
    );

    std::fs::remove_file(m.mount_path("beta/web/SKILL.md"))
        .expect("unlink the losing candidate through the mount");

    assert!(
        m.entry_body("web")
            .is_some_and(|b| b.contains("owner body")),
        "unlinking a losing same-leaf candidate must keep the retained owner"
    );
    assert!(
        std::fs::read_to_string(m.source.path().join("alpha/web/SKILL.md"))
            .expect("owner manifest")
            .contains("owner body"),
        "the retained owner's manifest must stay intact"
    );
    let mut fresh = SkillStore::new();
    fresh.load_from_directory(m.source.path(), &ParseConfig::default());
    assert!(
        fresh
            .get("web")
            .is_some_and(|e| e.body.contains("owner body")),
        "a fresh scan still discovers the owner; the live store must agree"
    );
}

/// kongche-jbw final follow-up P2 on #7745 (regular marker): discovery
/// loads a nested skill only from a real directory whose `SKILL.md` is
/// a regular file (`has_regular_skill_md`, no-follow), but the sync
/// worker's parser follows symlinks. Renaming a relative symlink onto a
/// nested `SKILL.md` path registered the linked payload as a skill
/// while a fresh scan excludes it. The send sites must apply
/// discovery's regular-manifest rule; the physical rename is preserved.
#[test]
fn symlinked_nested_manifests_stay_out_of_the_store() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
            std::fs::create_dir_all(src.join("cloud/linkskill")).expect("symlink leaf dir");
            std::fs::write(
                src.join("cloud/linkskill/payload.md"),
                "---\nname: linkskill\ndescription: linked\n---\nlinked payload\n",
            )
            .expect("seed payload");
            std::os::unix::fs::symlink("payload.md", src.join("cloud/linkskill/draft.md"))
                .expect("seed relative symlink");
            std::fs::create_dir_all(src.join("cloud/regular")).expect("regular leaf dir");
            std::fs::write(
                src.join("cloud/regular/DRAFT.md"),
                "---\nname: regular\ndescription: ordinary\n---\nordinary body\n",
            )
            .expect("seed regular draft");
        },
        "cloud/web/SKILL.md",
    );

    std::fs::rename(
        m.mount_path("cloud/linkskill/draft.md"),
        m.mount_path("cloud/linkskill/SKILL.md"),
    )
    .expect("rename the symlink onto SKILL.md");

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        m.store.read().get("linkskill").is_none(),
        "a symlink marker must not register a skill a fresh scan excludes"
    );
    let marker = m.source.path().join("cloud/linkskill/SKILL.md");
    assert!(
        std::fs::symlink_metadata(&marker)
            .expect("marker stat")
            .file_type()
            .is_symlink(),
        "the physical marker must remain a symlink"
    );
    assert!(
        std::fs::symlink_metadata(m.source.path().join("cloud/linkskill/payload.md"))
            .expect("payload stat")
            .file_type()
            .is_file(),
        "the physical payload must stay present"
    );
    let mut fresh = SkillStore::new();
    fresh.load_from_directory(m.source.path(), &ParseConfig::default());
    assert!(
        fresh.get("linkskill").is_none(),
        "a fresh scan excludes the symlinked marker; the live store must agree"
    );

    // Ordinary regular-manifest control: the same rename shape onto a
    // regular draft still syncs the store entry.
    std::fs::rename(
        m.mount_path("cloud/regular/DRAFT.md"),
        m.mount_path("cloud/regular/SKILL.md"),
    )
    .expect("rename a regular draft onto SKILL.md");
    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("regular")
                .is_some_and(|b| b.contains("ordinary body"))
        }),
        "a regular manifest renamed onto SKILL.md must still sync, body is {:?}",
        m.entry_body("regular")
    );
}

/// kongche-jbw final follow-up P2 on #7745 (invalidation transition):
/// replacing an already-registered manifest with a non-regular marker
/// left the old owner in the store. The rename-onto send site skipped
/// the Reparse for the non-regular marker, but the old path was a
/// passthrough file, so no removal fired either: the physical `SKILL.md`
/// became a symlink, a fresh scan excluded the skill, and the live store
/// kept serving the old ordinary body. The replacement must withdraw the
/// entry the destination identity backs.
#[test]
fn non_regular_replacement_of_the_registered_owner_withdraws_the_entry() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("cloud/newbie")).expect("nested skill dir");
            std::fs::write(
                src.join("cloud/newbie/payload.md"),
                "---\nname: newbie\ndescription: linked\n---\nlinked payload\n",
            )
            .expect("seed payload");
            std::os::unix::fs::symlink("payload.md", src.join("cloud/newbie/draft.md"))
                .expect("seed relative symlink");
            std::fs::create_dir_all(src.join("cloud/web")).expect("served nested skill dir");
            std::fs::write(
                src.join("cloud/web/SKILL.md"),
                "---\nname: web\ndescription: nested\n---\noriginal body\n",
            )
            .expect("seed SKILL.md");
        },
        "cloud/web/SKILL.md",
    );

    let mut md = std::fs::File::create(m.mount_path("cloud/newbie/SKILL.md"))
        .expect("create nested SKILL.md");
    md.write_all(b"---\nname: newbie\ndescription: created\n---\ncreated body\n")
        .expect("write created SKILL.md");
    drop(md);

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry_body("newbie")
                .is_some_and(|body| body.contains("created body"))
        }),
        "the ordinary manifest must be registered before the replacement, body is {:?}",
        m.entry_body("newbie")
    );

    std::fs::rename(
        m.mount_path("cloud/newbie/draft.md"),
        m.mount_path("cloud/newbie/SKILL.md"),
    )
    .expect("replace the registered manifest with the symlink");

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.store.read().get("newbie").is_none()
        }),
        "a non-regular marker replacing its registered manifest must withdraw the \
         store entry, body is {:?}",
        m.entry_body("newbie")
    );
    let marker = m.source.path().join("cloud/newbie/SKILL.md");
    assert!(
        std::fs::symlink_metadata(&marker)
            .expect("marker stat")
            .file_type()
            .is_symlink(),
        "the physical marker must remain the renamed symlink"
    );
    assert!(
        std::fs::symlink_metadata(m.source.path().join("cloud/newbie/payload.md"))
            .expect("payload stat")
            .file_type()
            .is_file(),
        "the physical payload must stay present"
    );
    let mut fresh = SkillStore::new();
    fresh.load_from_directory(m.source.path(), &ParseConfig::default());
    assert!(
        fresh.get("newbie").is_none(),
        "a fresh scan excludes the symlinked marker; the live store must agree"
    );
}

/// The ownership gate of the withdrawal: replacing a losing candidate's
/// marker with a non-regular marker must keep the retained owner — the
/// destination identity does not back the stored entry, and a fresh scan
/// still selects the winner.
#[test]
fn non_regular_replacement_of_a_losing_candidate_keeps_the_retained_owner() {
    skip_if_no_fuse!();

    let m = HermesMount::new(
        |src| {
            std::fs::create_dir_all(src.join("alpha/web")).expect("discovered owner dir");
            std::fs::write(
                src.join("alpha/web/SKILL.md"),
                "---\nname: web\ndescription: owner\n---\nowner body\n",
            )
            .expect("seed owner SKILL.md");
            std::fs::create_dir_all(src.join("beta/web")).expect("losing candidate dir");
            std::fs::write(
                src.join("beta/web/SKILL.md"),
                "---\nname: web\ndescription: loser\n---\nloser body\n",
            )
            .expect("seed losing SKILL.md");
            std::fs::write(
                src.join("beta/web/payload.md"),
                "---\nname: web\ndescription: linked\n---\nlinked payload\n",
            )
            .expect("seed payload");
            std::os::unix::fs::symlink("payload.md", src.join("beta/web/draft.md"))
                .expect("seed relative symlink");
        },
        "alpha/web/SKILL.md",
    );

    assert!(
        m.entry_body("web")
            .is_some_and(|b| b.contains("owner body")),
        "discovery keeps the lexicographically smaller nested source"
    );

    std::fs::rename(
        m.mount_path("beta/web/draft.md"),
        m.mount_path("beta/web/SKILL.md"),
    )
    .expect("replace the losing candidate's marker with the symlink");

    assert!(
        m.entry_body("web")
            .is_some_and(|b| b.contains("owner body")),
        "replacing a losing candidate's marker must keep the retained owner"
    );
    assert!(
        std::fs::read_to_string(m.source.path().join("alpha/web/SKILL.md"))
            .expect("owner manifest")
            .contains("owner body"),
        "the retained owner's manifest must stay intact"
    );
    assert!(
        std::fs::symlink_metadata(m.source.path().join("beta/web/SKILL.md"))
            .expect("loser marker stat")
            .file_type()
            .is_symlink(),
        "the losing candidate's marker must physically be the renamed symlink"
    );
    let mut fresh = SkillStore::new();
    fresh.load_from_directory(m.source.path(), &ParseConfig::default());
    assert!(
        fresh
            .get("web")
            .is_some_and(|e| e.body.contains("owner body")),
        "a fresh scan still discovers the owner; the live store must agree"
    );
}
