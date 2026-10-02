//! Task model — structured cross-session task persistence.
//!
//! Tasks are stored as `tasks/<ulid>.md` with YAML frontmatter under the
//! mount root. Each task has a title, status, progress, next steps, and
//! context (files modified, decisions made, blockers). This enables an
//! Agent to save its working context in one session and resume in another.
//!
//! Tools:
//! - `memory_task_save` — create or update a task
//! - `memory_task_resume` — load a task with full context for resuming
//! - `memory_task_list` — list tasks filtered by status
//! - `memory_task_close` — mark a task as done/cancelled

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

const TASKS_DIR: &str = "tasks";

/// Task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task is actively being worked on.
    InProgress,
    /// Task is blocked on something.
    Blocked,
    /// Task is completed.
    Done,
    /// Task was cancelled.
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::InProgress => write!(f, "in-progress"),
            TaskStatus::Blocked => write!(f, "blocked"),
            TaskStatus::Done => write!(f, "done"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// A persisted task with cross-session context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// ULID-based unique identifier.
    pub id: String,
    /// Short human-readable title.
    pub title: String,
    /// Current status.
    pub status: TaskStatus,
    /// Progress percentage 0-100.
    #[serde(default)]
    pub progress: u8,
    /// What to do next (ordered list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
    /// Blockers (why the task is stuck).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    /// Files modified during this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_modified: Vec<String>,
    /// Key decisions made during this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    /// Sessions that contributed to this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_history: Vec<String>,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
    /// Last update timestamp (RFC3339).
    pub updated_at: String,
    /// Detailed context / notes (markdown body).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
}

/// Summary of a task for list display (no context body).
#[derive(Debug, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub progress: u8,
    pub next_steps_count: usize,
    pub files_count: usize,
    pub updated_at: String,
}

impl Task {
    fn to_summary(&self) -> TaskSummary {
        TaskSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            status: self.status.to_string(),
            progress: self.progress,
            next_steps_count: self.next_steps.len(),
            files_count: self.files_modified.len(),
            updated_at: self.updated_at.clone(),
        }
    }

    fn to_markdown(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("title: {}\n", yaml_quote(&self.title)));
        out.push_str(&format!("status: {}\n", self.status));
        out.push_str(&format!("progress: {}\n", self.progress));
        if !self.next_steps.is_empty() {
            out.push_str("next_steps:\n");
            for s in &self.next_steps {
                out.push_str(&format!("  - {}\n", yaml_quote(s)));
            }
        }
        if !self.blockers.is_empty() {
            out.push_str("blockers:\n");
            for s in &self.blockers {
                out.push_str(&format!("  - {}\n", yaml_quote(s)));
            }
        }
        if !self.files_modified.is_empty() {
            out.push_str("files_modified:\n");
            for s in &self.files_modified {
                out.push_str(&format!("  - {}\n", yaml_quote(s)));
            }
        }
        if !self.decisions.is_empty() {
            out.push_str("decisions:\n");
            for s in &self.decisions {
                out.push_str(&format!("  - {}\n", yaml_quote(s)));
            }
        }
        if !self.session_history.is_empty() {
            out.push_str("session_history:\n");
            for s in &self.session_history {
                out.push_str(&format!("  - {}\n", yaml_quote(s)));
            }
        }
        out.push_str(&format!("created_at: {}\n", self.created_at));
        out.push_str(&format!("updated_at: {}\n", self.updated_at));
        out.push_str("---\n\n");
        out.push_str(&self.context);
        if !self.context.ends_with('\n') && !self.context.is_empty() {
            out.push('\n');
        }
        out
    }
}

/// Emit a YAML double-quoted scalar for safe frontmatter values.
fn yaml_quote(s: &str) -> String {
    let escaped: String = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ");
    format!("\"{escaped}\"")
}

/// Strip YAML double-quotes and unescape a frontmatter value.
fn unquote_yaml(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        s.to_string()
    }
}

