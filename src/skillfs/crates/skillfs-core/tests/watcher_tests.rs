//! Integration tests for the watcher module.
//!
//! Note: These tests may be flaky in CI environments due to filesystem
//! event timing. They are marked with #[ignore] and can be run manually.

use std::time::Duration;

use tempfile::tempdir;

use skillfs_core::watcher::{SkillEvent, watch_source, watch_source_with_handle};

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_directory_deletion() {
    let source_dir = tempdir().expect("source directory");
    let source = source_dir.path().to_path_buf();
    let skill_dir = source.join("to-delete");
    std::fs::create_dir(&skill_dir).expect("skill directory");

    let (mut rx, handle) = watch_source_with_handle(source, 50)
        .await
        .expect("watcher must be attached before deleting the directory");
    std::fs::remove_dir(&skill_dir).expect("remove skill directory");

    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
    handle.shutdown().await;
    assert!(
        matches!(event, Ok(Some(SkillEvent::DirDeleted(path))) if path == skill_dir),
        "directory deletion must emit a directory-level event"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_new_skill() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Start watching
    let mut rx = watch_source(source.clone(), 100)
        .await
        .expect("should start watcher");

    // Create a new skill directory and file
    tokio::time::sleep(Duration::from_millis(100)).await;
    let skill_dir = source.join("new-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: new-skill\n---\n").unwrap();

    // Wait for event
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;

    assert!(event.is_ok(), "should receive event within timeout");
    let event = event.unwrap();
    assert!(event.is_some(), "should receive Some(event)");

    match event.unwrap() {
        SkillEvent::Created(path) | SkillEvent::Modified(path) => {
            assert!(path.to_string_lossy().contains("new-skill"));
        }
        _ => panic!("expected Created or Modified event"),
    }
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_modified_skill() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Pre-create a skill
    let skill_dir = source.join("existing-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: existing\n---\n").unwrap();

    // Start watching
    let mut rx = watch_source(source.clone(), 100)
        .await
        .expect("should start watcher");

    // Modify the skill file
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: existing\ndescription: updated\n---\n",
    )
    .unwrap();

    // Wait for event
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;

    assert!(event.is_ok());
    let event = event.unwrap();
    assert!(event.is_some());

    match event.unwrap() {
        SkillEvent::Modified(path) => {
            assert!(path.to_string_lossy().contains("existing-skill"));
        }
        _ => panic!("expected Modified event"),
    }
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_deleted_skill() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Pre-create a skill
    let skill_dir = source.join("to-delete");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: to-delete\n---\n").unwrap();

    // Start watching
    let mut rx = watch_source(source.clone(), 100)
        .await
        .expect("should start watcher");

    // Delete the skill file
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::remove_file(skill_dir.join("SKILL.md")).unwrap();

    // Wait for event
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;

    assert!(event.is_ok());
    let event = event.unwrap();
    assert!(event.is_some());

    match event.unwrap() {
        SkillEvent::Deleted(path) => {
            assert!(path.to_string_lossy().contains("to-delete"));
        }
        _ => panic!("expected Deleted event"),
    }
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_ignores_non_skill_files() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Start watching
    let mut rx = watch_source(source.clone(), 100)
        .await
        .expect("should start watcher");

    // Create a non-SKILL.md file
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(source.join("README.md"), "# Readme").unwrap();

    // Should not receive any event (or at least not a skill event)
    let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;

    // Either timeout (no event) or event is for a directory change
    if let Ok(Some(event)) = result {
        // If we get an event, it shouldn't be for README.md
        let path_str = match &event {
            SkillEvent::Created(p) | SkillEvent::Modified(p) | SkillEvent::Deleted(p) => {
                p.to_string_lossy()
            }
            SkillEvent::DirCreated(p) | SkillEvent::DirDeleted(p) => p.to_string_lossy(),
        };
        assert!(
            !path_str.contains("README.md"),
            "should not emit events for README.md"
        );
    }
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_directory_creation() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Start watching
    let mut rx = watch_source(source.clone(), 100)
        .await
        .expect("should start watcher");

    // Create a new directory
    tokio::time::sleep(Duration::from_millis(100)).await;
    let new_dir = source.join("new-dir");
    std::fs::create_dir(&new_dir).unwrap();

    // Wait for event
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;

    assert!(event.is_ok());
    let event = event.unwrap();
    assert!(event.is_some());

    match event.unwrap() {
        SkillEvent::DirCreated(path) => {
            assert!(path.to_string_lossy().contains("new-dir"));
        }
        _ => {} // Other events are ok too
    }
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_directory_rename_within_source() {
    let source_dir = tempdir().expect("source directory");
    let source = source_dir.path().to_path_buf();
    let old_dir = source.join("alpha");
    let new_dir = source.join("beta");
    std::fs::create_dir(&old_dir).expect("skill directory");
    std::fs::write(old_dir.join("SKILL.md"), "---\nname: alpha\n---\n").expect("manifest");

    let (mut rx, handle) = watch_source_with_handle(source, 50)
        .await
        .expect("watcher must be attached before renaming the directory");
    std::fs::rename(&old_dir, &new_dir).expect("rename skill directory");

    // A rename reports both sides: DirDeleted for the old name and
    // DirCreated for the new one. Child events (SKILL.md) may also arrive;
    // collect until both directory-level events are seen or time runs out.
    let mut saw_deleted = false;
    let mut saw_created = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && !(saw_deleted && saw_created) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirDeleted(path))) => saw_deleted |= path == old_dir,
            Ok(Some(SkillEvent::DirCreated(path))) => saw_created |= path == new_dir,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        saw_deleted,
        "renaming a skill directory must emit DirDeleted for the old name"
    );
    assert!(
        saw_created,
        "renaming a skill directory must emit DirCreated for the new name"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_directory_moved_in() {
    let parent = tempdir().expect("parent directory");
    let source = parent.path().join("source");
    std::fs::create_dir(&source).expect("source directory");
    // A skill directory created OUTSIDE the watched tree, then moved in.
    let staging = parent.path().join("staging");
    std::fs::create_dir(&staging).expect("staging directory");
    let moved_in = staging.join("arriving-skill");
    std::fs::create_dir(&moved_in).expect("staged skill directory");
    std::fs::write(moved_in.join("SKILL.md"), "---\nname: arriving\n---\n").expect("manifest");

    let (mut rx, handle) = watch_source_with_handle(source.clone(), 50)
        .await
        .expect("watcher must be attached before moving the directory in");
    std::fs::rename(&moved_in, source.join("arriving-skill"))
        .expect("move skill directory into the source");

    let expected = source.join("arriving-skill");
    let mut saw_created = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && !saw_created {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirCreated(path))) => saw_created |= path == expected,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        saw_created,
        "a directory moved into the source must emit DirCreated"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_detects_directory_moved_out() {
    let parent = tempdir().expect("parent directory");
    let source = parent.path().join("source");
    std::fs::create_dir(&source).expect("source directory");
    let leaving = source.join("leaving-skill");
    std::fs::create_dir(&leaving).expect("skill directory");
    std::fs::write(leaving.join("SKILL.md"), "---\nname: leaving\n---\n").expect("manifest");

    let (mut rx, handle) = watch_source_with_handle(source.clone(), 50)
        .await
        .expect("watcher must be attached before moving the directory out");
    std::fs::rename(&leaving, parent.path().join("gone-skill"))
        .expect("move skill directory out of the source");

    let mut saw_deleted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && !saw_deleted {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirDeleted(path))) => saw_deleted |= path == leaving,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        saw_deleted,
        "a directory moved out of the source must emit DirDeleted"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
#[cfg(unix)]
async fn test_watcher_symlink_rename_is_not_a_dir_deletion() {
    // Review regression, real inotify: renaming a symlink that points at
    // a real directory must not emit DirDeleted — the drift conversion
    // maps DirDeleted straight into a skill-scoped deletion, and no skill
    // directory ever moved. Pins the no-follow type contract of the
    // paired-rename correlation (`paths[1].is_dir()` used to follow the
    // symlink target and record both sides as directories).
    let parent = tempdir().expect("parent directory");
    let source = parent.path().join("source");
    std::fs::create_dir(&source).expect("source directory");
    let real_target = parent.path().join("real-target");
    std::fs::create_dir(&real_target).expect("real directory target");
    let link = source.join("link-a");
    std::os::unix::fs::symlink("../real-target", &link).expect("symlink to a directory");

    let (mut rx, handle) = watch_source_with_handle(source.clone(), 50)
        .await
        .expect("watcher must be attached before renaming the symlink");
    std::fs::rename(&link, source.join("link-b")).expect("rename the symlink");

    let mut saw_dir_deleted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirDeleted(path))) => {
                saw_dir_deleted = true;
                eprintln!("unexpected DirDeleted: {}", path.display());
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        !saw_dir_deleted,
        "renaming a symlink must never be reported as a skill-directory deletion"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
#[cfg(unix)]
async fn test_watcher_symlink_moved_in_is_not_a_dir_created() {
    // Delta-audit round 9, real inotify: a symlink pointing at a real
    // directory, moved into the source, must not emit DirCreated — the
    // drift conversion would report a skill directory that never existed.
    // Pins the no-follow contract of the move-in (To) arm.
    let parent = tempdir().expect("parent directory");
    let source = parent.path().join("source");
    std::fs::create_dir(&source).expect("source directory");
    let real_target = parent.path().join("real-target");
    std::fs::create_dir(&real_target).expect("real directory target");
    let staging = parent.path().join("staging");
    std::fs::create_dir(&staging).expect("staging directory");
    let link = staging.join("linked-skill");
    std::os::unix::fs::symlink("../real-target", &link).expect("symlink to a directory");

    let (mut rx, handle) = watch_source_with_handle(source.clone(), 50)
        .await
        .expect("watcher must be attached before moving the symlink in");
    std::fs::rename(&link, source.join("linked-skill")).expect("move the symlink into the source");

    // Ordered sentinel, same watch session: a REAL directory moved in
    // right after the symlink. Requiring its DirCreated proves the
    // watcher observed this session's move events — otherwise (notify
    // dropped or delayed them, the exact flake this test's `#[ignore]`
    // names) the negative assertion below would pass vacuously without
    // ever exercising the production RenameMode::To path.
    let real_skill = staging.join("real-skill");
    std::fs::create_dir(&real_skill).expect("real skill directory");
    std::fs::rename(&real_skill, source.join("real-skill"))
        .expect("move the real directory in after the symlink");

    let linked_in_source = source.join("linked-skill");
    let real_in_source = source.join("real-skill");
    let mut saw_dir_created_for_link = false;
    let mut saw_sentinel_dir_created = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirCreated(path))) => {
                if path == linked_in_source {
                    saw_dir_created_for_link = true;
                    eprintln!("unexpected DirCreated: {}", path.display());
                } else if path == real_in_source {
                    saw_sentinel_dir_created = true;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        saw_sentinel_dir_created,
        "the sentinel real-directory move-in was never observed: this session's \
         filesystem events did not fire, so the negative assertion below would \
         prove nothing"
    );
    assert!(
        !saw_dir_created_for_link,
        "a moved-in symlink must never be reported as a skill-directory creation"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn delta9_flat_to_categorized_move_does_not_delete_a_live_skill() {
    // Delta-audit round 9, real inotify: moving a flat skill into a
    // category directory inside the same source (`mv source/my-skill
    // source/category/my-skill`) must not emit DirDeleted for the skill —
    // the manifest stays loadable at category/my-skill/SKILL.md.
    let source_dir = tempdir().expect("source directory");
    let source = source_dir.path().to_path_buf();
    let category = source.join("category");
    std::fs::create_dir(&category).expect("category directory");
    let skill = source.join("my-skill");
    std::fs::create_dir(&skill).expect("flat skill directory");
    std::fs::write(skill.join("SKILL.md"), "---\nname: my-skill\n---\n").expect("manifest");

    let (mut rx, handle) = watch_source_with_handle(source, 50)
        .await
        .expect("watcher must be attached before categorizing the skill");
    let categorized = category.join("my-skill");
    std::fs::rename(&skill, &categorized).expect("categorize the skill");
    assert!(categorized.join("SKILL.md").is_file());

    let mut saw_dir_deleted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(SkillEvent::DirDeleted(path))) => {
                saw_dir_deleted = true;
                eprintln!("unexpected DirDeleted: {}", path.display());
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown().await;
    assert!(
        !saw_dir_deleted,
        "a flat-to-categorized move must not report the live skill deleted"
    );
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_debouncing() {
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();

    // Start watching with 200ms debounce
    let mut rx = watch_source(source.clone(), 200)
        .await
        .expect("should start watcher");

    // Create a skill
    let skill_dir = source.join("debounce-test");
    std::fs::create_dir(&skill_dir).unwrap();

    // Multiple rapid modifications
    for i in 0..5 {
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: test-{i}\n---\n"),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Should receive events (possibly coalesced)
    let mut event_count = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        event_count += 1;
        if event_count >= 5 {
            break;
        }
    }

    // Should have received at least one event, but debouncing may reduce the count
    assert!(event_count >= 1, "should receive at least one event");
}

#[tokio::test]
#[ignore = "flaky in CI - filesystem events may not fire reliably"]
async fn test_watcher_ignores_skill_meta_snapshot_manifests() {
    // Reviewer live probe shape: a metadata change on
    // `<skill>/.skill-meta/versions/<v>.snapshot/SKILL.md` is
    // store-internal snapshot state, not a user-edited manifest, and
    // must not surface as a skill event.
    use std::os::unix::fs::PermissionsExt;

    let source_dir = tempdir().unwrap();
    let source = source_dir.path().to_path_buf();
    let snapshot = source
        .join("alpha")
        .join(".skill-meta")
        .join("versions")
        .join("v1.snapshot")
        .join("SKILL.md");
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    std::fs::write(&snapshot, "---\nname: alpha\n---\n").unwrap();

    let (mut rx, handle) = watch_source_with_handle(source, 50)
        .await
        .expect("watcher must be attached before touching the snapshot");

    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut perms = std::fs::metadata(&snapshot).unwrap().permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&snapshot, perms).unwrap();

    let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
    handle.shutdown().await;
    if let Ok(Some(event)) = result {
        let path_str = match &event {
            SkillEvent::Created(p) | SkillEvent::Modified(p) | SkillEvent::Deleted(p) => {
                p.to_string_lossy()
            }
            SkillEvent::DirCreated(p) | SkillEvent::DirDeleted(p) => p.to_string_lossy(),
        };
        assert!(
            !path_str.contains(".skill-meta"),
            ".skill-meta snapshot writes must not emit skill events, got {event:?}"
        );
    }
}
