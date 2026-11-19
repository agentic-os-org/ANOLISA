//! Startup trust gate for config-driven stdio MCP servers (issue #9518).
//!
//! A stdio server declared only in the writable user config is code execution at
//! startup. It runs only after an explicit user trust decision pins its command,
//! args and env; any change to those bytes invalidates the pin and re-gates.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::McpServerConfig;
use crate::state::states_dir;

/// State file recording per-server startup trust fingerprints.
const TRUSTED_MCP_STATE: &str = "mcp-trusted.json";
/// Sidecar lock serializing trust read-modify-write transactions across processes.
const TRUSTED_MCP_LOCK: &str = "mcp-trusted.lock";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TrustedMcpState {
    #[serde(default)]
    trusted: BTreeMap<String, String>,
}

/// Pins the resolved executable identity of a stdio server: the command resolved
/// against the workspace root, the env-expanded args and explicit env values, plus
/// the exposed tool allowlist. Pinning the resolved target binds the file that
/// actually starts; pinning `allowed_tools` means widening the exposed tool set
/// re-gates startup too.
pub(crate) fn fingerprint(config: &McpServerConfig, workspace_root: &Path) -> String {
    let command = super::resolve_executable(&config.command, workspace_root);
    let args: Vec<String> = config
        .args
        .iter()
        .map(|argument| super::expand_env_vars(argument))
        .collect();
    let env: BTreeMap<String, String> = config
        .env
        .iter()
        .map(|(name, value)| (name.clone(), super::expand_env_vars(value)))
        .collect();
    let allowed_tools = config.allowed_tools.as_ref().map(|tools| {
        let mut sorted = tools.clone();
        sorted.sort();
        sorted
    });
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    entries.insert("command".to_string(), json!(command));
    entries.insert("args".to_string(), json!(args));
    entries.insert("env".to_string(), json!(env));
    entries.insert("allowed_tools".to_string(), json!(allowed_tools));
    // Bind the resolution/startup directory: the stdio child runs with this cwd,
    // so relative script arguments resolve against it. A different workspace
    // changes the pin and re-gates startup (issue #9518 review).
    entries.insert(
        "workspace_root".to_string(),
        json!(workspace_root.to_string_lossy()),
    );
    let bytes = serde_json::to_vec(&entries).expect("canonical trust fingerprint serializes");
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Runs a trust read-modify-write transaction under a cross-process exclusive
/// lock so concurrent trust/untrust cannot restore a revoked pin from a stale
/// snapshot (issue #9518 review).
fn with_trust_lock<T>(transaction: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let dir = states_dir().ok_or_else(|| "cannot determine home directory".to_string())?;
    fs::create_dir_all(&dir).map_err(|error| format!("failed to create states dir: {error}"))?;
    let lock_path = dir.join(TRUSTED_MCP_LOCK);
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("failed to open MCP trust lock: {error}"))?;
    lock_file
        .lock_exclusive()
        .map_err(|error| format!("failed to lock MCP trust state: {error}"))?;
    let result = transaction();
    drop(lock_file);
    result
}

/// Loads the trust store fail-closed: a missing file is an empty store, but an
/// unreadable or corrupt file is logged once and also treated as empty so no
/// server inherits trust from damaged state.
pub(crate) fn load_trusted() -> BTreeMap<String, String> {
    let Some(dir) = states_dir() else {
        return BTreeMap::new();
    };
    let path = dir.join(TRUSTED_MCP_STATE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = ?error, "MCP trust store unreadable; treating all servers as untrusted");
            return BTreeMap::new();
        }
    };
    match serde_json::from_str::<TrustedMcpState>(&content) {
        Ok(state) => state.trusted,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = ?error, "MCP trust store corrupt; treating all servers as untrusted");
            BTreeMap::new()
        }
    }
}

fn save_trusted(trusted: &BTreeMap<String, String>) -> Result<(), String> {
    let dir = states_dir().ok_or_else(|| "cannot determine home directory".to_string())?;
    fs::create_dir_all(&dir).map_err(|error| format!("failed to create states dir: {error}"))?;
    let state = TrustedMcpState {
        trusted: trusted.clone(),
    };
    let json = serde_json::to_string_pretty(&state)
        .map_err(|error| format!("failed to serialize MCP trust state: {error}"))?;
    let target = dir.join(TRUSTED_MCP_STATE);
    let tmp = dir.join(format!(".{TRUSTED_MCP_STATE}.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|error| format!("failed to create temp file: {error}"))?;
    file.write_all(json.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            let _ = fs::remove_file(&tmp);
            format!("failed to write temp file: {error}")
        })?;
    drop(file);
    fs::rename(&tmp, &target).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("failed to rename temp file: {error}")
    })?;
    Ok(())
}

/// True only when the server's resolved identity matches a pin in `trusted`.
pub(crate) fn is_trusted_in(
    trusted: &BTreeMap<String, String>,
    name: &str,
    config: &McpServerConfig,
    workspace_root: &Path,
) -> bool {
    trusted
        .get(name)
        .is_some_and(|stored| stored == &fingerprint(config, workspace_root))
}

/// True only when the server's current executable identity matches a stored pin.
pub(crate) fn is_trusted(name: &str, config: &McpServerConfig, workspace_root: &Path) -> bool {
    is_trusted_in(&load_trusted(), name, config, workspace_root)
}

/// Records an explicit trust decision and returns the pinned fingerprint.
pub(crate) fn trust(
    name: &str,
    config: &McpServerConfig,
    workspace_root: &Path,
) -> Result<String, String> {
    with_trust_lock(|| {
        let pin = fingerprint(config, workspace_root);
        let mut trusted = load_trusted();
        trusted.insert(name.to_string(), pin.clone());
        save_trusted(&trusted)?;
        Ok(pin)
    })
}