/// Validate a task id before it is interpolated into `tasks/{id}.md`.
///
/// Only ASCII letters, digits, `_`, and `-` are allowed. This refuses
/// `..`, path separators (`/`, `\`), NUL, control chars, and any suffix a
/// caller could use to escape the `tasks/` directory (path traversal).
fn validate_task_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(MemoryError::InvalidArgument(
            "task id must not be empty".into(),
        ));
    }
    if id.len() > 128 {
        return Err(MemoryError::InvalidArgument(format!(
            "task id length {} exceeds 128 bytes",
            id.len()
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(MemoryError::InvalidArgument(format!(
            "task id '{id}' contains forbidden character; allowed: A-Z a-z 0-9 _ -"
        )));
    }
    Ok(())
}

/// Parse a task from markdown content with YAML frontmatter.
fn parse_task(content: &str) -> Result<Task> {
    // Split frontmatter and body.
    let (frontmatter, body) = content
        .strip_prefix("---\n")
        .and_then(|rest| {
            rest.find("\n---\n")
                .map(|pos| (&rest[..pos], &rest[pos + 5..]))
        })
        .ok_or_else(|| MemoryError::Other("invalid task file: missing frontmatter".into()))?;

    // Parse frontmatter as YAML (using serde_yaml via serde_json roundtrip).
    // We use a simple line-by-line parser for the known fields since we
    // don't want to add a yaml dependency.
    let mut task = Task {
        id: String::new(),
        title: String::new(),
        status: TaskStatus::InProgress,
        progress: 0,
        next_steps: Vec::new(),
        blockers: Vec::new(),
        files_modified: Vec::new(),
        decisions: Vec::new(),
        session_history: Vec::new(),
        created_at: String::new(),
        updated_at: String::new(),
        context: body.to_string(),
    };

    let mut current_list: Option<&mut Vec<String>> = None;
    for line in frontmatter.lines() {
        if let Some(rest) = line.strip_prefix("  - ") {
            if let Some(list) = current_list.as_mut() {
                list.push(unquote_yaml(rest.trim()));
            }
            continue;
        }
        current_list = None;

        // Try "key: value" (with space after colon) for scalar fields.
        if let Some((key, value)) = line.split_once(": ") {
            let value = unquote_yaml(value.trim());
            match key {
                "id" => task.id = value,
                "title" => task.title = value,
                "status" => {
                    task.status = match value.as_str() {
                        "in-progress" => TaskStatus::InProgress,
                        "blocked" => TaskStatus::Blocked,
                        "done" => TaskStatus::Done,
                        "cancelled" => TaskStatus::Cancelled,
                        _ => TaskStatus::InProgress,
                    };
                }
                "progress" => task.progress = value.parse().unwrap_or(0),
                "created_at" => task.created_at = value,
                "updated_at" => task.updated_at = value,
                _ => {}
            }
            continue;
        }

        // Try "key:" (no space, no value) for list fields.
        if let Some(key) = line.strip_suffix(':') {
            current_list = match key {
                "next_steps" => Some(&mut task.next_steps),
                "blockers" => Some(&mut task.blockers),
                "files_modified" => Some(&mut task.files_modified),
                "decisions" => Some(&mut task.decisions),
                "session_history" => Some(&mut task.session_history),
                _ => None,
            };
        }
    }

    Ok(task)
}

/// Load all tasks from the tasks/ directory.
fn load_all_tasks(svc: &MemoryService) -> Result<Vec<Task>> {
    let mut tasks = Vec::new();
    let tasks_path = std::path::Path::new(TASKS_DIR);

    let entries = match std::fs::read_dir(svc.mount.root.join(tasks_path)) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(tasks),
        Err(e) => return Err(e.into()),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(task) = parse_task(&content) {
            tasks.push(task);
        }
    }

    // Sort by updated_at descending (most recent first).
    tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(tasks)
}

/// Save a task to the tasks/ directory.
fn save_task(svc: &MemoryService, task: &Task) -> Result<()> {
    let dir = svc.mount.root.join(TASKS_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", task.id));
    std::fs::write(&path, task.to_markdown())?;
    Ok(())
}

// ── MCP Tool: memory_task_save ──────────────────────────────────

/// Create or update a task. If `id` is provided and a task with that id
/// exists, it is updated; otherwise a new task is created.
#[allow(clippy::too_many_arguments)]
pub fn memory_task_save(
    svc: &MemoryService,
    title: &str,
    status: Option<&str>,
    progress: Option<u8>,
    next_steps: Option<Vec<String>>,
    blockers: Option<Vec<String>>,
    files_modified: Option<Vec<String>>,
    decisions: Option<Vec<String>>,
    context: Option<&str>,
    id: Option<&str>,
) -> Result<String> {
    let now = Utc::now().to_rfc3339();

    // Validate caller-supplied id early so it can never reach the
    // filesystem as part of a path (defense against path traversal).
    if let Some(existing_id) = id {
        validate_task_id(existing_id)?;
    }

    // Try to load existing task for update. If an explicit id was given but
    // no matching task exists, create a new task with that id (the tool
    // description promises "update if exists, otherwise create new").
    let mut task = if let Some(existing_id) = id {
        let path = svc
            .mount
            .root
            .join(TASKS_DIR)
            .join(format!("{existing_id}.md"));
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            parse_task(&content)?
        } else {
            Task {
                id: existing_id.to_string(),
                title: title.to_string(),
                status: TaskStatus::InProgress,
                progress: 0,
                next_steps: Vec::new(),
                blockers: Vec::new(),
                files_modified: Vec::new(),
                decisions: Vec::new(),
                session_history: Vec::new(),
                created_at: now.clone(),
                updated_at: now.clone(),
                context: String::new(),
            }
        }
    } else {
        Task {
            id: ulid::Ulid::new().to_string(),
            title: title.to_string(),
            status: TaskStatus::InProgress,
            progress: 0,
            next_steps: Vec::new(),
            blockers: Vec::new(),
            files_modified: Vec::new(),
            decisions: Vec::new(),
            session_history: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            context: String::new(),
        }
    };

    // Apply updates.
    if !title.is_empty() {
        task.title = title.to_string();
    }
    if let Some(s) = status {
        task.status = match s {
            "in-progress" => TaskStatus::InProgress,
            "blocked" => TaskStatus::Blocked,
            "done" => TaskStatus::Done,
            "cancelled" => TaskStatus::Cancelled,
            _ => {
                return Err(MemoryError::InvalidArgument(format!(
                    "unknown status '{s}'; expected in-progress, blocked, done, or cancelled"
                )));
            }
        };
    }
    if let Some(p) = progress {
        task.progress = p.min(100);
    }
    if let Some(ns) = next_steps {
        task.next_steps = ns;
    }
    if let Some(b) = blockers {
        task.blockers = b;
    }
    if let Some(f) = files_modified {
        task.files_modified = f;
    }
    if let Some(d) = decisions {
        task.decisions = d;
    }
    if let Some(c) = context {
        task.context = c.to_string();
    }

    // Add current session to history (if available and not already present).
    if let Some(session) = &svc.session {
        let sid = session.sid().to_string();
        if !task.session_history.contains(&sid) {
            task.session_history.push(sid);
        }
    }

    task.updated_at = now;
    save_task(svc, &task)?;

    svc.audit_log(
        AuditEntry::new("memory_task_save")
            .path(format!("{}/{}", TASKS_DIR, task.id))
            .bytes(task.to_markdown().len() as u64),
    );

    Ok(format!(
        "task saved: {} (status={}, progress={}%)",
        task.id, task.status, task.progress
    ))
}

// ── MCP Tool: memory_task_resume ────────────────────────────────

/// Load a task by id and return its full context for resuming work.
pub fn memory_task_resume(svc: &MemoryService, id: &str) -> Result<String> {
    validate_task_id(id)?;
    let path = svc.mount.root.join(TASKS_DIR).join(format!("{id}.md"));
    if !path.exists() {
        return Err(MemoryError::NotFound(format!("task {id}")));
    }
    let content = std::fs::read_to_string(&path)?;
    let task = parse_task(&content)?;

    svc.audit_log(
        AuditEntry::new("memory_task_resume")
            .path(format!("{}/{}", TASKS_DIR, id))
            .bytes(content.len() as u64),
    );

    // Format as a readable resume context.
    let mut out = format!(
        "## Task: {} ({} — {}%)\n\n",
        task.title, task.status, task.progress
    );
    if !task.next_steps.is_empty() {
        out.push_str("### Next Steps\n");
        for (i, step) in task.next_steps.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, step));
        }
        out.push('\n');
    }
    if !task.blockers.is_empty() {
        out.push_str("### Blockers\n");
        for b in &task.blockers {
            out.push_str(&format!("- {}\n", b));
        }
        out.push('\n');
    }
    if !task.files_modified.is_empty() {
        out.push_str("### Files Modified\n");
        for f in &task.files_modified {
            out.push_str(&format!("- `{}`\n", f));
        }
        out.push('\n');
    }
    if !task.decisions.is_empty() {
        out.push_str("### Decisions Made\n");
        for d in &task.decisions {
            out.push_str(&format!("- {}\n", d));
        }
        out.push('\n');
    }
    if !task.context.is_empty() {
        out.push_str("### Context\n");
        out.push_str(&task.context);
        out.push('\n');
    }
    Ok(out)
}

