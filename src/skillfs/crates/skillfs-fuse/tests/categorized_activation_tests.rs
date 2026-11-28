//! FUSE mount coverage for categorized live skills and activation targets.

use std::fs::{self, File, FileTimes};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActivationReloadController, ActivationWatcher, ActiveMappingError, ActiveResolverError,
    ActiveSkillResolver, ActiveTarget, LedgerResolveResult, NoopProtocolEventWriter, ReloadOutcome,
    bootstrap_activation, enumerate_hermes_skill_ids,
};
use skillfs_fuse::{
    MountConfig, MountHandle, MountOptions, SkillLayout, mount_background_configured,
};

#[path = "common/mod.rs"]
mod common;

const ALLOWED: &str = "published/allowed";
const HIDDEN: &str = "private/hidden";
const V1: &str = ".skill-meta/versions/v000001.snapshot";
const V2: &str = ".skill-meta/versions/v000002.snapshot";
const V3: &str = ".skill-meta/versions/v000003.snapshot";

fn write_manifest(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).expect("create skill dir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} description\n---\n\n{body}\n"),
    )
    .expect("write skill manifest");
}

fn write_activation(dir: &Path, target: Option<&str>, seconds: u64) {
    let meta = dir.join(".skill-meta");
    fs::create_dir_all(&meta).expect("create activation dir");
    let path = meta.join("activation.json");
    fs::write(
        &path,
        serde_json::json!({ "schemaVersion": 1, "target": target }).to_string(),
    )
    .expect("write activation");
    // Strictly increasing mtimes make freshness checks deterministic.
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    File::open(path)
        .expect("open activation for setting mtime")
        .set_times(FileTimes::new().set_modified(timestamp))
        .expect("set activation mtime");
}

fn resolve(skill: &str, status: &str, decision: &str, target: Option<&str>) -> LedgerResolveResult {
    let json = serde_json::json!({
        "schemaVersion": 1,
        "skillName": skill,
        "status": status,
        "decision": decision,
        "target": target,
        "targetKind": target.map(|_| "relative_to_skill_dir"),
    });
    LedgerResolveResult::from_json_str(&json.to_string()).expect("valid provider response")
}

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    mountpoint: PathBuf,
    store: SharedSkillStore,
    resolver: Arc<ActiveSkillResolver>,
}

impl Fixture {
    fn new(with_views: bool, extra_current: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("p2a-sec-fixes-")
            .tempdir()
            .expect("fixture directory");
        let source = root.path().join("source");
        let mountpoint = root.path().join("mount");
        fs::create_dir(&mountpoint).expect("create mountpoint");

        write_manifest(&source.join(ALLOWED), "allowed", "live body");
        write_manifest(&source.join(HIDDEN), "hidden", "private body");
        for (version, body) in [
            (V1, "first snapshot"),
            (V2, "second snapshot"),
            (V3, "third snapshot"),
        ] {
            write_manifest(&source.join(ALLOWED).join(version), "allowed", body);
        }
        if extra_current {
            write_manifest(&source.join("published/current"), "current", "current body");
            write_manifest(
                &source.join("private/unmapped"),
                "unmapped",
                "unmapped body",
            );
        }
        if with_views {
            fs::write(
                source.join("skillfs-views.toml"),
                r#"[[view]]
name = "default"
default = true
skills = []

[[view]]
name = "optional"
skills = ["allowed", "current", "hidden", "unmapped"]
"#,
            )
            .expect("write views");
        }
        let mut store = SkillStore::new();
        let errors = store.load_from_directory(&source, &ParseConfig::default());
        assert!(errors.is_empty(), "{errors:?}");
        let store = Arc::new(RwLock::new(store));
        let resolver = Arc::new(ActiveSkillResolver::new(&source).with_store(store.clone()));
        Self {
            _root: root,
            source,
            mountpoint,
            store,
            resolver,
        }
    }

    fn mount(&self, in_place: bool) -> MountHandle {
        let mountpoint = if in_place {
            &self.source
        } else {
            &self.mountpoint
        };
        let handle = mount_background_configured(
            mountpoint,
            &self.source,
            self.store.clone(),
            MountOptions::default(),
            in_place,
            MountConfig {
                skill_layout: Some(SkillLayout::Flat),
                active_resolver: Some(self.resolver.clone()),
                ..MountConfig::default()
            },
        )
        .expect("mount categorized source");
        assert!(handle.is_mounted());
        handle
    }
}

