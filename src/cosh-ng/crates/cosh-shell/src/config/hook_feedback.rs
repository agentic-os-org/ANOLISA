use std::path::{Path, PathBuf};

use super::load::copilot_shell_cosh_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFeedbackPreference {
    pub suppression_key: String,
    pub label: String,
    pub topic: String,
    pub entity_key: String,
    pub severity: String,
    pub command_intent: String,
    pub action: String,
    pub recorded_at_ms: u64,
    pub window_ms: u64,
}

impl HookFeedbackPreference {
    pub fn minimal(suppression_key: &str, label: &str) -> Self {
        Self {
            suppression_key: suppression_key.to_string(),
            label: label.to_string(),
            topic: String::new(),
            entity_key: String::new(),
            severity: String::new(),
            command_intent: String::new(),
            action: label.to_string(),
            recorded_at_ms: 0,
            window_ms: 0,
        }
    }
}

pub fn load_hook_feedback_preferences() -> Vec<(String, String)> {
    load_hook_feedback_preference_details()
        .into_iter()
        .map(|entry| (entry.suppression_key, entry.label))
        .collect()
}

pub fn load_hook_feedback_preference_details() -> Vec<HookFeedbackPreference> {
    hook_feedback_store_path()
        .map(|path| read_hook_feedback_from_store_path(&path))
        .unwrap_or_default()
}

pub fn record_hook_feedback_key(suppression_key: &str, label: &str) -> Result<(), String> {
    record_hook_feedback_preference(HookFeedbackPreference::minimal(suppression_key, label))
}

pub fn record_hook_feedback_preference(preference: HookFeedbackPreference) -> Result<(), String> {
    let path = hook_feedback_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist hook feedback".to_string())?;
    write_hook_feedback_to_store_path(&path, preference)
}

pub fn clear_hook_feedback_store() -> Result<(), String> {
    let path = hook_feedback_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist hook feedback".to_string())?;
    write_hook_feedback_entries_to_store_path(&path, &[])
}

pub(super) fn read_hook_feedback_from_store_path(path: &Path) -> Vec<HookFeedbackPreference> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(parse_hook_feedback_line)
        .collect()
}

pub(super) fn write_hook_feedback_to_store_path(
    path: &Path,
    preference: HookFeedbackPreference,
) -> Result<(), String> {
    validate_hook_feedback_preference(&preference)?;
    let mut entries = read_hook_feedback_from_store_path(path);
    entries.retain(|entry| entry.suppression_key != preference.suppression_key);
    entries.push(preference);
    write_hook_feedback_entries_to_store_path(path, &entries)
}

pub(super) fn write_hook_feedback_entries_to_store_path(
    path: &Path,
    entries: &[HookFeedbackPreference],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create hook feedback store directory failed: {err}"))?;
    }
    let mut content = String::new();
    content.push_str(
        "# cosh-shell hook feedback; format: label<TAB>suppression_key<TAB>key=value...\n",
    );
    for entry in entries {
        if validate_hook_feedback_preference(entry).is_ok() {
            content.push_str(&entry.label);
            content.push('\t');
            content.push_str(&entry.suppression_key);
            push_hook_feedback_metadata(&mut content, "topic", &entry.topic);
            push_hook_feedback_metadata(&mut content, "entity", &entry.entity_key);
            push_hook_feedback_metadata(&mut content, "severity", &entry.severity);
            push_hook_feedback_metadata(&mut content, "intent", &entry.command_intent);
            push_hook_feedback_metadata(&mut content, "action", &entry.action);
            if entry.recorded_at_ms > 0 {
                content.push('\t');
                content.push_str("recorded_at_ms=");
                content.push_str(&entry.recorded_at_ms.to_string());
            }
            if entry.window_ms > 0 {
                content.push('\t');
                content.push_str("window_ms=");
                content.push_str(&entry.window_ms.to_string());
            }
            content.push('\n');
        }
    }
    std::fs::write(path, content).map_err(|err| format!("write hook feedback store failed: {err}"))
}

fn hook_feedback_store_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("COSH_SHELL_HOOK_FEEDBACK_STORE") {
        return Some(PathBuf::from(path));
    }
    copilot_shell_cosh_dir().map(|d| hook_feedback_store_path_in_dir(&d))
}

pub(super) fn hook_feedback_store_path_in_dir(cosh_dir: &Path) -> PathBuf {
    cosh_dir.join("hook-feedback")
}

fn validate_hook_feedback_preference(preference: &HookFeedbackPreference) -> Result<(), String> {
    if !is_valid_hook_feedback_key(&preference.suppression_key) {
        return Err("invalid hook feedback policy key".to_string());
    }
    if !is_valid_hook_feedback_label(&preference.label) {
        return Err(format!("invalid hook feedback label: {}", preference.label));
    }
    for value in [
        preference.topic.as_str(),
        preference.entity_key.as_str(),
        preference.severity.as_str(),
        preference.command_intent.as_str(),
        preference.action.as_str(),
    ] {
        if !is_valid_hook_feedback_metadata_value(value) {
            return Err("invalid hook feedback metadata value".to_string());
        }
    }
    Ok(())
}

fn parse_hook_feedback_line(line: &str) -> Option<HookFeedbackPreference> {
    let mut parts = line.split('\t');
    let label = parts.next()?.trim();
    let suppression_key = parts.next()?.trim();
    if !is_valid_hook_feedback_label(label) || !is_valid_hook_feedback_key(suppression_key) {
        return None;
    }
    let mut preference = HookFeedbackPreference::minimal(suppression_key, label);
    for part in parts {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if !is_valid_hook_feedback_metadata_value(value) {
            continue;
        }
        match key.trim() {
            "topic" => preference.topic = value.to_string(),
            "entity" => preference.entity_key = value.to_string(),
            "severity" => preference.severity = value.to_string(),
            "intent" => preference.command_intent = value.to_string(),
            "action" => preference.action = value.to_string(),
            "recorded_at_ms" => {
                preference.recorded_at_ms = value.parse::<u64>().unwrap_or(0);
            }
            "window_ms" => {
                preference.window_ms = value.parse::<u64>().unwrap_or(0);
            }
            _ => {}
        }
    }
    Some(preference)
}

fn push_hook_feedback_metadata(content: &mut String, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    content.push('\t');
    content.push_str(key);
    content.push('=');
    content.push_str(value);
}

fn is_valid_hook_feedback_key(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, '\n' | '\r' | '\t' | '\0'))
}

fn is_valid_hook_feedback_label(value: &str) -> bool {
    matches!(value, "noisy" | "useful")
}

fn is_valid_hook_feedback_metadata_value(value: &str) -> bool {
    !value
        .chars()
        .any(|ch| matches!(ch, '\n' | '\r' | '\t' | '\0'))
}
