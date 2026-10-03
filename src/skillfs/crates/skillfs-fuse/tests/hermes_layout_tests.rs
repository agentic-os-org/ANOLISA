//! Integration tests for the Hermes skill layout mode.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{MountFixture, create_skill_dir, list_dir_names};

fn seed_hermes_workspace(dir: &Path) {
    std::fs::create_dir_all(dir.join(".hub")).unwrap();
    std::fs::write(dir.join(".hub/config.json"), r#"{"version": 1}"#).unwrap();
    std::fs::write(dir.join(".bundled_manifest"), "manifest-content").unwrap();
    std::fs::write(dir.join(".no-bundled-skills"), "").unwrap();

    let apple_notes = dir.join("apple/apple-notes");
    std::fs::create_dir_all(&apple_notes).unwrap();
    std::fs::write(
        apple_notes.join("SKILL.md"),
        "---\nname: apple-notes\ndescription: notes\n---\nApple Notes skill body.\n",
    )
    .unwrap();

    let apple_music = dir.join("apple/apple-music");
    std::fs::create_dir_all(&apple_music).unwrap();
    std::fs::write(
        apple_music.join("SKILL.md"),
        "---\nname: apple-music\ndescription: music\n---\n",
    )
    .unwrap();
}

// -----------------------------------------------------------------------
// 1. Flat mode behavior unchanged
// -----------------------------------------------------------------------

#[test]
fn flat_mode_in_place_unchanged() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place(|dir| {
        create_skill_dir(dir, "my-skill");
    });

    let md_path = fix.mountpoint().join("my-skill/SKILL.md");
    let content = std::fs::read_to_string(&md_path).expect("read SKILL.md");
    assert!(
        content.contains("my-skill"),
        "flat in-place SKILL.md should be readable"
    );
}

// -----------------------------------------------------------------------
// 2. Hermes mode management path passthrough
// -----------------------------------------------------------------------

#[test]
fn hermes_management_path_stat() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let hub = fix.mountpoint().join(".hub");
    let meta = std::fs::metadata(&hub).expect("stat .hub");
    assert!(meta.is_dir(), ".hub must be a directory");
}

#[test]
fn hermes_management_path_readdir() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let hub = fix.mountpoint().join(".hub");
    let entries = list_dir_names(&hub);
    assert!(
        entries.contains(&"config.json".to_string()),
        ".hub readdir should contain config.json, got: {:?}",
        entries
    );
}

#[test]
fn hermes_management_path_mkdir_eexist() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let hub = fix.mountpoint().join(".hub");
    assert!(hub.exists(), "stat .hub should succeed first");
    let err = std::fs::create_dir(&hub).expect_err("mkdir .hub should fail");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EEXIST),
        "mkdir existing .hub must return EEXIST, got: {}",
        err
    );
}

// -----------------------------------------------------------------------
// 3. Hermes mode manifest passthrough
// -----------------------------------------------------------------------

#[test]
fn hermes_manifest_stat_and_read() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let manifest = fix.mountpoint().join(".bundled_manifest");
    let meta = std::fs::metadata(&manifest).expect("stat .bundled_manifest");
    assert!(meta.is_file(), ".bundled_manifest must be a regular file");

    let content = std::fs::read_to_string(&manifest).expect("read .bundled_manifest");
    assert_eq!(content, "manifest-content");
}

// -----------------------------------------------------------------------
// 4. Hermes mode category dir is container
// -----------------------------------------------------------------------

#[test]
fn hermes_category_dir_readdir() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let apple = fix.mountpoint().join("apple");
    let entries = list_dir_names(&apple);
    assert!(
        entries.contains(&"apple-notes".to_string()),
        "apple/ readdir should contain apple-notes, got: {:?}",
        entries
    );
    assert!(
        entries.contains(&"apple-music".to_string()),
        "apple/ readdir should contain apple-music, got: {:?}",
        entries
    );
}