/// Revokes trust by name within the same locked transaction and returns the pin
/// that was removed (if any), so the result reflects the committed state.
pub(crate) fn untrust(name: &str) -> Result<Option<String>, String> {
    with_trust_lock(|| {
        let mut trusted = load_trusted();
        let previous = trusted.remove(name);
        save_trusted(&trusted)?;
        Ok(previous)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TEST_STATE_LOCK;

    fn server(command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            command: command.to_string(),
            url: None,
            args: args.iter().map(|arg| arg.to_string()).collect(),
            env: std::collections::HashMap::new(),
            bearer_token: None,
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        }
    }

    fn isolated() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("COSH_STATES_DIR", tmp.path());
        tmp
    }

    fn ws() -> &'static Path {
        Path::new("/tmp/test-workspace")
    }

    #[test]
    fn fingerprint_pins_command_args_and_env() {
        let base = server("python3", &["-c", "print(1)"]);
        let mut changed_args = base.clone();
        changed_args.args = vec!["-c".to_string(), "print(2)".to_string()];
        let mut changed_env = base.clone();
        changed_env
            .env
            .insert("LD_PRELOAD".to_string(), "/tmp/evil.so".to_string());
        let mut changed_command = base.clone();
        changed_command.command = "sh".to_string();
        let pin = fingerprint(&base, ws());
        assert!(pin.starts_with("sha256:"));
        assert_eq!(pin, fingerprint(&base.clone(), ws()));
        assert_ne!(pin, fingerprint(&changed_args, ws()));
        assert_ne!(pin, fingerprint(&changed_env, ws()));
        assert_ne!(pin, fingerprint(&changed_command, ws()));
    }

    #[test]
    fn relative_command_pin_binds_workspace_root() {
        let config = server("./srv", &[]);
        let root_a = Path::new("/tmp/project-a");
        let root_b = Path::new("/tmp/project-b");
        assert_eq!(
            super::super::resolve_executable("./srv", root_a),
            "/tmp/project-a/./srv"
        );
        assert_ne!(fingerprint(&config, root_a), fingerprint(&config, root_b));
    }

    #[test]
    fn absolute_command_pin_still_binds_workspace_root() {
        // An absolute command resolves identically everywhere; only the
        // workspace_root entry distinguishes the pins, so removing that entry
        // would let a pin trusted under one workspace match another.
        let config = server("/usr/bin/python3", &["-c", "x"]);
        let root_a = Path::new("/tmp/project-a");
        let root_b = Path::new("/tmp/project-b");
        assert_eq!(
            super::super::resolve_executable("/usr/bin/python3", root_a),
            super::super::resolve_executable("/usr/bin/python3", root_b)
        );
        assert_ne!(fingerprint(&config, root_a), fingerprint(&config, root_b));
    }

    #[test]
    fn untrusted_server_is_not_trusted() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated();
        assert!(!is_trusted("probe", &server("python3", &["-c", "x"]), ws()));
    }

    #[test]
    fn trust_then_change_invalidates_pin() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated();
        let original = server("python3", &["-c", "x"]);
        trust("probe", &original, ws()).unwrap();
        assert!(is_trusted("probe", &original, ws()));
        let mut changed = original.clone();
        changed.args = vec!["-c".to_string(), "evil".to_string()];
        assert!(!is_trusted("probe", &changed, ws()));
        untrust("probe").unwrap();
        assert!(!is_trusted("probe", &original, ws()));
    }

    #[test]
    fn trust_is_per_server() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated();
        trust("a", &server("python3", &["a"]), ws()).unwrap();
        assert!(is_trusted("a", &server("python3", &["a"]), ws()));
        assert!(!is_trusted("b", &server("python3", &["a"]), ws()));
    }

    #[test]
    fn corrupt_store_fails_closed() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let tmp = isolated();
        std::fs::write(tmp.path().join("mcp-trusted.json"), "{not valid json").unwrap();
        assert!(load_trusted().is_empty());
        assert!(!is_trusted("probe", &server("python3", &["x"]), ws()));
    }

    #[test]
    fn fingerprint_env_insertion_order_independent() {
        let mut forward = server("python3", &["x"]);
        forward.env.insert("A".to_string(), "1".to_string());
        forward.env.insert("B".to_string(), "2".to_string());
        let mut reverse = server("python3", &["x"]);
        reverse.env.insert("B".to_string(), "2".to_string());
        reverse.env.insert("A".to_string(), "1".to_string());
        assert_eq!(fingerprint(&forward, ws()), fingerprint(&reverse, ws()));
    }

    #[test]
    fn allowed_tools_change_invalidates_pin() {
        let base = server("python3", &["x"]);
        let mut widened = base.clone();
        widened.allowed_tools = Some(vec![]);
        assert_ne!(fingerprint(&base, ws()), fingerprint(&widened, ws()));
    }

    #[test]
    fn untrust_revokes_pin_without_config() {
        let _lock = TEST_STATE_LOCK.lock().unwrap();
        let _tmp = isolated();
        let pin = trust("gone", &server("python3", &["x"]), ws()).unwrap();
        // The server may leave config entirely; the pin must still be revocable.
        let removed = untrust("gone").unwrap();
        assert_eq!(removed.as_deref(), Some(pin.as_str()));
        assert!(!load_trusted().contains_key("gone"));
        assert_eq!(untrust("gone").unwrap(), None);
    }
}
