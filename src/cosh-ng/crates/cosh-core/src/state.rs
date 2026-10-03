//! Unified component state management.
//!
//! Manages legacy component disabled sets for hooks and skills. Extension state
//! uses the versioned store in `crate::extension::state`.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::skill::COPILOT_CONFIG_DIR;

#[cfg(test)]
pub(crate) static TEST_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sub-directory under `~/.copilot-shell/` containing state files.
const STATES_DIR: &str = "states";

/// State file shared with the versioned extension state owner.
pub const EXTENSIONS_STATE: &str = "extensions.json";
/// State file for hook enable/disable status.
pub const HOOKS_STATE: &str = "hooks.json";
/// State file for skill enable/disable status.
pub const SKILLS_STATE: &str = "skills.json";
/// State file for configured MCP servers disabled by the user.
pub const MCP_SERVERS_STATE: &str = "mcp-servers.json";

/// Unified state file schema: `{ "disabled": ["name1", "name2"] }`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ComponentState {
    #[serde(default)]
    disabled: Vec<String>,
}

/// Returns the states directory path: `~/.copilot-shell/states/`.
/// Can be overridden via the `COSH_STATES_DIR` environment variable (for testing).
pub fn states_dir() -> Option<PathBuf> {
    if let Ok(override_dir) = std::env::var("COSH_STATES_DIR") {
        return Some(PathBuf::from(override_dir));
    }
    dirs::home_dir().map(|h| h.join(COPILOT_CONFIG_DIR).join(STATES_DIR))
}

/// Load the disabled set from a state file.
/// Returns an empty set if the file does not exist or cannot be parsed.
pub fn load_disabled(filename: &str) -> HashSet<String> {
    let Some(dir) = states_dir() else {
        return HashSet::new();
    };
    let path = dir.join(filename);
    match fs::read_to_string(&path) {
        Ok(content) => {
            let state: ComponentState = serde_json::from_str(&content).unwrap_or_default();
            state.disabled.into_iter().collect()
        }
        Err(_) => HashSet::new(),
    }
}

/// Atomically save the disabled set to a state file.
/// Creates the states directory if it does not exist.
pub fn save_disabled(filename: &str, disabled: &HashSet<String>) -> Result<(), String> {
    let dir = states_dir().ok_or_else(|| "cannot determine home directory".to_string())?;

    // Ensure directory exists
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create states dir: {e}"))?;

    let mut sorted: Vec<&String> = disabled.iter().collect();
    sorted.sort();
    let state = ComponentState {
        disabled: sorted.into_iter().cloned().collect(),
    };
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("failed to serialize state: {e}"))?;

    // Atomic write: write to temp file, then rename
    let target = dir.join(filename);
    let tmp = dir.join(format!(".{filename}.tmp"));
    fs::write(&tmp, json.as_bytes()).map_err(|e| format!("failed to write temp file: {e}"))?;
    fs::rename(&tmp, &target).map_err(|e| format!("failed to rename temp file: {e}"))?;

    Ok(())
}

/// Add a name to the disabled set of a state file.
pub fn add_disabled(filename: &str, name: &str) -> Result<(), String> {
    let mut disabled = load_disabled(filename);
    disabled.insert(name.to_string());
    save_disabled(filename, &disabled)
}

/// Remove a name from the disabled set of a state file.
pub fn remove_disabled(filename: &str, name: &str) -> Result<(), String> {
    let mut disabled = load_disabled(filename);
    disabled.remove(name);
    save_disabled(filename, &disabled)
}

/// Remove a set of names from the disabled set of a state file.
pub fn remove_disabled_set(filename: &str, names: &HashSet<String>) -> Result<(), String> {
    let mut disabled = load_disabled(filename);
    for name in names {
        disabled.remove(name);
    }
    save_disabled(filename, &disabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: set COSH_STATES_DIR to a unique temp directory for this test.
    fn isolated_states_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("COSH_STATES_DIR", tmp.path());
        tmp
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated_states_dir();
        let result = load_disabled("nonexistent.json");
        assert!(result.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated_states_dir();
        let mut disabled = HashSet::new();
        disabled.insert("foo".to_string());
        disabled.insert("bar".to_string());

        save_disabled("test.json", &disabled).unwrap();
        let loaded = load_disabled("test.json");
        assert_eq!(loaded, disabled);
    }

    #[test]
    fn add_and_remove() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated_states_dir();
        add_disabled("test.json", "alpha").unwrap();
        add_disabled("test.json", "beta").unwrap();

        let loaded = load_disabled("test.json");
        assert!(loaded.contains("alpha"));
        assert!(loaded.contains("beta"));

        remove_disabled("test.json", "alpha").unwrap();
        let loaded = load_disabled("test.json");
        assert!(!loaded.contains("alpha"));
        assert!(loaded.contains("beta"));
    }

    #[test]
    fn remove_disabled_set_removes_multiple() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated_states_dir();
        add_disabled("test.json", "a").unwrap();
        add_disabled("test.json", "b").unwrap();
        add_disabled("test.json", "c").unwrap();

        let to_remove: HashSet<String> = ["a".to_string(), "c".to_string()].into_iter().collect();
        remove_disabled_set("test.json", &to_remove).unwrap();

        let loaded = load_disabled("test.json");
        assert!(!loaded.contains("a"));
        assert!(loaded.contains("b"));
        assert!(!loaded.contains("c"));
    }

    #[test]
    fn idempotent_add() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated_states_dir();
        add_disabled("test.json", "x").unwrap();
        add_disabled("test.json", "x").unwrap();

        let loaded = load_disabled("test.json");
        assert_eq!(loaded.len(), 1);
    }
}