// ── MCP Tool: memory_task_list ──────────────────────────────────

/// List tasks, optionally filtered by status.
pub fn memory_task_list(svc: &MemoryService, status_filter: Option<&str>) -> Result<String> {
    let tasks = load_all_tasks(svc)?;

    let filtered: Vec<TaskSummary> = tasks
        .iter()
        .filter(|t| {
            if let Some(filter) = status_filter {
                t.status.to_string() == filter
            } else {
                // Default: show active tasks (in-progress + blocked).
                matches!(t.status, TaskStatus::InProgress | TaskStatus::Blocked)
            }
        })
        .map(|t| t.to_summary())
        .collect();

    svc.audit_log(
        AuditEntry::new("memory_task_list")
            .path(status_filter.unwrap_or("active").to_string())
            .bytes(filtered.len() as u64),
    );

    if filtered.is_empty() {
        // Return an empty JSON array so callers parsing the documented
        // JSON-array response shape don't break on the empty case.
        return Ok("[]".to_string());
    }

    serde_json::to_string_pretty(&filtered)
        .map_err(|e| MemoryError::Other(format!("serialize task list: {e}")))
}

// ── MCP Tool: memory_task_close ─────────────────────────────────

/// Mark a task as done or cancelled.
pub fn memory_task_close(svc: &MemoryService, id: &str, reason: Option<&str>) -> Result<String> {
    validate_task_id(id)?;
    let path = svc.mount.root.join(TASKS_DIR).join(format!("{id}.md"));
    if !path.exists() {
        return Err(MemoryError::NotFound(format!("task {id}")));
    }
    let content = std::fs::read_to_string(&path)?;
    let mut task = parse_task(&content)?;

    task.status = TaskStatus::Done;
    task.progress = 100;
    task.updated_at = Utc::now().to_rfc3339();

    if let Some(r) = reason {
        if !task.context.is_empty() {
            task.context.push('\n');
        }
        task.context.push_str(&format!("**Closed**: {}\n", r));
    }

    save_task(svc, &task)?;

    svc.audit_log(
        AuditEntry::new("memory_task_close")
            .path(format!("{}/{}", TASKS_DIR, id))
            .bytes(task.to_markdown().len() as u64),
    );

    Ok(format!("task {} closed (done)", id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, MemoryService) {
        let tmp = tempdir().unwrap();
        let mut config = AppConfig::default();
        config.memory.paths.base_dir = tmp.path().to_string_lossy().to_string();
        config.memory.index.enabled = false;
        config.memory.git.enabled = false;
        config.memory.consolidation.enabled = false;
        // Use the tmp dir as session dir too.
        config.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().to_string();
        let svc = MemoryService::new(config).unwrap();
        (tmp, svc)
    }

    #[test]
    fn save_and_resume_task() {
        let (_tmp, svc) = setup();
        let result = memory_task_save(
            &svc,
            "Implement JWT auth",
            None,
            Some(50),
            Some(vec!["Write tests".into(), "Add middleware".into()]),
            None,
            Some(vec!["src/auth/jwt.rs".into()]),
            Some(vec!["Chose jose over jsonwebtoken".into()]),
            Some("JWT auth module for the API"),
            None,
        )
        .unwrap();
        assert!(result.contains("task saved"));

        // Extract task id from result.
        let id = result
            .split(": ")
            .nth(1)
            .unwrap()
            .split(" ")
            .next()
            .unwrap();

        // Resume should return formatted context.
        let resume = memory_task_resume(&svc, id).unwrap();
        assert!(resume.contains("Implement JWT auth"));
        assert!(resume.contains("Write tests"));
        assert!(resume.contains("jose over jsonwebtoken"));
    }

    #[test]
    fn list_tasks_filters_active() {
        let (_tmp, svc) = setup();
        // Create two tasks.
        memory_task_save(
            &svc, "Task A", None, None, None, None, None, None, None, None,
        )
        .unwrap();
        memory_task_save(
            &svc, "Task B", None, None, None, None, None, None, None, None,
        )
        .unwrap();

        // List active (default).
        let list = memory_task_list(&svc, None).unwrap();
        assert!(list.contains("Task A"));
        assert!(list.contains("Task B"));

        // List done (should be empty JSON array, per documented schema).
        let list_done = memory_task_list(&svc, Some("done")).unwrap();
        assert_eq!(list_done, "[]");
    }

    #[test]
    fn close_task() {
        let (_tmp, svc) = setup();
        let result = memory_task_save(
            &svc,
            "Task to close",
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
        let id = result
            .split(": ")
            .nth(1)
            .unwrap()
            .split(" ")
            .next()
            .unwrap();

        let close_result = memory_task_close(&svc, id, Some("All done")).unwrap();
        assert!(close_result.contains("closed"));

        // Task should now be done.
        let list = memory_task_list(&svc, Some("done")).unwrap();
        assert!(list.contains("Task to close"));
    }

    #[test]
    fn update_existing_task() {
        let (_tmp, svc) = setup();
        let result = memory_task_save(
            &svc,
            "Original title",
            None,
            Some(20),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let id = result
            .split(": ")
            .nth(1)
            .unwrap()
            .split(" ")
            .next()
            .unwrap();

        // Update with same id.
        let updated = memory_task_save(
            &svc,
            "Updated title",
            Some("blocked"),
            Some(40),
            None,
            Some(vec!["Waiting for review".into()]),
            None,
            None,
            None,
            Some(id),
        )
        .unwrap();
        assert!(updated.contains("task saved"));

        let resume = memory_task_resume(&svc, id).unwrap();
        assert!(resume.contains("Updated title"));
        assert!(resume.contains("blocked"));
        assert!(resume.contains("40%"));
        assert!(resume.contains("Waiting for review"));
    }

    #[test]
    fn resume_nonexistent_task() {
        let (_tmp, svc) = setup();
        let result = memory_task_resume(&svc, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn parse_task_roundtrip() {
        let task = Task {
            id: "test-id".into(),
            title: "Test Task".into(),
            status: TaskStatus::InProgress,
            progress: 75,
            next_steps: vec!["Step 1".into(), "Step 2".into()],
            blockers: vec![],
            files_modified: vec!["src/main.rs".into()],
            decisions: vec![],
            session_history: vec!["ses_123".into()],
            created_at: "2026-06-11T10:00:00Z".into(),
            updated_at: "2026-06-11T12:00:00Z".into(),
            context: "Some context here.".into(),
        };
        let md = task.to_markdown();
        let parsed = parse_task(&md).unwrap();
        assert_eq!(parsed.id, "test-id");
        assert_eq!(parsed.title, "Test Task");
        assert_eq!(parsed.progress, 75);
        assert_eq!(parsed.next_steps.len(), 2);
        assert_eq!(parsed.files_modified.len(), 1);
    }

    #[test]
    fn task_id_rejects_path_traversal() {
        let (_tmp, svc) = setup();
        // Each of these must be rejected before any filesystem path is built.
        for bad in [
            "../escape",
            "..",
            "a/b",
            "a\\b",
            "foo\x00bar",
            "sub/dir/task",
            "good..bad",
        ] {
            let err = memory_task_resume(&svc, bad).unwrap_err();
            assert!(
                matches!(err, MemoryError::InvalidArgument(_)),
                "expected InvalidArgument for id {bad:?}, got {err:?}"
            );
            let err = memory_task_close(&svc, bad, None).unwrap_err();
            assert!(
                matches!(err, MemoryError::InvalidArgument(_)),
                "expected InvalidArgument for id {bad:?}, got {err:?}"
            );
            let err = memory_task_save(
                &svc,
                "t",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(bad),
            )
            .unwrap_err();
            assert!(
                matches!(err, MemoryError::InvalidArgument(_)),
                "expected InvalidArgument for id {bad:?}, got {err:?}"
            );
        }
        // The tasks/ dir must not exist after the rejected attempts.
        assert!(
            !svc.mount.root.join(TASKS_DIR).exists(),
            "tasks/ dir should not have been created by rejected saves"
        );
    }

    #[test]
    fn task_save_creates_when_explicit_id_missing() {
        let (_tmp, svc) = setup();
        // Explicit id that does not yet exist must create, not NotFound.
        let result = memory_task_save(
            &svc,
            "Brand new task",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("custom-id-42"),
        )
        .unwrap();
        assert!(result.contains("custom-id-42"));

        let resume = memory_task_resume(&svc, "custom-id-42").unwrap();
        assert!(resume.contains("Brand new task"));
    }
}
