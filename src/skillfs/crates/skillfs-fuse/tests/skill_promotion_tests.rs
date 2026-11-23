//! Directory promotion keeps the live skill view consistent with discovery.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    InMemoryNotifyClient, NotifyController, StagingConfig, StagingMatcher, StagingPattern,
};
use skillfs_fuse::{MountConfig, MountOptions, SkillLayout, mount_background_configured};

mod common;

fn wait_for_mount(mountpoint: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mounts = std::fs::read("/proc/mounts").unwrap();
        if skillfs_fuse::proc_mounts::mounts_contain_target(
            &mounts,
            mountpoint.as_os_str().as_bytes(),
        ) {
            return;
        }
        assert!(Instant::now() < deadline, "mount did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn check_promotion(layout: SkillLayout, in_place: bool, nested: bool, malformed: bool) {
    let source = tempfile::tempdir().unwrap();
    let mountpoint = tempfile::tempdir().unwrap();
    let old = if nested {
        "apple/nested"
    } else {
        "demo/nested"
    };
    if !nested {
        common::create_skill_dir(source.path(), "demo");
    }
    common::create_skill_dir(source.path(), old);
    let manifest = if malformed {
        "---\nname: [broken\n---\npromoted body\n".to_string()
    } else {
        "---\nname: nested\ndescription: nested skill\n---\npromoted body\n".to_string()
    };
    std::fs::write(source.path().join(old).join("SKILL.md"), &manifest).unwrap();
    // A subdirectory inside demo does not own this unrelated same-leaf key.
    if !nested {
        common::create_skill_dir(&source.path().join("other"), "nested");
    }
    let mut initial = SkillStore::new();
    initial.load_from_directory(source.path(), &ParseConfig::default());
    let store: SharedSkillStore = Arc::new(RwLock::new(initial));
    let client = Arc::new(InMemoryNotifyClient::new());
    let notify = NotifyController::new(client.clone(), source.path(), Duration::from_secs(1), 5000);
    let mount_path = if in_place {
        source.path()
    } else {
        mountpoint.path()
    };
    let handle = mount_background_configured(
        mount_path,
        source.path(),
        store.clone(),
        MountOptions::default(),
        in_place,
        MountConfig {
            skill_layout: Some(layout),
            notify_controller: Some(notify.clone()),
            ..MountConfig::default()
        },
    )
    .unwrap();
    wait_for_mount(mount_path);
    let root = if in_place {
        mount_path.to_path_buf()
    } else {
        mount_path.join("skills")
    };
    let target = root.join("promoted");
    // Cache a negative lookup too: publication must be visible immediately.
    assert!(!target.join("SKILL.md").exists());
    std::fs::rename(root.join(old), &target).expect("promote directory through mount");
    assert!(common::list_dir_names(&root).contains(&"promoted".to_string()));
    let content = std::fs::read_to_string(target.join("SKILL.md"))
        .expect("promoted manifest must be readable immediately");
    assert!(content.contains("promoted body"));
    let live = store.read();
    let entry = live.get("promoted").unwrap_or_else(|| {
        panic!("promotion must publish a store entry: {layout:?}, in_place={in_place}, nested={nested}, malformed={malformed}, names={:?}", live.list())
    });
    assert_eq!(entry.source_path, source.path().join("promoted/SKILL.md"));
    assert_eq!(entry.parse_status.is_ok(), !malformed);
    assert_eq!(live.get("nested").is_some(), !nested);
    drop(live);
    if layout == SkillLayout::Flat {
        let discover = std::fs::read_to_string(root.join("skill-discover/SKILL.md")).unwrap();
        assert!(discover.contains("| promoted |"));
    }
    assert!(!root.join(old).exists());
    notify.flush_for_testing();
    assert!(
        client
            .events()
            .iter()
            .any(|event| { event.skill_id == "promoted" && event.event_kind == "rename" }),
        "promotion must notify the new skill identity"
    );
    notify.shutdown();
    handle.unmount().unwrap();
    assert_eq!(
        std::fs::read_to_string(source.path().join("promoted/SKILL.md")).unwrap(),
        manifest
    );
    assert_matches_fresh(&store, source.path());
}

fn assert_matches_fresh(store: &SharedSkillStore, source: &Path) {
    let mut fresh = SkillStore::new();
    fresh.load_from_directory(source, &ParseConfig::default());
    let live = store.read();
    assert_eq!(live.list(), fresh.list());
    for (name, expected) in fresh.iter() {
        let actual = live.get(name).unwrap();
        assert_eq!(actual.source_path, expected.source_path);
        assert_eq!(actual.parse_status, expected.parse_status);
    }
}

#[test]
fn promote_skill_subdirectory_in_flat_and_hermes_views() {
    skip_if_no_fuse!();
    for layout in [SkillLayout::Flat, SkillLayout::Hermes] {
        for in_place in [false, true] {
            for malformed in [false, true] {
                check_promotion(layout, in_place, false, malformed);
            }
        }
    }
}

#[test]
fn promote_hermes_nested_skill_to_top_level() {
    skip_if_no_fuse!();
    for in_place in [false, true] {
        for malformed in [false, true] {
            check_promotion(SkillLayout::Hermes, in_place, true, malformed);
        }
    }
}

#[test]
fn promotion_only_discovers_regular_manifests_within_the_configured_ceiling() {
    skip_if_no_fuse!();
    for layout in [SkillLayout::Flat, SkillLayout::Hermes] {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        common::create_skill_dir(source.path(), "demo");
        let demo = source.path().join("demo");
        std::fs::create_dir(demo.join("plain")).unwrap();
        std::fs::create_dir_all(demo.join("directory/SKILL.md")).unwrap();
        common::create_skill_dir(&demo, "linked");
        std::fs::rename(demo.join("linked/SKILL.md"), demo.join("linked/real.md")).unwrap();
        std::os::unix::fs::symlink("real.md", demo.join("linked/SKILL.md")).unwrap();
        common::create_skill_dir(&demo, "oversized");
        std::fs::write(demo.join("oversized/SKILL.md"), "x".repeat(2048)).unwrap();
        let config = ParseConfig {
            max_skill_size: 1024,
            ..ParseConfig::default()
        };
        let mut initial = SkillStore::new();
        initial.load_from_directory(source.path(), &config);
        let store = Arc::new(RwLock::new(initial));
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store.clone(),
            MountOptions::default(),
            false,
            MountConfig {
                skill_layout: Some(layout),
                max_skill_size: Some(1024),
                ..MountConfig::default()
            },
        )
        .unwrap();
        wait_for_mount(mountpoint.path());
        let root = mountpoint.path().join("skills");
        for name in ["plain", "directory", "linked", "oversized"] {
            std::fs::rename(root.join("demo").join(name), root.join(name)).unwrap();
            assert!(
                store.read().get(name).is_none(),
                "{name} must not be discovered"
            );
        }
        handle.unmount().unwrap();
        let mut fresh = SkillStore::new();
        fresh.load_from_directory(source.path(), &config);
        assert_eq!(store.read().list(), fresh.list());
    }
}