fn assert_body(skills: &Path, name: &str, expected: &str) {
    let body =
        fs::read_to_string(skills.join(name).join("SKILL.md")).expect("read mounted SKILL.md");
    assert!(body.ends_with(&format!("{expected}\n")), "{body}");
}

fn assert_hidden(skills: &Path) {
    let err = fs::metadata(skills.join("hidden")).expect_err("hidden skill lookup must fail");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn categorized_file_activation_bootstraps_polls_and_watches_the_physical_directory() {
    skip_if_no_fuse!();
    let fixture = Fixture::new(false, false);
    let allowed_dir = fixture.source.join(ALLOWED);
    write_activation(&allowed_dir, Some(V1), 1);
    write_activation(&fixture.source.join(HIDDEN), None, 1);
    let names = ["allowed".to_owned(), "hidden".to_owned()];
    let results = bootstrap_activation(&fixture.source, &names, &fixture.resolver);
    assert!(
        results.iter().all(|(_, target)| target.is_ok()),
        "{results:?}"
    );

    let handle = fixture.mount(false);
    let skills = fixture.mountpoint.join("skills");
    assert_body(&skills, "allowed", "first snapshot");
    assert_hidden(&skills);

    let reload = Arc::new(ActivationReloadController::with_defaults(
        &fixture.source,
        fixture.resolver.clone(),
    ));
    let watcher =
        ActivationWatcher::with_defaults(reload.clone(), Arc::new(NoopProtocolEventWriter));
    assert_eq!(watcher.tick_for_testing().len(), 2);
    let baseline = reload.snapshot_freshness("allowed");
    write_activation(&allowed_dir, Some(V2), 2);
    assert!(matches!(
        reload.poll_reload_skill("allowed", baseline),
        ReloadOutcome::Updated(_)
    ));
    assert_body(&skills, "allowed", "second snapshot");

    write_activation(&allowed_dir, Some(V3), 3);
    let updates = watcher.tick_for_testing();
    assert!(
        matches!(updates.as_slice(), [(name, ReloadOutcome::Updated(_))] if name == "allowed"),
        "{updates:?}"
    );
    assert_body(&skills, "allowed", "third snapshot");
    assert_hidden(&skills);
    handle.unmount().expect("unmount categorized source");
}

#[test]
fn categorized_decisions_serve_current_and_fallback_with_and_without_overmount() {
    skip_if_no_fuse!();
    for in_place in [false, true] {
        let fixture = Fixture::new(false, false);
        let current = resolve("allowed", "pass", "current", None);
        let fallback = resolve("allowed", "deny", "fallback", Some(V1));
        let hidden = resolve("hidden", "none", "hidden", None);
        let allowed_dir = fixture.source.join(ALLOWED);
        fs::write(allowed_dir.join("notes.txt"), "live").expect("seed live companion");
        fs::write(allowed_dir.join(V1).join("notes.txt"), "snapshot")
            .expect("seed snapshot companion");
        assert_eq!(
            fixture
                .resolver
                .set_from_resolve_for_expected("allowed", &current)
                .expect("set current"),
            ActiveTarget::Current {
                source_dir: allowed_dir.clone()
            },
        );
        fixture
            .resolver
            .set_from_resolve_for_expected("hidden", &hidden)
            .expect("set hidden");

        let handle = fixture.mount(in_place);
        let skills = if in_place {
            fixture.source.clone()
        } else {
            fixture.mountpoint.join("skills")
        };
        assert_body(&skills, "allowed", "live body");
        assert_hidden(&skills);
        assert_eq!(
            fixture
                .resolver
                .set_from_resolve_for_skill_dir("allowed", allowed_dir.clone(), &fallback)
                .expect("set fallback")
                .read_dir(),
            Some(allowed_dir.join(V1).as_path()),
        );
        assert_body(&skills, "allowed", "first snapshot");
        assert_hidden(&skills);
        let mounted_companion = skills.join("allowed/notes.txt");
        fs::write(&mounted_companion, "updated live").expect("write through snapshot view");
        assert_eq!(
            fs::read_to_string(&mounted_companion).expect("read mounted companion"),
            "snapshot",
        );
        handle.unmount().expect("unmount categorized source");
        assert_eq!(
            fs::read_to_string(allowed_dir.join("notes.txt"))
                .expect("read physical live companion"),
            "updated live",
        );
    }
}

#[test]
fn hermes_top_level_snapshot_writes_only_to_the_top_level_live_skill() {
    skip_if_no_fuse!();
    for in_place in [true, false] {
        let root = tempfile::tempdir().expect("fixture directory");
        let source = root.path().join("source");
        let top = source.join("demo");
        let nested = source.join("a-category/demo");
        let snapshot = top.join(V1);
        write_manifest(&top, "demo", "top-level live body");
        write_manifest(&nested, "demo", "categorized live body");
        write_manifest(&snapshot, "demo", "top-level snapshot body");
        for (dir, body) in [
            (&top, "top-level live notes"),
            (&nested, "categorized live notes"),
            (&snapshot, "snapshot notes"),
        ] {
            fs::write(dir.join("notes.txt"), body).expect("seed notes");
        }
        write_activation(&top, Some(V1), 1);
        write_activation(&nested, None, 1);

        let resolver = ActiveSkillResolver::new(&source);
        let ids = enumerate_hermes_skill_ids(&source);
        let results = bootstrap_activation(&source, &ids, &resolver);
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|(_, target)| target.is_ok()),
            "{results:?}"
        );

        let mut store = SkillStore::new();
        let errors = store.load_from_directory(&source, &ParseConfig::default());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            store
                .get("demo")
                .expect("duplicate leaf in store")
                .source_path,
            nested.join("SKILL.md"),
        );

        let mountpoint = if in_place {
            source.clone()
        } else {
            let mountpoint = root.path().join("mount");
            fs::create_dir(&mountpoint).expect("create mountpoint");
            mountpoint
        };
        let handle = mount_background_configured(
            &mountpoint,
            &source,
            Arc::new(RwLock::new(store)),
            MountOptions::default(),
            in_place,
            MountConfig {
                skill_layout: Some(SkillLayout::Hermes),
                active_resolver: Some(Arc::new(resolver)),
                ..MountConfig::default()
            },
        )
        .expect("mount Hermes source");
        let skills = if in_place {
            mountpoint
        } else {
            mountpoint.join("skills")
        };

        assert_body(&skills, "demo", "top-level snapshot body");
        let err =
            fs::metadata(skills.join("a-category/demo")).expect_err("categorized skill is hidden");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        let mounted_top = skills.join("demo");
        let notes = mounted_top.join("notes.txt");
        assert_eq!(
            fs::read_to_string(&notes).expect("read snapshot notes"),
            "snapshot notes",
        );
        fs::write(&notes, "updated top-level live notes").expect("write mounted notes");
        assert_eq!(
            fs::read_to_string(&notes).expect("read snapshot notes after write"),
            "snapshot notes",
        );
        fs::write(mounted_top.join("new.txt"), "new top-level file")
            .expect("create a file through the snapshot view");

        handle.unmount().expect("unmount Hermes source");
        for (dir, expected) in [
            (&top, "updated top-level live notes"),
            (&nested, "categorized live notes"),
            (&snapshot, "snapshot notes"),
        ] {
            assert_eq!(
                fs::read_to_string(dir.join("notes.txt")).expect("read physical notes"),
                expected
            );
        }
        assert_eq!(
            fs::read_to_string(top.join("new.txt")).expect("read created live file"),
            "new top-level file",
        );
        assert!(!nested.join("new.txt").exists());
        assert!(!snapshot.join("new.txt").exists());
    }
}