// -----------------------------------------------------------------------
// 5. Hermes mode nested skill leaf readable
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_skill_md_readable() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
    });

    let md = fix.mountpoint().join("apple/apple-notes/SKILL.md");
    let content = std::fs::read_to_string(&md).expect("read nested SKILL.md");
    assert!(
        content.contains("Apple Notes skill body"),
        "nested SKILL.md should be readable with correct content"
    );
}

// -----------------------------------------------------------------------
// 6. Management path changes do not trigger notify
//    (path classification unit test — management paths produce HermesMeta
//     which mutate callbacks skip for observe_mutation)
// -----------------------------------------------------------------------

#[test]
fn hermes_path_classification_management() {
    use skillfs_fuse::path::{
        PathType, SkillLayout, is_hermes_management_path, parse_path_with_layout,
    };

    assert!(is_hermes_management_path(".hub"));
    assert!(is_hermes_management_path(".bundled_manifest"));
    assert!(is_hermes_management_path(".no-bundled-skills"));
    assert!(!is_hermes_management_path("apple"));

    let pt = parse_path_with_layout(Path::new("/.hub"), true, SkillLayout::Hermes);
    assert!(
        matches!(pt, PathType::HermesMeta { ref name } if name == ".hub"),
        "expected HermesMeta, got: {:?}",
        pt
    );

    let pt = parse_path_with_layout(Path::new("/.hub/config.json"), true, SkillLayout::Hermes);
    assert!(
        matches!(pt, PathType::HermesMetaChild { ref name, .. } if name == ".hub"),
        "expected HermesMetaChild, got: {:?}",
        pt
    );

    let pt = parse_path_with_layout(Path::new("/apple"), true, SkillLayout::Hermes);
    assert!(
        matches!(pt, PathType::CategoryDir { ref category } if category == "apple"),
        "expected CategoryDir, got: {:?}",
        pt
    );
}

// -----------------------------------------------------------------------
// 7. Nested skill source-relative path preserved
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_skill_path_preserved() {
    use skillfs_fuse::path::{PathType, SkillLayout, parse_path_with_layout};

    let pt = parse_path_with_layout(Path::new("/apple/apple-notes"), true, SkillLayout::Hermes);
    match pt {
        PathType::NestedSkillDir {
            category,
            skill_name,
        } => {
            assert_eq!(category, "apple");
            assert_eq!(skill_name, "apple-notes");
        }
        other => panic!("expected NestedSkillDir, got: {:?}", other),
    }

    let pt = parse_path_with_layout(
        Path::new("/apple/apple-notes/SKILL.md"),
        true,
        SkillLayout::Hermes,
    );
    match pt {
        PathType::NestedSkillMd {
            category,
            skill_name,
        } => {
            assert_eq!(category, "apple");
            assert_eq!(skill_name, "apple-notes");
        }
        other => panic!("expected NestedSkillMd, got: {:?}", other),
    }

    let pt = parse_path_with_layout(
        Path::new("/apple/apple-notes/scripts/run.sh"),
        true,
        SkillLayout::Hermes,
    );
    match pt {
        PathType::NestedPassthrough {
            category,
            skill_name,
            relative_path,
        } => {
            assert_eq!(category, "apple");
            assert_eq!(skill_name, "apple-notes");
            assert_eq!(relative_path, std::path::PathBuf::from("scripts/run.sh"));
        }
        other => panic!("expected NestedPassthrough, got: {:?}", other),
    }
}

// -----------------------------------------------------------------------
// 8. Management path writes do not trigger notify
// -----------------------------------------------------------------------

#[test]
fn hermes_management_path_write_no_notify() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{InMemoryNotifyClient, NotifyController};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    // Write to a management path — should NOT trigger notify.
    std::fs::write(mp.join(".hub/new-file.json"), r#"{"test": true}"#).unwrap();
    std::fs::write(mp.join(".bundled_manifest"), "updated-manifest").unwrap();

    // Wait and check no notify was produced.
    std::thread::sleep(Duration::from_millis(300));
    notify_ctrl.flush_for_testing();
    assert!(
        notify_client.is_empty(),
        "management path writes must not trigger notify, got {} events",
        notify_client.len()
    );
}