#[test]
fn hermes_promotion_preserves_duplicate_name_ordering() {
    skip_if_no_fuse!();
    for target in ["aaa", "zzz"] {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        common::create_skill_dir(source.path(), "demo");
        common::create_skill_dir(&source.path().join("demo"), "nested");
        common::create_skill_dir(&source.path().join("middle"), target);
        let mut initial = SkillStore::new();
        initial.load_from_directory(source.path(), &ParseConfig::default());
        let store = Arc::new(RwLock::new(initial));
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store.clone(),
            MountOptions::default(),
            false,
            MountConfig {
                skill_layout: Some(SkillLayout::Hermes),
                ..MountConfig::default()
            },
        )
        .unwrap();
        wait_for_mount(mountpoint.path());
        let root = mountpoint.path().join("skills");
        std::fs::rename(root.join("demo/nested"), root.join(target)).unwrap();
        let content = std::fs::read_to_string(root.join(target).join("SKILL.md")).unwrap();
        assert!(content.contains(if target == "aaa" {
            "name: nested"
        } else {
            "name: zzz"
        }));
        handle.unmount().unwrap();
        assert_matches_fresh(&store, source.path());
    }
}

#[test]
fn promoted_staging_directories_can_complete_installation() {
    skip_if_no_fuse!();
    for (layout, stage, target) in [
        (SkillLayout::Flat, "install-stage-1", "final-name"),
        (SkillLayout::Hermes, "install-stage-1", "final-name"),
        (
            SkillLayout::Hermes,
            "apple/install-stage-1",
            "apple/final-name",
        ),
        (
            SkillLayout::Hermes,
            "install-stage-category/helper",
            "apple/final-name",
        ),
    ] {
        for in_place in [false, true] {
            let source = tempfile::tempdir().unwrap();
            let mountpoint = tempfile::tempdir().unwrap();
            common::create_skill_dir(source.path(), "demo");
            common::create_skill_dir(&source.path().join("demo"), "helper");
            for category in ["apple", "install-stage-category"] {
                std::fs::create_dir(source.path().join(category)).unwrap();
            }
            let mut initial = SkillStore::new();
            initial.load_from_directory(source.path(), &ParseConfig::default());
            let store = Arc::new(RwLock::new(initial));
            let matcher = Arc::new(StagingMatcher::new(StagingConfig {
                patterns: vec![StagingPattern::PrefixStar("install-stage-".to_string())],
                ..StagingConfig::default()
            }));
            let mount_path = if in_place {
                source.path()
            } else {
                mountpoint.path()
            };
            let handle = mount_background_configured(
                mount_path,
                source.path(),
                store.clone(),
                MountOptions::default(),
                in_place,
                MountConfig {
                    skill_layout: Some(layout),
                    staging_matcher: Some(matcher),
                    ..MountConfig::default()
                },
            )
            .unwrap();
            wait_for_mount(mount_path);
            let root = if in_place {
                mount_path.to_path_buf()
            } else {
                mount_path.join("skills")
            };
            std::fs::rename(root.join("demo/helper"), root.join(stage)).unwrap();
            let stage_key = Path::new(stage).file_name().unwrap().to_str().unwrap();
            assert!(
                store.read().get(stage_key).is_some(),
                "staging entry must support completion: {layout:?}, {stage}, in_place={in_place}"
            );
            let stage_parent = Path::new(stage).parent().unwrap();
            let (listing_parent, hidden) = if stage.starts_with("install-stage-category/") {
                (root.clone(), "install-stage-category")
            } else {
                (root.join(stage_parent), stage_key)
            };
            assert!(
                !common::list_dir_names(&listing_parent)
                    .iter()
                    .any(|name| name == hidden)
            );
            if layout == SkillLayout::Flat {
                let discover =
                    std::fs::read_to_string(root.join("skill-discover/SKILL.md")).unwrap();
                assert!(!discover.contains("| install-stage-1 |"));
            }
            std::fs::rename(root.join(stage), root.join(target)).unwrap();
            assert!(
                std::fs::read_to_string(root.join(target).join("SKILL.md"))
                    .unwrap()
                    .contains("name: helper")
            );
            let parent = Path::new(target).parent().unwrap();
            assert!(common::list_dir_names(&root.join(parent)).contains(&"final-name".to_string()));
            assert!(store.read().get("final-name").is_some());
            assert!(store.read().get(stage_key).is_none());
            handle.unmount().unwrap();
            assert_matches_fresh(&store, source.path());
        }
    }
}

#[test]
fn promotion_mount_is_cleaned_up_during_unwind() {
    skip_if_no_fuse!();
    for in_place in [false, true] {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        common::create_skill_dir(source.path(), "demo");
        let mut initial = SkillStore::new();
        initial.load_from_directory(source.path(), &ParseConfig::default());
        let store = Arc::new(RwLock::new(initial));
        let mount_path = if in_place {
            source.path()
        } else {
            mountpoint.path()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _handle = mount_background_configured(
                mount_path,
                source.path(),
                store,
                MountOptions::default(),
                in_place,
                MountConfig::default(),
            )
            .unwrap();
            wait_for_mount(mount_path);
            panic!("simulate a failed promotion assertion");
        }));
        let panic = result.expect_err("the simulated assertion must unwind");
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"simulate a failed promotion assertion")
        );
        let mounts = std::fs::read("/proc/mounts").unwrap();
        assert!(!skillfs_fuse::proc_mounts::mounts_contain_target(
            &mounts,
            mount_path.as_os_str().as_bytes(),
        ));
        assert!(source.path().join("demo/SKILL.md").is_file());
    }
}