#[test]
fn categorized_snapshot_long_path_listing_metadata_and_read_agree() {
    skip_if_no_fuse!();
    let fixture = Fixture::new(false, false);
    let live = fixture.source.join(ALLOWED);
    let snapshot = live.join(V1);
    write_activation(&live, Some(V1), 1);
    let results = bootstrap_activation(&fixture.source, &["allowed".to_owned()], &fixture.resolver);
    assert!(matches!(
        results.as_slice(),
        [(_, Ok(ActiveTarget::Snapshot { .. }))]
    ));

    // The snapshot parent and the mounted leaf fit within PATH_MAX. Only
    // the snapshot's absolute leaf path requires a parent-fd retry.
    let path_max = libc::PATH_MAX as usize;
    let parent_limit = path_max - 64;
    let mut relative_parent = PathBuf::new();
    while snapshot.join(&relative_parent).as_os_str().len() + 202 < parent_limit {
        relative_parent.push("d".repeat(200));
    }
    let bytes_left = parent_limit - snapshot.join(&relative_parent).as_os_str().len();
    if bytes_left > 1 {
        relative_parent.push("d".repeat(bytes_left - 1));
    }
    let snapshot_parent = snapshot.join(&relative_parent);
    let live_parent = live.join(&relative_parent);
    fs::create_dir_all(&snapshot_parent).expect("create deep snapshot directory");
    fs::create_dir_all(&live_parent).expect("create deep live directory");
    let leaf = "f".repeat(80);
    let snapshot_bytes = "snapshot-only file contents";
    let parent_fd = File::open(&snapshot_parent).expect("open snapshot parent");
    fs::write(
        format!("/proc/self/fd/{}/{}", parent_fd.as_raw_fd(), leaf),
        snapshot_bytes,
    )
    .expect("write snapshot leaf through parent fd");
    let err = fs::symlink_metadata(snapshot_parent.join(&leaf))
        .expect_err("absolute snapshot leaf must exceed PATH_MAX");
    assert_eq!(err.raw_os_error(), Some(libc::ENAMETOOLONG));

    let live_leaf = live_parent.join(&leaf);
    fs::write(&live_leaf, "live file contents").expect("seed live counterpart");
    let handle = fixture.mount(false);
    fs::remove_file(&live_leaf).expect("remove live counterpart after activation");

    let mounted_parent = fixture
        .mountpoint
        .join("skills/allowed")
        .join(relative_parent);
    let mounted_leaf = mounted_parent.join(&leaf);
    assert!(mounted_leaf.as_os_str().len() < path_max);
    assert!(
        fs::read_dir(&mounted_parent)
            .expect("list mounted snapshot directory")
            .any(|entry| entry.expect("read snapshot entry").file_name() == leaf.as_str())
    );
    let expected_len = snapshot_bytes.len() as u64;
    assert_eq!(
        fs::metadata(&mounted_leaf)
            .expect("lookup snapshot leaf")
            .len(),
        expected_len
    );
    let pinned = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(&mounted_leaf)
        .expect("pin the snapshot dentry");
    // Check getattr on the existing inode after the lookup attr expires.
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        pinned
            .metadata()
            .expect("refresh snapshot attributes")
            .len(),
        expected_len
    );
    assert_eq!(
        fs::read_to_string(&mounted_leaf).expect("open long snapshot leaf"),
        snapshot_bytes
    );
    drop(pinned);
    handle.unmount().expect("unmount categorized source");
}