// -----------------------------------------------------------------------
// 10. Hermes activation current — nested skill is readable
// -----------------------------------------------------------------------

#[test]
fn hermes_activation_current() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{ActiveSkillResolver, ActiveTarget};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let resolver = ActiveSkillResolver::new(source.path());
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-notes"),
        },
    );

    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        active_resolver: Some(Arc::new(resolver)),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let md = mountpoint.path().join("apple/apple-notes/SKILL.md");
    let content = std::fs::read_to_string(&md).expect("read nested SKILL.md");
    assert!(
        content.contains("Apple Notes skill body"),
        "current activation must serve live source: {content}"
    );
}

// -----------------------------------------------------------------------
// 11. Hermes activation fallback — reads from snapshot
// -----------------------------------------------------------------------

#[test]
fn hermes_activation_fallback() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{ActiveSkillResolver, ActiveTarget};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let snap_dir = source
        .path()
        .join("apple/apple-notes/.skill-meta/versions/v000001.snapshot");
    std::fs::create_dir_all(&snap_dir).unwrap();
    std::fs::write(
        snap_dir.join("SKILL.md"),
        "---\nname: apple-notes\ndescription: snapshot\n---\nSnapshot body.\n",
    )
    .unwrap();

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let resolver = ActiveSkillResolver::new(source.path());
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Snapshot {
            snapshot_dir: snap_dir.clone(),
            version: "v000001.snapshot".to_string(),
        },
    );

    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        active_resolver: Some(Arc::new(resolver)),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let md = mountpoint.path().join("apple/apple-notes/SKILL.md");
    let content = std::fs::read_to_string(&md).expect("read nested SKILL.md");
    assert!(
        content.contains("Snapshot body"),
        "fallback activation must serve snapshot: {content}"
    );
}

// -----------------------------------------------------------------------
// 12. Hermes activation hidden — ENOENT on leaf, category stays visible
// -----------------------------------------------------------------------

#[test]
fn hermes_activation_hidden() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{ActiveSkillResolver, ActiveTarget};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let resolver = ActiveSkillResolver::new(source.path());
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Hidden {
            reason: "test hidden".to_string(),
        },
    );
    resolver.set(
        "apple/apple-music",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-music"),
        },
    );

    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        active_resolver: Some(Arc::new(resolver)),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    // Category dir itself must still be accessible.
    let apple = mp.join("apple");
    assert!(
        apple.is_dir(),
        "category dir must remain visible even with hidden children"
    );

    // Hidden leaf must return ENOENT.
    let notes = mp.join("apple/apple-notes");
    let err = std::fs::metadata(&notes).expect_err("hidden skill must return ENOENT");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "hidden nested skill lookup must return ENOENT, got: {err}"
    );

    // Category listing must omit hidden children.
    let entries = list_dir_names(&apple);
    assert!(
        !entries.contains(&"apple-notes".to_string()),
        "hidden skill must be omitted from category listing, got: {:?}",
        entries
    );

    // Visible child must still appear.
    assert!(
        entries.contains(&"apple-music".to_string()),
        "visible skill must appear in category listing, got: {:?}",
        entries
    );
}

// -----------------------------------------------------------------------
// 13. Hermes nested SKILL.md write triggers notify
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_write_triggers_notify() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{InMemoryNotifyClient, NotifyController};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    // Write to a nested SKILL.md.
    std::fs::write(
        mp.join("apple/apple-notes/SKILL.md"),
        "---\nname: apple-notes\ndescription: updated\n---\nUpdated.\n",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));
    notify_ctrl.flush_for_testing();

    let events = notify_client.events();
    assert!(
        !events.is_empty(),
        "nested SKILL.md write must trigger notify"
    );

    let event = &events[0];
    assert_eq!(
        event.skill_id, "apple/apple-notes",
        "skillId must be category/skill"
    );
    assert!(
        event.canonical_skill_dir.ends_with("/apple/apple-notes"),
        "canonicalSkillDir must end with /apple/apple-notes, got: {}",
        event.canonical_skill_dir
    );
    assert!(
        event.paths.contains(&"SKILL.md".to_string()),
        "paths must contain SKILL.md, got: {:?}",
        event.paths
    );
}

