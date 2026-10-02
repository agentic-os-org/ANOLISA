//! Cross-platform local agent session file discovery
//!
//! Scans well-known directories on the local machine for JSONL session files
//! produced by AI coding agents (Claude Code, Qoder, QoderWork, Codex, Cursor).
//! Uses `dirs::home_dir()` for cross-platform `$HOME` resolution.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Layout type for session file storage
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Sessions stored under per-project subdirectories: <root>/<project>/<file>.jsonl
    PerProject,
    /// Sessions stored flat: <root>/<file>.jsonl
    Flat,
}

/// Describes where a particular agent stores its session files
struct SessionSource {
    agent_id: &'static str,
    agent_name: &'static str,
    icon: &'static str,
    dirs: &'static [&'static str],
    layout: Layout,
    /// Subdirectory names to recurse into (e.g. Qoder's "transcript")
    scan_subdirs: &'static [&'static str],
}

/// Well-known session storage locations for supported AI coding agents
static SESSION_SOURCES: &[SessionSource] = &[
    SessionSource {
        agent_id: "claude-code",
        agent_name: "Claude Code",
        icon: "🟣",
        dirs: &[".claude/projects"],
        layout: Layout::PerProject,
        scan_subdirs: &[],
    },
    SessionSource {
        agent_id: "qoder",
        agent_name: "Qoder",
        icon: "🔧",
        dirs: &[".qoder/projects"],
        layout: Layout::PerProject,
        scan_subdirs: &["transcript"],
    },
    SessionSource {
        agent_id: "qoderwork",
        agent_name: "QoderWork",
        icon: "🏗️",
        dirs: &[".qoderwork/projects"],
        layout: Layout::PerProject,
        scan_subdirs: &["transcript"],
    },
    SessionSource {
        agent_id: "codex",
        agent_name: "Codex",
        icon: "🟢",
        dirs: &[".codex/sessions", ".codex/archived_sessions"],
        layout: Layout::Flat,
        scan_subdirs: &[],
    },
    SessionSource {
        agent_id: "cursor",
        agent_name: "Cursor",
        icon: "⚡",
        dirs: &[".cursor/projects"],
        layout: Layout::PerProject,
        scan_subdirs: &[],
    },
];

/// A discovered local agent session file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSession {
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub agent_icon: String,
    pub project: String,
    pub message_count: u32,
    pub first_message: String,
    pub file_path: String,
    pub file_size_kb: f64,
    pub modified_ts: u64,
}

/// Response for the local sessions API endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSessionsResponse {
    pub sessions: Vec<LocalSession>,
    pub total: usize,
    pub scanned_at: u64,
}

/// Discover all local agent session files across all configured agents.
pub fn discover_local_sessions() -> Vec<LocalSession> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return vec![],
    };

    let mut sessions = Vec::new();

    for source in SESSION_SOURCES {
        for rel_dir in source.dirs {
            let dir = home.join(rel_dir);
            if !dir.exists() {
                continue;
            }
            scan_source(&dir, source, &mut sessions);
        }
    }

    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified_ts));
    sessions
}

fn scan_source(dir: &Path, source: &SessionSource, sessions: &mut Vec<LocalSession>) {
    match source.layout {
        Layout::PerProject => {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let project = decode_project_dir(&path, dir);
                        scan_project_dir(&path, source, &project, sessions);
                    }
                }
            }
        }
        Layout::Flat => {
            scan_project_dir(dir, source, "(default)", sessions);
        }
    }
}

fn scan_project_dir(
    dir: &Path,
    source: &SessionSource,
    project: &str,
    sessions: &mut Vec<LocalSession>,
) {
    // Scan top-level .jsonl files
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "jsonl")
                && let Some(session) = parse_session_file(&path, source, project)
            {
                sessions.push(session);
            }
        }
    }

    // Recurse into whitelisted subdirectories (e.g. Qoder's "transcript")
    if !source.scan_subdirs.is_empty()
        && let Ok(entries) = fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if source.scan_subdirs.contains(&dir_name) {
                    scan_project_dir(&path, source, project, sessions);
                }
            }
        }
    }
}