#[test]
fn discover_omits_hidden_and_unmapped_skills_from_every_rendered_section() {
    skip_if_no_fuse!();
    for with_views in [false, true] {
        let fixture = Fixture::new(with_views, true);
        for (id, result) in [
            ("allowed", resolve("allowed", "deny", "fallback", Some(V1))),
            ("current", resolve("current", "pass", "current", None)),
            ("hidden", resolve("hidden", "none", "hidden", None)),
        ] {
            fixture
                .resolver
                .set_from_resolve_for_expected(id, &result)
                .expect("set activation");
        }

        let handle = fixture.mount(false);
        let content = fs::read_to_string(fixture.mountpoint.join("skills/skill-discover/SKILL.md"))
            .expect("read discover manifest");
        for name in ["hidden", "unmapped", "private"] {
            assert!(!content.contains(name), "{content}");
        }
        assert!(
            content.contains("| allowed |") && content.contains("| current |"),
            "{content}"
        );
        if with_views {
            let base = fixture.source.join("published");
            assert!(
                content.contains(&format!("Base path: `{}`", base.display())),
                "{content}"
            );
            assert!(
                content.contains("description: 'Hidden skills: allowed, current'"),
                "{content}"
            );
            assert!(content.contains("| allowed/SKILL.md |"), "{content}");
        }
        handle.unmount().expect("unmount categorized source");
    }
}

