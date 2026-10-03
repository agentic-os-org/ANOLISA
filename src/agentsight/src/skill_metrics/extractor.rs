//! Skill event extraction from GenAI event data.
//!
//! Extracts two kinds of skill events from stored LLM interaction records:
//! 1. **Skill Downloads**: Skills present in `<available_skills>` XML within system messages,
//!    or read directly from the system skills directory for cosh agents.
//! 2. **Skill Loads**: Tool calls that read SKILL.md files (Read/ReadFile/read_file).

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::genai::semantic::{InputMessage, MessagePart, OutputMessage};
use crate::storage::sqlite::genai::TraceEventDetail;

use super::types::{SkillDownloadRecord, SkillLoadRecord};

/// System skills directory installed by os-skills package.
const SYSTEM_SKILLS_DIR: &str = "/usr/share/anolisa/skills";

/// Agent name patterns that indicate a cosh/copilot-shell agent.
/// Used to trigger filesystem-based skill discovery.
const COSH_AGENT_PATTERNS: &[&str] = &["cosh", "copilot", "copilot-shell"];

/// Agent name patterns that indicate a QwenCode agent.
/// QwenCode does not embed `<available_skills>` in system prompts and has no
/// dedicated `skill` tool, so download discovery uses filesystem scanning of
/// per-user home directories.
const QWENCODE_AGENT_PATTERNS: &[&str] = &["qwencode", "qwen-code", "qwen_code", "qwen"];

/// Relative paths under each user home where QwenCode skills live.
const QWEN_USER_SKILL_RELDIRS: &[&str] = &[".qwen/skills", ".qwenpaw/skill_pool"];

/// Home directory roots scanned for QwenCode user-installed skills.
/// `/root` is the root user's home; entries in `/home` are individual users.
const QWEN_HOME_ROOTS: &[&str] = &["/root", "/home"];

// ─── Regex patterns (compiled once) ─────────────────────────────────────────

static RE_AVAILABLE_SKILLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<available_skills>(.*?)</available_skills>").unwrap());

static RE_SKILL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<skill>.*?<name>(.*?)</name>.*?</skill>").unwrap());

/// Regex for Hermes-style plain-text skill entries: `  - skill-name: description`
static RE_HERMES_SKILL_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*-\s+([\w-]+):.*$").unwrap());

/// Function names that indicate a file read operation (case-insensitive match).
const READ_FUNCTION_NAMES: &[&str] = &["read", "readfile", "read_file"];

/// Function names that indicate a skill invocation (case-insensitive match).
/// Used by cosh/copilot-shell ("Skill") and Hermes ("skill_view") tool_calls.
const SKILL_FUNCTION_NAMES: &[&str] = &["skill", "skill_view"];

// ─── Public API ──────────────────────────────────────────────────────────────

/// Extract skill names from the `<available_skills>` block in system messages.
///
/// For cosh/copilot-shell agents: reads skill names directly from the system
/// skills directory (`/usr/share/anolisa/skills/`) since cosh embeds
/// `<available_skills>` inside the Skill tool description, which agentsight
/// does not capture in `system_instructions`.
///
/// For other agents: parses `system_instructions` JSON (Vec<InputMessage>)
/// and applies regex to extract skill names from the XML block.
pub fn extract_skill_downloads(event: &TraceEventDetail) -> Vec<SkillDownloadRecord> {
    let session_id = event
        .trace_id
        .clone()
        .or_else(|| event.conversation_id.clone())
        .unwrap_or_default();
    let timestamp_ns = event.start_timestamp_ns;

    // For cosh / QwenCode agents, read from filesystem directly
    let agent_lower = event.agent_name.as_deref().map(|name| name.to_lowercase());

    let is_cosh = agent_lower
        .as_deref()
        .map(|lower| COSH_AGENT_PATTERNS.iter().any(|&pat| lower.contains(pat)))
        .unwrap_or(false);

    let is_qwencode = !is_cosh
        && agent_lower
            .as_deref()
            .map(|lower| {
                QWENCODE_AGENT_PATTERNS
                    .iter()
                    .any(|&pat| lower.contains(pat))
            })
            .unwrap_or(false);

    let skill_names = if is_cosh {
        scan_system_skills_dir()
    } else if is_qwencode {
        scan_qwen_user_skills_dirs()
    } else {
        let system_text = extract_system_text(event);
        if system_text.is_empty() {
            return Vec::new();
        }
        parse_available_skills(&system_text)
    };

    if skill_names.is_empty() {
        return Vec::new();
    }

    skill_names
        .into_iter()
        .map(|name| SkillDownloadRecord {
            skill_name: name,
            session_id: session_id.clone(),
            timestamp_ns,
        })
        .collect()
}

