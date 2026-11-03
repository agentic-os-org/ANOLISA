//! Sandbox contract for the task-persistence tools: task IO is anchored to
//! the mount's root_fd like every other content tool, so a symlink planted
//! at `tasks/<id>.md` must fail closed instead of reading or writing
//! outside the mount (see safe_fs's module doc for the threat model).
//! Resume/close surface `NotFound` (the symlinked entry is skipped in
//! `task_exists`), save-update surfaces `PathOutsideMount`, and list omits
//! the entry.

use std::os::unix::fs::symlink;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    // Use a sub-temp for sessions so /run/anolisa isn't required.
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Save a task, then replace its on-disk file with a symlink to an outside
/// "victim" file whose content is that same, well-formed task markdown.
/// Returns (task_id, victim_content).
///
/// The victim must parse as a task file: with plain-text victims,
/// `parse_task` rejects the bytes before anything observable happens, so
/// these tests pass on the unfixed code for a reason unrelated to the
/// sandbox. A valid victim means the sandbox refusal is the only way the
/// calls below can fail (or the only thing standing between the outside
/// file and the listing).
fn plant_symlinked_task(svc: &MemoryService, victim: &std::path::Path) -> (String, String) {
    let created = agent_memory::tools::memory_task_save(
        svc,
        "victim task",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    // The returned text embeds the id: "task saved: <id> (status=...)".
    let id = created
        .split_whitespace()
        .nth(2)
        .expect("id token")
        .to_string();
    let task_file = svc.mount.root.join("tasks").join(format!("{id}.md"));
    let victim_content = std::fs::read_to_string(&task_file).unwrap();
    std::fs::write(victim, &victim_content).unwrap();
    std::fs::remove_file(&task_file).unwrap();
    symlink(victim, &task_file).unwrap();
    (id, victim_content)
}

#[test]
fn resume_refuses_symlinked_task_file() {
    let (_tmp, svc) = setup();
    let victim_dir = tempdir().unwrap();
    let victim = victim_dir.path().join("victim.md");

    let (id, victim_content) = plant_symlinked_task(&svc, &victim);
    let result = agent_memory::tools::memory_task_resume(&svc, &id);
    assert!(
        result.is_err(),
        "resume must refuse a symlinked task file, got: {:?}",
        result.unwrap_or_default()
    );
    // The outside file is untouched — its content never reached the model.
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), victim_content);
}

#[test]
fn close_does_not_write_through_symlinked_task_file() {
    let (_tmp, svc) = setup();
    let victim_dir = tempdir().unwrap();
    let victim = victim_dir.path().join("victim.md");

    let (id, victim_content) = plant_symlinked_task(&svc, &victim);
    let result = agent_memory::tools::memory_task_close(&svc, &id, Some("done"));
    assert!(
        result.is_err(),
        "close must refuse a symlinked task file instead of writing through it"
    );
    // The victim is NOT truncated and replaced with task markdown.
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), victim_content);
}

#[test]
fn save_update_does_not_write_through_symlinked_task_file() {
    let (_tmp, svc) = setup();
    let victim_dir = tempdir().unwrap();
    let victim = victim_dir.path().join("victim.md");

    let (id, victim_content) = plant_symlinked_task(&svc, &victim);
    let result = agent_memory::tools::memory_task_save(
        &svc,
        "updated title",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&id),
    );
    assert!(
        result.is_err(),
        "save-update must refuse a symlinked task file instead of writing through it"
    );
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), victim_content);
}

#[test]
fn list_skips_symlinked_task_entries() {
    let (_tmp, svc) = setup();
    let victim_dir = tempdir().unwrap();
    let victim = victim_dir.path().join("victim.md");

    // One real task...
    agent_memory::tools::memory_task_save(
        &svc,
        "real task",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    // ...and one symlinked to an outside file.
    plant_symlinked_task(&svc, &victim);

    let listed = agent_memory::tools::memory_task_list(&svc, None).unwrap();
    assert!(
        !listed.contains("victim task"),
        "symlinked entry content must never appear in the listing"
    );
    assert!(listed.contains("real task"));
}

#[test]
fn close_refuses_traversal_task_id() {
    // save_task derives the destination from task.id, parsed off disk and
    // never re-validated: a traversal id is a write-out-of-sandbox primitive
    // that needs no symlink at all.
    let (_tmp, svc) = setup();
    let tasks = svc.mount.root.join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("planted.md"),
        "---\nid: ../../ESCAPED_MARKER\ntitle: \"pwn\"\nstatus: in-progress\nprogress: 0\ncreated_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n---\n\nbody\n",
    )
    .unwrap();
    let marker = svc.mount.root.parent().unwrap().join("ESCAPED_MARKER.md");

    let result = agent_memory::tools::memory_task_close(&svc, "planted", Some("done"));
    assert!(
        result.is_err(),
        "close must refuse a task id that traverses out of the mount, got: {:?}",
        result.as_ref().map_err(|e| e.to_string())
    );
    assert!(
        !marker.exists(),
        "nothing may be written outside mount.root, found {:?}",
        marker
    );
}

#[test]
fn save_and_resume_still_round_trip() {
    // Regression guard: the sandboxed IO must not break the happy path.
    let (_tmp, svc) = setup();
    let created = agent_memory::tools::memory_task_save(
        &svc,
        "round trip",
        None,
        None,
        None,
        None,
        None,
        None,
        Some("context body"),
        None,
    )
    .unwrap();
    let id = created
        .split_whitespace()
        .nth(2)
        .expect("id token")
        .to_string();

    let resumed = agent_memory::tools::memory_task_resume(&svc, &id).unwrap();
    assert!(resumed.contains("round trip"));
    assert!(resumed.contains("context body"));

    let closed = agent_memory::tools::memory_task_close(&svc, &id, Some("shipped")).unwrap();
    assert!(closed.contains("closed"));
}
