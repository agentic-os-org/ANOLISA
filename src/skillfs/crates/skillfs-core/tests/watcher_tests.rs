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