/// Maximum file size to fully parse for the session listing (5 MB).
/// Larger files are listed with basic metadata only (no message count).
const MAX_PARSE_SIZE_BYTES: u64 = 5 * 1024 * 1024;

/// Skip files larger than this entirely (50 MB) — too large to parse quickly.
const MAX_FILE_SIZE_BYTES: u64 = 50 * 1024 * 1024;

/// Parse a JSONL session file to extract metadata for the session list.
fn parse_session_file(path: &Path, source: &SessionSource, project: &str) -> Option<LocalSession> {
    let metadata = fs::metadata(path).ok()?;
    let file_size_kb = metadata.len() as f64 / 1024.0;
    if file_size_kb < 0.05 {
        return None;
    }

    let modified_ts = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Skip very large files — just return basic metadata
    if metadata.len() > MAX_FILE_SIZE_BYTES {
        return Some(LocalSession {
            session_id: path
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            agent_id: source.agent_id.to_string(),
            agent_name: source.agent_name.to_string(),
            agent_icon: source.icon.to_string(),
            project: project.to_string(),
            message_count: 0,
            first_message: "(file too large to preview)".to_string(),
            file_path: path.to_string_lossy().to_string(),
            file_size_kb,
            modified_ts,
        });
    }

    let content = fs::read_to_string(path).ok()?;

    let mut session_id = String::new();
    let mut message_count = 0u32;
    let mut first_message = String::new();
    let mut has_human_text = false;

    // Fast pass: count user/assistant lines without full JSON parse.
    // Only parse JSON for the first match to extract session_id and first_message.
    let mut parsed_first_user = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Fast string check before expensive JSON parse
        let is_user = trimmed.contains(r#""type":"user""#) || trimmed.contains(r#""type": "user""#);
        let is_assistant =
            trimmed.contains(r#""type":"assistant""#) || trimmed.contains(r#""type": "assistant""#);

        if !is_user && !is_assistant {
            // Still try to extract session_id from early lines
            if session_id.is_empty()
                && trimmed.contains("sessionId")
                && let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed)
                && let Some(sid) = event.get("sessionId").and_then(|v| v.as_str())
            {
                session_id = sid.to_string();
            }
            continue;
        }

        if is_user || is_assistant {
            message_count += 1;
        }

        // Parse JSON only for the first user message to extract first_message
        if is_user && !parsed_first_user {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if session_id.is_empty()
                    && let Some(sid) = event.get("sessionId").and_then(|v| v.as_str())
                {
                    session_id = sid.to_string();
                }
                // message.content can be a string (Claude Code transcripts)
                // or an array of content blocks (Qoder/QoderWork).
                if let Some(content) = event.pointer("/message/content") {
                    if let Some(text) = content.as_str() {
                        let stripped = strip_system_context(text);
                        if !stripped.is_empty() {
                            if first_message.is_empty() {
                                first_message = truncate(&stripped, 200);
                            }
                            has_human_text = true;
                        }
                    } else if let Some(content_arr) = content.as_array() {
                        for block in content_arr {
                            let block_type =
                                block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if block_type == "text" {
                                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                let stripped = strip_system_context(text);
                                if !stripped.is_empty() {
                                    if first_message.is_empty() {
                                        first_message = truncate(&stripped, 200);
                                    }
                                    has_human_text = true;
                                }
                            }
                        }
                    }
                }
                if has_human_text {
                    parsed_first_user = true;
                }
            }
        }
    }

    // For large files where no user message was found via fast scan,
    // try parsing first 50 lines fully as fallback
    if !has_human_text && metadata.len() > MAX_PARSE_SIZE_BYTES {
        for line in content.lines().take(50) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
                let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if event_type == "user" {
                    if let Some(content) = event.pointer("/message/content") {
                        if let Some(text) = content.as_str() {
                            let stripped = strip_system_context(text);
                            if !stripped.is_empty() {
                                if first_message.is_empty() {
                                    first_message = truncate(&stripped, 200);
                                }
                                has_human_text = true;
                            }
                        } else if let Some(content_arr) = content.as_array() {
                            for block in content_arr {
                                let block_type =
                                    block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                if block_type == "text" {
                                    let text =
                                        block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                    let stripped = strip_system_context(text);
                                    if !stripped.is_empty() {
                                        if first_message.is_empty() {
                                            first_message = truncate(&stripped, 200);
                                        }
                                        has_human_text = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if !has_human_text {
        return None;
    }

    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
    }

    Some(LocalSession {
        session_id,
        agent_id: source.agent_id.to_string(),
        agent_name: source.agent_name.to_string(),
        agent_icon: source.icon.to_string(),
        project: project.to_string(),
        message_count,
        first_message,
        file_path: path.to_string_lossy().to_string(),
        file_size_kb,
        modified_ts,
    })
}

/// Decode an encoded project directory name to a human-readable project name.
///
/// Qoder/Claude Code encode absolute paths into directory names by replacing
/// `/` with `-` and prefixing with `-`:
///   `-Users-john-projects-myapp` → `myapp`
///   `-data-skillopt` → `skillopt`
///   `-Users-john-projects-my-app` → `my-app`
fn decode_project_dir(dir: &Path, _root: &Path) -> String {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if !name.starts_with('-') {
        return name.to_string();
    }

    let parts: Vec<&str> = name.split('-').filter(|s| !s.is_empty()).collect();

    if parts.is_empty() {
        return name.to_string();
    }

    // Look for known parent markers and return all segments after the last match
    const MARKERS: &[&str] = &[
        "code",
        "coding",
        "dev",
        "development",
        "projects",
        "repos",
        "src",
        "work",
        "workspace",
    ];

    let mut last_match_idx: Option<usize> = None;
    for (i, part) in parts.iter().enumerate() {
        if MARKERS.contains(&part.to_lowercase().as_str()) {
            last_match_idx = Some(i);
        }
    }

    match last_match_idx {
        Some(idx) if idx + 1 < parts.len() => parts[idx + 1..].join("-"),
        _ => parts.join("-"),
    }
}

/// Strip `<system-reminder>...</system-reminder>` blocks from text.
fn strip_system_context(text: &str) -> String {
    let mut result = String::new();
    let mut in_reminder = false;
    for line in text.lines() {
        if line.contains("<system-reminder>") {
            in_reminder = true;
            continue;
        }
        if line.contains("</system-reminder>") {
            in_reminder = false;
            continue;
        }
        if !in_reminder {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result.trim().to_string()
}

/// UTF-8 safe string truncation
fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_project_dir_with_markers() {
        let path = Path::new("-Users-alice-projects-myapp");
        let root = Path::new("/home/alice/.qoder/projects");
        assert_eq!(decode_project_dir(path, root), "myapp");
    }

    #[test]
    fn test_decode_project_dir_no_marker() {
        let path = Path::new("-data-skillopt");
        let root = Path::new("/root/.qoder/projects");
        assert_eq!(decode_project_dir(path, root), "data-skillopt");
    }

    #[test]
    fn test_decode_project_dir_hyphenated_name() {
        let path = Path::new("-Users-alice-projects-my-app");
        let root = Path::new("/home/alice/.qoder/projects");
        assert_eq!(decode_project_dir(path, root), "my-app");
    }

    #[test]
    fn test_decode_project_dir_empty_parts() {
        let path = Path::new("simple");
        let root = Path::new("/root");
        assert_eq!(decode_project_dir(path, root), "simple");
    }

    #[test]
    fn test_strip_system_context_with_reminder() {
        let text = "hello\n<system-reminder>\nsecret\n</system-reminder>\nworld";
        assert_eq!(strip_system_context(text), "hello\nworld");
    }

    #[test]
    fn test_strip_system_context_without_reminder() {
        assert_eq!(strip_system_context("hello world"), "hello world");
    }

    #[test]
    fn test_strip_system_context_multiline_reminder() {
        let text = "before\n<system-reminder>\nline1\nline2\n</system-reminder>\nafter";
        assert_eq!(strip_system_context(text), "before\nafter");
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_unicode() {
        assert_eq!(truncate("你好世界测试", 4), "你好世界");
    }

    #[test]
    fn test_discover_local_sessions_with_temp_claude() {
        let home = std::env::temp_dir().join("agentsight_disc_test_home");
        let claude_dir = home.join(".claude/projects/-test-projects-demo");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let session_file = claude_dir.join("abc123.jsonl");
        std::fs::write(
            &session_file,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hello world"}]}}"#,
        )
        .unwrap();

        // SAFETY: single-threaded test, no other thread reads HOME.
        unsafe { std::env::set_var("HOME", &home) };
        let sessions = discover_local_sessions();
        assert!(sessions.iter().any(|s| s.session_id == "abc123"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn test_discover_local_sessions_with_codex_flat() {
        let home = std::env::temp_dir().join("agentsight_disc_codex_home");
        let codex_dir = home.join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let session_file = codex_dir.join("rollout-xyz.jsonl");
        std::fs::write(
            &session_file,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"codex session"}]}}"#,
        )
        .unwrap();

        // SAFETY: single-threaded test, no other thread reads HOME.
        unsafe { std::env::set_var("HOME", &home) };
        let sessions = discover_local_sessions();
        assert!(sessions.iter().any(|s| s.agent_id == "codex"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn test_parse_session_file_skips_tiny() {
        let tmp = std::env::temp_dir().join("agentsight_tiny_test.jsonl");
        std::fs::write(&tmp, "").unwrap();
        let source = &SESSION_SOURCES[0];
        let result = parse_session_file(&tmp, source, "test");
        assert!(result.is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_parse_session_file_no_user_message() {
        let tmp = std::env::temp_dir().join("agentsight_nouser_test.jsonl");
        std::fs::write(&tmp, r#"{"type":"assistant","message":{"content":"hi"}}"#).unwrap();
        let source = &SESSION_SOURCES[0];
        let result = parse_session_file(&tmp, source, "test");
        assert!(result.is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_parse_session_file_with_user_message() {
        let tmp = std::env::temp_dir().join("agentsight_user_test.jsonl");
        std::fs::write(
            &tmp,
            r#"{"type":"user","sessionId":"sess-123","message":{"content":[{"type":"text","text":"hello there"}]}}"#,
        )
        .unwrap();
        let source = &SESSION_SOURCES[0];
        let result = parse_session_file(&tmp, source, "test");
        let session = result.unwrap();
        assert_eq!(session.session_id, "sess-123");
        assert_eq!(session.first_message, "hello there");
        assert_eq!(session.message_count, 1);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_parse_session_file_strips_system_reminder() {
        let tmp = std::env::temp_dir().join("agentsight_reminder_test.jsonl");
        std::fs::write(
            &tmp,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"real question\n<system-reminder>\nignore this\n</system-reminder>"}]}}"#,
        )
        .unwrap();
        let source = &SESSION_SOURCES[0];
        let result = parse_session_file(&tmp, source, "test");
        let session = result.unwrap();
        assert!(!session.first_message.contains("system-reminder"));
        assert!(!session.first_message.contains("ignore this"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_scan_source_flat_layout() {
        let tmp = std::env::temp_dir().join("agentsight_flat_scan_test");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("session1.jsonl"),
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        let source = &SESSION_SOURCES[3]; // codex, Flat
        let mut sessions = Vec::new();
        scan_source(&tmp, source, &mut sessions);
        assert_eq!(sessions.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_source_per_project_layout() {
        let tmp = std::env::temp_dir().join("agentsight_pp_scan_test");
        let proj = tmp.join("-Users-test-projects-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("s1.jsonl"),
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        let source = &SESSION_SOURCES[0]; // claude-code, PerProject
        let mut sessions = Vec::new();
        scan_source(&tmp, source, &mut sessions);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project, "app");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_project_dir_with_transcript_subdir() {
        let tmp = std::env::temp_dir().join("agentsight_transcript_test");
        let transcript = tmp.join("transcript");
        std::fs::create_dir_all(&transcript).unwrap();
        std::fs::write(
            transcript.join("s1.jsonl"),
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        let source = &SESSION_SOURCES[1]; // qoder, scan_subdirs: ["transcript"]
        let mut sessions = Vec::new();
        scan_project_dir(&tmp, source, "test", &mut sessions);
        assert_eq!(sessions.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_local_sessions_no_home() {
        // SAFETY: single-threaded test, no other thread reads HOME.
        unsafe { std::env::set_var("HOME", "/nonexistent/path/that/does/not/exist") };
        let sessions = discover_local_sessions();
        assert!(sessions.is_empty());
    }
}