#[test]
fn categorized_activation_does_not_follow_a_store_ownership_change() {
    skip_if_no_fuse!();
    for old_decision in [
        resolve("demo", "pass", "current", None),
        resolve("demo", "deny", "fallback", Some(V1)),
    ] {
        let fixture = Fixture::new(false, false);
        let old_dir = fixture.source.join("cat_a/demo");
        let new_dir = fixture.source.join("cat_b/demo");
        write_manifest(&old_dir, "demo", "old live");
        write_manifest(&old_dir.join(V1), "demo", "old snapshot");
        write_manifest(&new_dir, "demo", "new live");
        let load = |dir: &Path| {
            let entry = skillfs_core::parser::parse_skill_file(&dir.join("SKILL.md"))
                .expect("parse source manifest");
            fixture.store.write().upsert(entry);
        };
        load(&old_dir);
        let target = fixture
            .resolver
            .set_from_resolve_for_expected("demo", &old_decision)
            .expect("activate the old owner");
        let old_body = match target {
            ActiveTarget::Current { .. } => "old live",
            ActiveTarget::Snapshot { .. } => "old snapshot",
            ActiveTarget::Hidden { .. } => panic!("old owner must be visible"),
        };

        let handle = fixture.mount(false);
        let skills = fixture.mountpoint.join("skills");
        let manifest = skills.join("demo/SKILL.md");
        let discover = || {
            fs::read_to_string(skills.join("skill-discover/SKILL.md"))
                .expect("read discover manifest")
        };
        assert_body(&skills, "demo", old_body);
        assert!(discover().contains("| demo |"));
        let mut pinned = File::open(&manifest).expect("open manifest on the old owner");
        let dir_pin = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
            .open(skills.join("demo"))
            .expect("pin old directory inode without opening a listing");
        fs::write(new_dir.join("unapproved.txt"), "new owner's file").expect("seed new owner");

        load(&new_dir);
        // Reopen through the pinned dentry so opendir must decide on the
        // old inode even if a fresh lookup would already reject it.
        let dir_fd_path = format!("/proc/self/fd/{}", dir_pin.as_raw_fd());
        let err = fs::read_dir(dir_fd_path).expect_err("stale directory open must fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        drop(dir_pin);
        let err = File::open(&manifest).expect_err("stale manifest open must fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        // Let the kernel's one-second positive directory entry and attr expire.
        std::thread::sleep(Duration::from_millis(1100));
        let err = fs::metadata(skills.join("demo")).expect_err("stale directory lookup must fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
        assert!(!discover().contains("| demo |"));

        let err = fixture
            .resolver
            .set_from_resolve_for_skill_dir("demo", old_dir, &old_decision)
            .expect_err("reject the old owner's in-flight decision");
        assert!(matches!(
            err,
            ActiveResolverError::Mapping(ActiveMappingError::StaleSkillDir { .. })
        ));

        let current = resolve("demo", "pass", "current", None);
        fixture
            .resolver
            .set_from_resolve_for_expected("demo", &current)
            .expect("activate the new owner");
        assert_body(&skills, "demo", "new live");
        assert!(discover().contains("| demo |"));

        let mut content = String::new();
        pinned
            .read_to_string(&mut content)
            .expect("read pinned manifest");
        assert!(content.ends_with(&format!("{old_body}\n")), "{content}");
        drop(pinned);
        handle.unmount().expect("unmount categorized source");
    }
}

#[test]
fn in_place_categorized_current_companion_file_writes_through() {
    skip_if_no_fuse!();
    let fixture = Fixture::new(false, false);
    let companion = fixture.source.join(ALLOWED).join("notes.txt");
    fs::write(&companion, "before").expect("seed companion file");
    fixture
        .resolver
        .set_from_resolve_for_expected("allowed", &resolve("allowed", "pass", "current", None))
        .expect("set current");

    let handle = fixture.mount(true);
    let mounted_skill = fixture.source.join("allowed");
    let mounted_file = mounted_skill.join("notes.txt");
    assert_body(&fixture.source, "allowed", "live body");
    assert!(
        fs::read_dir(&mounted_skill)
            .expect("list mounted skill")
            .any(|entry| { entry.expect("read directory entry").file_name() == "notes.txt" })
    );
    assert_eq!(
        fs::metadata(&mounted_file)
            .expect("stat mounted companion")
            .len(),
        6
    );
    fs::write(&mounted_file, "after").expect("write mounted companion");
    assert_eq!(
        fs::read_to_string(&mounted_file).expect("read mounted companion"),
        "after"
    );

    handle.unmount().expect("unmount categorized source");
    assert_eq!(
        fs::read_to_string(&companion).expect("read physical companion"),
        "after"
    );
}