// -----------------------------------------------------------------------
// H3-15. Hermes nested file rename triggers notify
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_file_rename_triggers_notify() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{InMemoryNotifyClient, NotifyController};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());
    std::fs::write(source.path().join("apple/apple-notes/old.txt"), "rename-me").unwrap();

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    std::fs::rename(
        mp.join("apple/apple-notes/old.txt"),
        mp.join("apple/apple-notes/new.txt"),
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));
    notify_ctrl.flush_for_testing();

    let events = notify_client.events();
    let rename_events: Vec<_> = events
        .iter()
        .filter(|e| e.skill_id == "apple/apple-notes" && e.event_kind == "rename")
        .collect();
    assert_eq!(
        rename_events.len(),
        1,
        "nested file rename must trigger one notify for apple/apple-notes: {events:?}"
    );
    assert_eq!(rename_events[0].schema_version, 2);
    assert_eq!(rename_events[0].paths, vec!["new.txt", "old.txt"]);
}

// -----------------------------------------------------------------------
// H3-14. Management path writes produce zero notify even with
//        staging + pending install controllers attached.
// -----------------------------------------------------------------------

#[test]
fn hermes_management_no_notify_with_install_controllers() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{
        ActiveSkillResolver, ActiveTarget, InMemoryNotifyClient, InstallerStagingController,
        NotifyController, PendingInstallController, StagingConfig, StagingMatcher, StagingPattern,
    };
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().unwrap();

    let notify_client = Arc::new(InMemoryNotifyClient::new());
    let notify_ctrl = NotifyController::new(
        notify_client.clone(),
        source.path().to_path_buf(),
        Duration::from_millis(50),
        5000,
    );

    let staging_config = StagingConfig {
        patterns: vec![StagingPattern::PrefixStar(
            ".openclaw-install-stage-".to_string(),
        )],
        ..StagingConfig::default()
    };
    let matcher = Arc::new(StagingMatcher::new(staging_config));
    let staging_ctrl = InstallerStagingController::new(matcher.clone(), notify_ctrl.clone());

    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-notes"),
        },
    );

    let pending_ctrl = PendingInstallController::new(
        notify_ctrl.clone(),
        Duration::from_millis(200),
        source.path().to_path_buf(),
    );

    let config = MountConfig {
        notify_controller: Some(notify_ctrl.clone()),
        staging_matcher: Some(matcher),
        staging_controller: Some(staging_ctrl),
        active_resolver: Some(resolver),
        pending_install_controller: Some(pending_ctrl.clone()),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    std::fs::write(mp.join(".hub/new-file.json"), r#"{"test": true}"#).unwrap();
    std::fs::write(mp.join(".bundled_manifest"), "updated-manifest").unwrap();

    std::thread::sleep(Duration::from_millis(300));
    pending_ctrl.flush_for_testing();
    notify_ctrl.flush_for_testing();
    assert!(
        notify_client.is_empty(),
        "management path writes must not trigger notify even with \
         staging+pending controllers, got {} events",
        notify_client.len()
    );
}

// -----------------------------------------------------------------------
// 9. Non-skill subdirectory under category is accessible
// -----------------------------------------------------------------------

#[test]
fn hermes_non_skill_subdir_accessible() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
        let docs = dir.join("apple/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("readme.txt"), "documentation").unwrap();
        // A file living directly under the category (not in a subdir).
        std::fs::write(dir.join("apple/README.md"), "category readme").unwrap();
    });

    let docs = fix.mountpoint().join("apple/docs");
    let meta = std::fs::metadata(&docs).expect("stat apple/docs");
    assert!(meta.is_dir(), "non-skill subdir must be a directory");

    let readme = fix.mountpoint().join("apple/docs/readme.txt");
    let content = std::fs::read_to_string(&readme).expect("read readme.txt");
    assert_eq!(content, "documentation");

    // A plain file directly under the category must not be a ghost entry:
    // it must appear in the listing, stat as a file, and read back.
    let entries = list_dir_names(&fix.mountpoint().join("apple"));
    assert!(
        entries.contains(&"README.md".to_string()),
        "category direct-child file must be listed, got: {:?}",
        entries
    );
    let cat_file = fix.mountpoint().join("apple/README.md");
    let file_meta = std::fs::metadata(&cat_file).expect("stat apple/README.md");
    assert!(file_meta.is_file(), "apple/README.md must stat as a file");
    assert_eq!(
        std::fs::read_to_string(&cat_file).expect("read apple/README.md"),
        "category readme"
    );
}