/// Extract per-skill load counts from a slice of output messages.
///
/// Looks for tool calls where function_name matches a skill invocation or a
/// file read of `/SKILL.md`. Returns a map of skill_name -> load count within
/// these messages.
pub fn extract_skill_load_counts_from_messages(messages: &[OutputMessage]) -> HashMap<String, u64> {
    let mut counts: HashMap<String, u64> = HashMap::new();

    for msg in messages {
        for part in &msg.parts {
            if let MessagePart::ToolCall {
                name, arguments, ..
            } = part
            {
                if let Some(skill_name) = detect_skill_load(name, arguments) {
                    *counts.entry(skill_name).or_insert(0) += 1;
                }
            }
        }
    }

    counts
}

/// Detect whether a single tool call represents a skill load.
///
/// Returns the skill name if the tool_call is a skill invocation (cosh/Hermes)
/// or a read of a `SKILL.md` file (Qoder); otherwise returns `None`.
fn detect_skill_load(name: &str, arguments: &Option<serde_json::Value>) -> Option<String> {
    // Pattern 1: Skill tool_call (cosh/copilot-shell)
    // e.g. {"name": "Skill", "arguments": {"skill": "pdf"}}
    if SKILL_FUNCTION_NAMES
        .iter()
        .any(|&fn_name| name.eq_ignore_ascii_case(fn_name))
    {
        return arguments.as_ref().and_then(extract_skill_name_from_args);
    }

    // Pattern 2: ReadFile tool_call reading SKILL.md (Qoder)
    // e.g. {"name": "Read", "arguments": {"file_path": "/path/to/skill/SKILL.md"}}
    if !READ_FUNCTION_NAMES
        .iter()
        .any(|&fn_name| name.eq_ignore_ascii_case(fn_name))
    {
        return None;
    }

    let file_path = arguments.as_ref().and_then(extract_file_path)?;

    if !file_path.ends_with("/SKILL.md") {
        return None;
    }

    extract_skill_name_from_path(&file_path)
}

/// Extract skill load events from tool_calls in output messages.
///
/// Looks for tool calls where function_name matches Read/ReadFile/read_file
/// and the file_path argument ends with `/SKILL.md`.
pub fn extract_skill_loads(event: &TraceEventDetail) -> Vec<SkillLoadRecord> {
    let session_id = event
        .trace_id
        .clone()
        .or_else(|| event.conversation_id.clone())
        .unwrap_or_default();
    let call_id = event.call_id.clone().unwrap_or_default();
    let timestamp_ns = event.start_timestamp_ns;
    let agent_name = event.agent_name.clone();

    // Parse output_messages JSON
    let output_messages = match &event.output_messages {
        Some(json_str) => match serde_json::from_str::<Vec<OutputMessage>>(json_str) {
            Ok(msgs) => msgs,
            Err(_) => return Vec::new(),
        },
        None => return Vec::new(),
    };

    output_messages
        .iter()
        .flat_map(|msg| msg.parts.iter())
        .filter_map(|part| {
            if let MessagePart::ToolCall {
                name, arguments, ..
            } = part
            {
                detect_skill_load(name, arguments).map(|skill_name| SkillLoadRecord {
                    skill_name,
                    session_id: session_id.clone(),
                    call_id: call_id.clone(),
                    timestamp_ns,
                    agent_name: agent_name.clone(),
                    function_name: name.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Extract all unique tool call function names from an event's output messages.
/// Used for cross-agent tool overlap calculation (Metric 10).
pub fn extract_tool_function_names(event: &TraceEventDetail) -> Vec<String> {
    let output_messages = match &event.output_messages {
        Some(json_str) => match serde_json::from_str::<Vec<OutputMessage>>(json_str) {
            Ok(msgs) => msgs,
            Err(_) => return Vec::new(),
        },
        None => return Vec::new(),
    };

    let mut names = Vec::new();
    for msg in &output_messages {
        for part in &msg.parts {
            if let MessagePart::ToolCall { name, .. } = part {
                names.push(name.clone());
            }
        }
    }
    names
}

// ─── Internal Helpers ────────────────────────────────────────────────────────

/// Extract system message text from event data.
fn extract_system_text(event: &TraceEventDetail) -> String {
    if let Some(json_str) = &event.system_instructions {
        // Try parsing as Vec<InputMessage> first
        if let Ok(msgs) = serde_json::from_str::<Vec<InputMessage>>(json_str) {
            let mut text = String::new();
            for msg in &msgs {
                if msg.role == "system" {
                    for part in &msg.parts {
                        if let MessagePart::Text { content } = part {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(content);
                        }
                    }
                }
            }
            if !text.is_empty() {
                return text;
            }
        }

        // Fallback: try as plain JSON string
        if let Ok(s) = serde_json::from_str::<String>(json_str) {
            return s;
        }
    }

    String::new()
}

/// Scan the system skills directory and return a list of skill names.
///
/// Each subdirectory of `SYSTEM_SKILLS_DIR` that contains a `SKILL.md` file
/// is considered an installed skill; its directory name is the skill name.
/// Sub-directories within those (i.e. category folders like `ai/`, `devops/`)
/// are also scanned one level deep to match the os-skills layout.
fn scan_system_skills_dir() -> Vec<String> {
    scan_skills_dir_recursive(SYSTEM_SKILLS_DIR, 2)
}

/// Recursively scan a skills directory up to `depth` levels.
fn scan_skills_dir_recursive(dir: &str, depth: u32) -> Vec<String> {
    if depth == 0 {
        return Vec::new();
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut names = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Check if this directory contains SKILL.md
        if path.join("SKILL.md").exists() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                names.push(name.to_string());
            }
        } else {
            // No SKILL.md here — it may be a category folder; recurse
            let sub = path.to_string_lossy().into_owned();
            names.extend(scan_skills_dir_recursive(&sub, depth - 1));
        }
    }
    names
}

/// Scan QwenCode skill directories under all known user homes.
///
/// QwenCode stores skills per-user, e.g.
/// `/root/.qwen/skills/<name>/SKILL.md` or
/// `/home/<user>/.qwenpaw/skill_pool/<name>/SKILL.md`. AgentSight runs as root
/// and can read all user homes.
fn scan_qwen_user_skills_dirs() -> Vec<String> {
    scan_qwen_user_skills_dirs_in(QWEN_HOME_ROOTS)
}

/// Testable variant: scan home roots provided by caller.
///
/// Each entry in `home_roots` is treated as either:
/// - a literal home directory if it directly contains one of the relative
///   skill paths (e.g. `/root` containing `/root/.qwen/skills`), or
/// - a parent directory of multiple homes (e.g. `/home` containing
///   `/home/alice`, `/home/bob`).
/// Both interpretations are tried; missing paths are silently skipped.
fn scan_qwen_user_skills_dirs_in(home_roots: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut homes_to_scan: Vec<std::path::PathBuf> = Vec::new();

    for root in home_roots {
        let root_path = std::path::Path::new(root);
        // Treat root itself as a home (e.g. /root)
        homes_to_scan.push(root_path.to_path_buf());
        // Also treat its immediate children as homes (e.g. /home/alice)
        if let Ok(entries) = std::fs::read_dir(root_path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    homes_to_scan.push(p);
                }
            }
        }
    }

    for home in &homes_to_scan {
        for reldir in QWEN_USER_SKILL_RELDIRS {
            let target = home.join(reldir);
            if !target.is_dir() {
                continue;
            }
            for found in scan_skills_dir_recursive(&target.to_string_lossy(), 2) {
                if !names.contains(&found) {
                    names.push(found);
                }
            }
        }
    }

    names
}

/// Parse `<available_skills>` block and extract skill names.
///
/// Supports two formats:
/// 1. XML (cosh): `<skill><name>foo</name>...</skill>`
/// 2. Plain-text indented (Hermes): `  - skill-name: description`
fn parse_available_skills(text: &str) -> Vec<String> {
    let mut skill_names = Vec::new();

    for block_match in RE_AVAILABLE_SKILLS.captures_iter(text) {
        let block = &block_match[1];

        // Try XML format first (cosh/generic agents)
        for name_match in RE_SKILL_NAME.captures_iter(block) {
            let name = name_match[1].trim().to_string();
            if !name.is_empty() && !skill_names.contains(&name) {
                skill_names.push(name);
            }
        }

        // If no XML skills found, try Hermes plain-text format
        if skill_names.is_empty() {
            for name_match in RE_HERMES_SKILL_ENTRY.captures_iter(block) {
                let name = name_match[1].to_string();
                if !name.is_empty() && !skill_names.contains(&name) {
                    skill_names.push(name);
                }
            }
        }
    }

    skill_names
}

/// Extract file_path from tool call arguments (JSON value).
fn extract_file_path(args: &serde_json::Value) -> Option<String> {
    // Try as object with "file_path" key
    if let Some(obj) = args.as_object() {
        if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
            return Some(fp.to_string());
        }
        // Also try "path" as a fallback key
        if let Some(fp) = obj.get("path").and_then(|v| v.as_str()) {
            return Some(fp.to_string());
        }
    }

    // Try as string (double-encoded JSON)
    if let Some(s) = args.as_str()
        && let Ok(obj) = serde_json::from_str::<serde_json::Value>(s)
    {
        return extract_file_path(&obj);
    }

    None
}

/// Extract skill name from Skill tool_call arguments.
/// Supports:
/// - Cosh: {"skill": "pdf"} or {"skill": "ms-office-suite:pdf"}
/// - Hermes: {"name": "test-driven-development"}
fn extract_skill_name_from_args(args: &serde_json::Value) -> Option<String> {
    if let Some(obj) = args.as_object() {
        // Cosh format: {"skill": "skill-name"}
        if let Some(name) = obj.get("skill").and_then(|v| v.as_str()) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        // Hermes format: {"name": "skill-name"}
        if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // Try as string (double-encoded JSON)
    if let Some(s) = args.as_str()
        && let Ok(obj) = serde_json::from_str::<serde_json::Value>(s)
    {
        return extract_skill_name_from_args(&obj);
    }

    None
}

/// Extract skill name from a file path ending in /SKILL.md.
/// The skill name is the directory immediately before SKILL.md.
fn extract_skill_name_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 2 {
        let parent = parts[parts.len() - 2];
        if !parent.is_empty() {
            return Some(parent.to_string());
        }
    }
    None
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_available_skills_basic() {
        let text = r#"Some text before
<available_skills>
<skill>
<name>ui-designer</name>
<description>Web UI design expert</description>
</skill>
<skill>
<name>create-skill</name>
<description>Create new skills</description>
</skill>
</available_skills>
Some text after"#;

        let result = parse_available_skills(text);
        assert_eq!(result, vec!["ui-designer", "create-skill"]);
    }

    #[test]
    fn test_parse_available_skills_empty() {
        let result = parse_available_skills("no skills here");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_available_skills_dedup() {
        let text = r#"<available_skills>
<skill><name>foo</name></skill>
<skill><name>foo</name></skill>
<skill><name>bar</name></skill>
</available_skills>"#;

        let result = parse_available_skills(text);
        assert_eq!(result, vec!["foo", "bar"]);
    }

    #[test]
    fn test_extract_file_path_object() {
        let args = json!({"file_path": "/path/to/my-skill/SKILL.md"});
        assert_eq!(
            extract_file_path(&args),
            Some("/path/to/my-skill/SKILL.md".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_with_path_key() {
        let args = json!({"path": "/path/to/my-skill/SKILL.md"});
        assert_eq!(
            extract_file_path(&args),
            Some("/path/to/my-skill/SKILL.md".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_double_encoded() {
        let args = json!(r#"{"file_path": "/x/y/SKILL.md"}"#);
        assert_eq!(extract_file_path(&args), Some("/x/y/SKILL.md".to_string()));
    }

    #[test]
    fn test_extract_skill_name_from_path() {
        assert_eq!(
            extract_skill_name_from_path("/root/os-skills/ai/install-copaw/SKILL.md"),
            Some("install-copaw".to_string())
        );
        assert_eq!(
            extract_skill_name_from_path("/usr/share/skills/my-cool-skill/SKILL.md"),
            Some("my-cool-skill".to_string())
        );
        assert_eq!(extract_skill_name_from_path("SKILL.md"), None);
    }

    #[test]
    fn test_extract_skill_downloads_from_event() {
        let event = TraceEventDetail {
            id: 1,
            call_id: Some("c1".into()),
            start_timestamp_ns: 1000,
            end_timestamp_ns: Some(2000),
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            system_instructions: Some(serde_json::to_string(&vec![InputMessage {
                role: "system".to_string(),
                parts: vec![MessagePart::Text {
                    content: "<available_skills><skill><name>test-skill</name><description>A test</description></skill></available_skills>".to_string(),
                }],
                name: None,
            }]).unwrap()),
            agent_name: Some("TestAgent".into()),
            process_name: None,
            pid: Some(100),
            user_query: None,
            event_json: None,
            trace_id: Some("session-1".into()),
            conversation_id: Some("conv-1".into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        };

        let downloads = extract_skill_downloads(&event);
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].skill_name, "test-skill");
        assert_eq!(downloads[0].session_id, "session-1");
    }

    /// Verify that cosh/copilot-shell agents are detected by agent_name pattern.
    ///
    /// Since cosh embeds `<available_skills>` in the Skill tool description (not
    /// in system_instructions) and agentsight doesn't capture tool definitions,
    /// skill discovery for cosh uses filesystem scanning of SYSTEM_SKILLS_DIR.
    /// This test verifies the agent detection logic and that scan_system_skills_dir
    /// returns a Vec (empty when the directory doesn't exist in test env is fine).
    #[test]
    fn test_extract_skill_downloads_cosh_agent_detected() {
        let event = TraceEventDetail {
            id: 10,
            call_id: Some("c-cosh-1".into()),
            start_timestamp_ns: 1000,
            end_timestamp_ns: Some(2000),
            model: Some("qwen-max".into()),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            system_instructions: None, // cosh doesn't put skills in system message
            agent_name: Some("Cosh".into()),
            process_name: Some("node".into()),
            pid: Some(9999),
            user_query: None,
            event_json: None,
            trace_id: Some("cosh-session-1".into()),
            conversation_id: Some("cosh-conv-1".into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        };

        // In the test environment /usr/share/anolisa/skills may not exist;
        // the key assertion is that no panic occurs and session_id is set correctly
        // if skills are found.
        let downloads = extract_skill_downloads(&event);
        for d in &downloads {
            assert_eq!(d.session_id, "cosh-session-1");
            assert!(!d.skill_name.is_empty());
        }
    }

    /// Verify scan_system_skills_dir doesn't panic on missing directory.
    #[test]
    fn test_scan_system_skills_dir_missing_dir() {
        // scan_skills_dir_recursive should return empty vec when dir doesn't exist
        let result = scan_skills_dir_recursive("/nonexistent/path/that/does/not/exist", 2);
        assert!(result.is_empty());
    }

    /// Verify scan_skills_dir_recursive correctly finds SKILL.md in subdirs.
    #[test]
    fn test_scan_skills_dir_recursive_in_tempdir() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_test_scan_skills_{}",
            std::process::id()
        ));
        // Create: tmp/ai/install-copaw/SKILL.md  and  tmp/network/SKILL.md
        fs::create_dir_all(tmp.join("ai").join("install-copaw")).unwrap();
        fs::write(
            tmp.join("ai").join("install-copaw").join("SKILL.md"),
            "---\nname: install-copaw\n---",
        )
        .unwrap();
        fs::create_dir_all(tmp.join("network")).unwrap();
        fs::write(
            tmp.join("network").join("SKILL.md"),
            "---\nname: network\n---",
        )
        .unwrap();

        let names = scan_skills_dir_recursive(&tmp.to_string_lossy(), 2);
        assert!(
            names.contains(&"install-copaw".to_string()),
            "expected install-copaw in {names:?}"
        );
        assert!(
            names.contains(&"network".to_string()),
            "expected network in {names:?}"
        );

        // cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_extract_skill_loads_from_event() {
        let output_msgs = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![
                MessagePart::Text {
                    content: "Let me read the skill".to_string(),
                },
                MessagePart::ToolCall {
                    id: Some("tc-1".into()),
                    name: "Read".to_string(),
                    arguments: Some(json!({"file_path": "/os-skills/ai/install-copaw/SKILL.md"})),
                },
                MessagePart::ToolCall {
                    id: Some("tc-2".into()),
                    name: "read_file".to_string(),
                    arguments: Some(json!({"file_path": "/other/path/README.md"})),
                },
            ],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];

        let event = TraceEventDetail {
            id: 2,
            call_id: Some("c2".into()),
            start_timestamp_ns: 5000,
            end_timestamp_ns: Some(6000),
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: Some(serde_json::to_string(&output_msgs).unwrap()),
            system_instructions: None,
            agent_name: Some("Cosh".into()),
            process_name: None,
            pid: Some(200),
            user_query: None,
            event_json: None,
            trace_id: Some("session-2".into()),
            conversation_id: Some("conv-2".into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        };

        let loads = extract_skill_loads(&event);
        assert_eq!(loads.len(), 1);
        assert_eq!(loads[0].skill_name, "install-copaw");
        assert_eq!(loads[0].function_name, "Read");
        assert_eq!(loads[0].agent_name, Some("Cosh".into()));
    }

    /// Verify QwenCode agent_name patterns are detected (case-insensitive substring).
    #[test]
    fn test_qwencode_agent_pattern_detected() {
        for name in ["QwenCode", "qwen-code", "qwen_code", "qwencode-cli", "Qwen"] {
            let lower = name.to_lowercase();
            let matched = QWENCODE_AGENT_PATTERNS.iter().any(|&p| lower.contains(p));
            assert!(matched, "expected '{name}' to match QwenCode pattern");
        }
    }

    /// Verify scan_qwen_user_skills_dirs_in returns empty Vec on missing roots.
    #[test]
    fn test_scan_qwen_user_skills_dirs_in_missing_roots() {
        let result = scan_qwen_user_skills_dirs_in(&[
            "/definitely/does/not/exist",
            "/another/missing/place",
        ]);
        assert!(result.is_empty());
    }

    /// Verify scan_qwen_user_skills_dirs_in finds skills under both .qwen/skills
    /// and .qwenpaw/skill_pool, in both literal-home and parent-of-homes layouts.
    #[test]
    fn test_scan_qwen_user_skills_dirs_in_tempdir() {
        use std::fs;
        let base = std::env::temp_dir().join(format!(
            "agentsight_qwen_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // Layout 1: literal home (mimics /root)
        let root_home = base.join("root");
        let qwen_skill_a = root_home.join(".qwen").join("skills").join("skill-a");
        let qwenpaw_b = root_home
            .join(".qwenpaw")
            .join("skill_pool")
            .join("skill-b");
        fs::create_dir_all(&qwen_skill_a).unwrap();
        fs::write(qwen_skill_a.join("SKILL.md"), "---\nname: skill-a\n---").unwrap();
        fs::create_dir_all(&qwenpaw_b).unwrap();
        fs::write(qwenpaw_b.join("SKILL.md"), "---\nname: skill-b\n---").unwrap();

        // Layout 2: parent-of-homes (mimics /home/<user>)
        let homes_root = base.join("home");
        let user_alice = homes_root.join("alice");
        let qwen_skill_c = user_alice.join(".qwen").join("skills").join("skill-c");
        fs::create_dir_all(&qwen_skill_c).unwrap();
        fs::write(qwen_skill_c.join("SKILL.md"), "---\nname: skill-c\n---").unwrap();

        let root_str = root_home.to_string_lossy().into_owned();
        let homes_str = homes_root.to_string_lossy().into_owned();
        let names = scan_qwen_user_skills_dirs_in(&[&root_str, &homes_str]);

        assert!(
            names.contains(&"skill-a".to_string()),
            "missing skill-a in {names:?}"
        );
        assert!(
            names.contains(&"skill-b".to_string()),
            "missing skill-b in {names:?}"
        );
        assert!(
            names.contains(&"skill-c".to_string()),
            "missing skill-c in {names:?}"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&base);
    }

    /// Verify QwenCode branch in extract_skill_downloads uses filesystem scan and
    /// session_id / timestamp_ns are propagated from the event.
    /// (Real QWEN_HOME_ROOTS may or may not have skills in test env; we only
    /// assert the records' session_id/timestamp invariants.)
    #[test]
    fn test_extract_skill_downloads_qwencode_branch_no_panic() {
        let event = TraceEventDetail {
            id: 20,
            call_id: Some("c-qwen-1".into()),
            start_timestamp_ns: 9000,
            end_timestamp_ns: Some(10_000),
            model: Some("qwen3-max".into()),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            // QwenCode does not embed available_skills in system prompt.
            system_instructions: None,
            agent_name: Some("QwenCode".into()),
            process_name: Some("node".into()),
            pid: Some(8888),
            user_query: None,
            event_json: None,
            trace_id: Some("qwen-session-1".into()),
            conversation_id: Some("qwen-conv-1".into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        };

        // Must not panic regardless of whether real QWEN_HOME_ROOTS exist.
        let downloads = extract_skill_downloads(&event);
        for d in &downloads {
            assert_eq!(d.session_id, "qwen-session-1");
            assert_eq!(d.timestamp_ns, 9000);
            assert!(!d.skill_name.is_empty());
        }
    }

    #[test]
    fn test_extract_skill_load_counts_from_messages() {
        let messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![
                MessagePart::ToolCall {
                    id: None,
                    name: "Skill".to_string(),
                    arguments: Some(json!({"skill": "pdf"})),
                },
                MessagePart::ToolCall {
                    id: None,
                    name: "skill".to_string(),
                    arguments: Some(json!({"skill": "pdf"})),
                },
                MessagePart::ToolCall {
                    id: None,
                    name: "read_file".to_string(),
                    arguments: Some(json!({"file_path": "/skills/read/SKILL.md"})),
                },
            ],
            name: None,
            finish_reason: None,
        }];
        let counts = extract_skill_load_counts_from_messages(&messages);
        assert_eq!(counts.get("pdf"), Some(&2));
        assert_eq!(counts.get("read"), Some(&1));
    }

    #[test]
    fn test_detect_skill_load_ignores_unrelated_tools() {
        // A tool that is neither a skill function nor a read function is ignored.
        let name = "shell";
        let arguments = Some(json!({"command": "ls"}));
        assert!(detect_skill_load(name, &arguments).is_none());
    }

    #[test]
    fn test_extract_skill_loads_returns_empty_for_invalid_json() {
        let event = TraceEventDetail {
            id: 30,
            call_id: Some("c-invalid".into()),
            start_timestamp_ns: 1000,
            end_timestamp_ns: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: Some("not valid json".into()),
            system_instructions: None,
            agent_name: None,
            process_name: None,
            pid: None,
            user_query: None,
            event_json: None,
            trace_id: None,
            conversation_id: None,
            cache_read_tokens: None,
            status: None,
            interruption_type: None,
        };
        assert!(extract_skill_loads(&event).is_empty());
    }

    #[test]
    fn test_extract_skill_loads_returns_empty_when_no_output_messages() {
        let event = TraceEventDetail {
            id: 31,
            call_id: Some("c-none".into()),
            start_timestamp_ns: 1000,
            end_timestamp_ns: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            system_instructions: None,
            agent_name: None,
            process_name: None,
            pid: None,
            user_query: None,
            event_json: None,
            trace_id: None,
            conversation_id: None,
            cache_read_tokens: None,
            status: None,
            interruption_type: None,
        };
        assert!(extract_skill_loads(&event).is_empty());
    }
}