// -----------------------------------------------------------------------
// 9b. Non-skill category child stays accessible even with an active
//     resolver attached (regression: non-skill children were classified
//     as nested skills and mapped to Hidden by the resolver).
// -----------------------------------------------------------------------

#[test]
fn hermes_non_skill_subdir_accessible_with_resolver() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{ActiveSkillResolver, ActiveTarget};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());
    let docs = source.path().join("apple/docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("readme.txt"), "documentation").unwrap();
    std::fs::write(source.path().join("apple/README.md"), "category readme").unwrap();

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    // Resolver knows only the real nested skills — nothing for apple/docs.
    let resolver = ActiveSkillResolver::new(source.path());
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-notes"),
        },
    );
    resolver.set(
        "apple/apple-music",
        ActiveTarget::Current {
            source_dir: source.path().join("apple/apple-music"),
        },
    );

    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        active_resolver: Some(Arc::new(resolver)),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let mp = mountpoint.path();

    // The non-skill directory and its file must remain accessible — they
    // are category passthrough, never subject to activation gating.
    let docs_meta =
        std::fs::metadata(mp.join("apple/docs")).expect("stat apple/docs with resolver");
    assert!(docs_meta.is_dir(), "apple/docs must remain a directory");

    let content = std::fs::read_to_string(mp.join("apple/docs/readme.txt"))
        .expect("read apple/docs/readme.txt with resolver");
    assert_eq!(content, "documentation");

    // A category direct-child file must also stay accessible with a
    // resolver attached (it must not be gated as a nested skill).
    let cat_file = mp.join("apple/README.md");
    let file_meta = std::fs::metadata(&cat_file).expect("stat apple/README.md with resolver");
    assert!(file_meta.is_file(), "apple/README.md must stat as a file");
    assert_eq!(
        std::fs::read_to_string(&cat_file).expect("read apple/README.md with resolver"),
        "category readme"
    );

    // Creating a NEW file inside a non-skill category subdir must succeed:
    // it is plain passthrough, not a hidden skill, so the write-path
    // hidden-write gate must not reject it.
    std::fs::write(mp.join("apple/docs/new.txt"), "created via mount")
        .expect("create apple/docs/new.txt with resolver");
    assert_eq!(
        std::fs::read_to_string(mp.join("apple/docs/new.txt")).expect("read back new.txt"),
        "created via mount"
    );

    // apple/ listing must still contain the non-skill children.
    let entries = list_dir_names(&mp.join("apple"));
    assert!(
        entries.contains(&"docs".to_string()),
        "non-skill child must remain listed under its category, got: {:?}",
        entries
    );
    assert!(
        entries.contains(&"README.md".to_string()),
        "category direct-child file must remain listed, got: {:?}",
        entries
    );
}

// -----------------------------------------------------------------------
// 14. Nested SKILL.md is compiled (directives stripped), not raw fd
//     passthrough, and its stat size matches the compiled payload.
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_skill_md_is_compiled_not_raw() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
        // Replace the nested SKILL.md with one carrying a conditional
        // directive. The compiler always strips `<!-- @if ... -->` marker
        // lines regardless of branch, so a raw passthrough read would leak
        // them while a compiled read must not.
        std::fs::write(
            dir.join("apple/apple-notes/SKILL.md"),
            "---\nname: apple-notes\ndescription: notes\n---\n\
             <!-- @if os == linux -->\nlinux-only line\n<!-- @endif -->\n\
             Apple Notes skill body.\n",
        )
        .unwrap();
    });

    let md = fix.mountpoint().join("apple/apple-notes/SKILL.md");
    let content = std::fs::read_to_string(&md).expect("read nested SKILL.md");

    assert!(
        !content.contains("<!-- @if"),
        "nested SKILL.md must be compiled (directive markers stripped), got: {content}"
    );
    assert!(
        !content.contains("@endif"),
        "compiled output must not contain directive markers, got: {content}"
    );
    assert!(
        content.contains("Apple Notes skill body"),
        "compiled body must be preserved, got: {content}"
    );

    // lookup/getattr size must match the compiled bytes served on read.
    let meta = std::fs::metadata(&md).expect("stat nested SKILL.md");
    assert_eq!(
        meta.len() as usize,
        content.len(),
        "stat size must equal compiled content length"
    );
}

// -----------------------------------------------------------------------
// 15. Nested SKILL.md served from a fallback snapshot is also compiled.
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_skill_md_snapshot_is_compiled() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{ActiveSkillResolver, ActiveTarget};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let snap_dir = source
        .path()
        .join("apple/apple-notes/.skill-meta/versions/v000001.snapshot");
    std::fs::create_dir_all(&snap_dir).unwrap();
    std::fs::write(
        snap_dir.join("SKILL.md"),
        "---\nname: apple-notes\ndescription: snapshot\n---\n\
         <!-- @if os == linux -->\nsnap-linux\n<!-- @endif -->\nSnapshot body.\n",
    )
    .unwrap();

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let resolver = ActiveSkillResolver::new(source.path());
    resolver.set(
        "apple/apple-notes",
        ActiveTarget::Snapshot {
            snapshot_dir: snap_dir.clone(),
            version: "v000001.snapshot".to_string(),
        },
    );

    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        active_resolver: Some(Arc::new(resolver)),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    let md = mountpoint.path().join("apple/apple-notes/SKILL.md");
    let content = std::fs::read_to_string(&md).expect("read nested SKILL.md");
    assert!(
        content.contains("Snapshot body"),
        "snapshot content must be served, got: {content}"
    );
    assert!(
        !content.contains("<!-- @if"),
        "snapshot nested SKILL.md must be compiled, got: {content}"
    );
    let meta = std::fs::metadata(&md).expect("stat nested SKILL.md");
    assert_eq!(
        meta.len() as usize,
        content.len(),
        "snapshot stat size must equal compiled content length"
    );
}

// -----------------------------------------------------------------------
// 16. Nested SKILL.md write is audited and attributed to category/skill.
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_write_audit_attribution() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{InMemoryEventSink, SkillEventKind, SkillEventSink};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let sink = Arc::new(InMemoryEventSink::new());
    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        event_sink: Some(sink.clone() as Arc<dyn SkillEventSink>),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    std::fs::write(
        mountpoint.path().join("apple/apple-notes/SKILL.md"),
        "---\nname: apple-notes\ndescription: updated\n---\nUpdated.\n",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(150));

    let events = sink.events();
    let attributed: Vec<_> = events
        .iter()
        .filter(|e| {
            e.skill_name.as_deref() == Some("apple/apple-notes")
                && matches!(e.kind, SkillEventKind::Open | SkillEventKind::Write)
        })
        .collect();
    assert!(
        !attributed.is_empty(),
        "nested SKILL.md write must emit audit events attributed to \
         'apple/apple-notes', got: {:?}",
        events
            .iter()
            .map(|e| (e.kind, e.skill_name.clone(), e.relative_path.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        attributed
            .iter()
            .any(|e| e.relative_path.as_deref() == Some(std::path::Path::new("SKILL.md"))),
        "audit event relative_path must be SKILL.md, got: {:?}",
        attributed
            .iter()
            .map(|e| e.relative_path.clone())
            .collect::<Vec<_>>()
    );
}

// -----------------------------------------------------------------------
// 17. Nested passthrough xattr set is audited and attributed to
//     category/skill with the correct relative path.
// -----------------------------------------------------------------------

#[test]
fn hermes_nested_xattr_audit_attribution() {
    skip_if_no_fuse!();

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use skillfs_fuse::security::{InMemoryEventSink, SkillEventKind, SkillEventSink};
    use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

    let source = tempfile::tempdir().unwrap();
    seed_hermes_workspace(source.path());
    std::fs::write(
        source.path().join("apple/apple-notes/notes.txt"),
        "note body",
    )
    .unwrap();

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let sink = Arc::new(InMemoryEventSink::new());
    let mountpoint = tempfile::tempdir().unwrap();
    let config = MountConfig {
        event_sink: Some(sink.clone() as Arc<dyn SkillEventSink>),
        skill_layout: Some(SkillLayout::Hermes),
        ..MountConfig::default()
    };

    let _handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        true,
        config,
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(300));

    // Attempt to set a user.* xattr through the mount. The FUSE side emits
    // the audit event whether the underlying filesystem accepts the xattr
    // or not, so the assertion holds regardless of tmpfs xattr support.
    let target = mountpoint.path().join("apple/apple-notes/notes.txt");
    set_user_xattr(&target, "user.skillfs.test", b"1");

    std::thread::sleep(Duration::from_millis(150));

    let events = sink.events();
    let attributed: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == SkillEventKind::Metadata
                && e.skill_name.as_deref() == Some("apple/apple-notes")
        })
        .collect();
    assert!(
        !attributed.is_empty(),
        "nested xattr set must emit a Metadata event attributed to \
         'apple/apple-notes', got: {:?}",
        events
            .iter()
            .map(|e| (e.kind, e.skill_name.clone(), e.relative_path.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        attributed
            .iter()
            .any(|e| e.relative_path.as_deref() == Some(std::path::Path::new("notes.txt"))),
        "xattr audit relative_path must be notes.txt, got: {:?}",
        attributed
            .iter()
            .map(|e| e.relative_path.clone())
            .collect::<Vec<_>>()
    );
}

// -----------------------------------------------------------------------
// 18. Mixed layout: top-level skill and nested skill coexist.
// -----------------------------------------------------------------------

#[test]
fn hermes_mixed_layout_top_level_and_nested() {
    skip_if_no_fuse!();

    let fix = MountFixture::in_place_hermes(|dir| {
        seed_hermes_workspace(dir);
        // A top-level skill living directly under the source root.
        let weather = dir.join("weather");
        std::fs::create_dir_all(&weather).unwrap();
        std::fs::write(
            weather.join("SKILL.md"),
            "---\nname: weather\ndescription: top-level\n---\n\
             <!-- @if os == linux -->\nw-linux\n<!-- @endif -->\nWeather body.\n",
        )
        .unwrap();
        std::fs::create_dir_all(weather.join("scripts")).unwrap();
        std::fs::write(weather.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    });

    let mp = fix.mountpoint();

    // Root listing exposes both the top-level skill and the category.
    let root_entries = list_dir_names(mp);
    assert!(
        root_entries.contains(&"weather".to_string()),
        "root must list top-level skill 'weather', got: {:?}",
        root_entries
    );
    assert!(
        root_entries.contains(&"apple".to_string()),
        "root must list category 'apple', got: {:?}",
        root_entries
    );

    // Top-level skill SKILL.md is compiled (behaves like a flat skill).
    let top_md = std::fs::read_to_string(mp.join("weather/SKILL.md"))
        .expect("read top-level skill SKILL.md");
    assert!(
        !top_md.contains("<!-- @if"),
        "top-level skill SKILL.md must be compiled, got: {top_md}"
    );
    assert!(top_md.contains("Weather body"));

    // Top-level skill passthrough file is readable.
    let script = std::fs::read_to_string(mp.join("weather/scripts/run.sh"))
        .expect("read top-level skill passthrough");
    assert_eq!(script, "#!/bin/sh\necho hi\n");

    // Nested skill still works alongside the top-level skill.
    let nested_md = std::fs::read_to_string(mp.join("apple/apple-notes/SKILL.md"))
        .expect("read nested SKILL.md");
    assert!(nested_md.contains("Apple Notes skill body"));
}

// -----------------------------------------------------------------------
// 19. Conservative layout auto-detection.
// -----------------------------------------------------------------------

#[test]
fn hermes_layout_auto_detection() {
    use skillfs_fuse::{SkillLayout, detect_skill_layout};

    // Bundled manifest marker => Hermes.
    let hermes_manifest = tempfile::tempdir().unwrap();
    std::fs::write(hermes_manifest.path().join(".bundled_manifest"), "x").unwrap();
    assert_eq!(
        detect_skill_layout(hermes_manifest.path()),
        SkillLayout::Hermes,
        ".bundled_manifest must select Hermes"
    );

    // .hub directory marker => Hermes.
    let hermes_hub = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(hermes_hub.path().join(".hub")).unwrap();
    assert_eq!(
        detect_skill_layout(hermes_hub.path()),
        SkillLayout::Hermes,
        ".hub/ must select Hermes"
    );

    // A bare .no-bundled-skills sentinel is NOT a strong marker => Flat.
    let flat_sentinel = tempfile::tempdir().unwrap();
    std::fs::write(flat_sentinel.path().join(".no-bundled-skills"), "").unwrap();
    create_skill_dir(flat_sentinel.path(), "my-skill");
    assert_eq!(
        detect_skill_layout(flat_sentinel.path()),
        SkillLayout::Flat,
        ".no-bundled-skills alone must NOT select Hermes"
    );

    // A plain flat workspace => Flat.
    let flat = tempfile::tempdir().unwrap();
    create_skill_dir(flat.path(), "my-skill");
    assert_eq!(
        detect_skill_layout(flat.path()),
        SkillLayout::Flat,
        "plain workspace must be Flat"
    );
}

// -----------------------------------------------------------------------
// 20. Activation enumeration matches mount discovery for mixed layouts.
// -----------------------------------------------------------------------

#[test]
fn hermes_enumerate_skill_ids_matches_mixed_layout() {
    use skillfs_fuse::security::enumerate_hermes_skill_ids;

    let dir = tempfile::tempdir().unwrap();
    seed_hermes_workspace(dir.path());
    // Top-level skill.
    std::fs::create_dir_all(dir.path().join("weather")).unwrap();
    std::fs::write(
        dir.path().join("weather/SKILL.md"),
        "---\nname: weather\n---\n",
    )
    .unwrap();
    // A subdir under the top-level skill that itself contains a SKILL.md
    // file. The mount treats this as a passthrough of the `weather` skill,
    // NOT a nested skill, so enumeration must not register `weather/scripts`.
    std::fs::create_dir_all(dir.path().join("weather/scripts")).unwrap();
    std::fs::write(dir.path().join("weather/scripts/SKILL.md"), "decoy").unwrap();
    // Non-skill category child must be excluded.
    std::fs::create_dir_all(dir.path().join("apple/docs")).unwrap();
    std::fs::write(dir.path().join("apple/docs/readme.txt"), "x").unwrap();

    let mut ids = enumerate_hermes_skill_ids(dir.path());
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "apple/apple-music".to_string(),
            "apple/apple-notes".to_string(),
            "weather".to_string(),
        ],
        "enumeration must cover top-level and nested skills, excluding non-skill \
         children and top-level skill subtrees"
    );
}

/// Set a `user.*` xattr on `path` via libc (best-effort; the test only
/// needs the FUSE callback to fire so the return value is ignored).
fn set_user_xattr(path: &Path, name: &str, value: &[u8]) {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let c_name = std::ffi::CString::new(name).unwrap();
    unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        );
    }
}
