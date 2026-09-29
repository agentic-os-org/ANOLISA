//! Codex app-server requests used to trust installed plugin hooks.

use serde::Deserialize;

use crate::adapter::AdapterError;
use crate::adapter::driver::{CliOutput, FrameworkCommand, FrameworkRpcSession};

const ID_INITIALIZE: u32 = 0;
const ID_OPERATION: u32 = 1;
const DEFAULT_HOOKS_FILE: &str = "hooks/hooks.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookEntry {
    key: String,
    current_hash: String,
    source: String,
    #[serde(default)]
    plugin_id: Option<String>,
    is_managed: bool,
}

#[derive(Debug, Deserialize)]
struct HooksListEntry {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    hooks: Vec<HookEntry>,
    #[serde(default)]
    warnings: Vec<serde_json::Value>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct HooksListResult {
    #[serde(default)]
    data: Vec<HooksListEntry>,
}

/// Build a session that asks Codex for hook identities in an isolated cwd.
pub(super) fn list_session(
    program: String,
    timeout: std::time::Duration,
    cwd: &std::path::Path,
) -> FrameworkRpcSession {
    rpc_session(
        program,
        timeout,
        format!(
            r#"{{"jsonrpc":"2.0","id":{ID_OPERATION},"method":"hooks/list","params":{{"cwds":[{cwd}]}}}}"#,
            cwd = json_string(&cwd.to_string_lossy()),
        ),
    )
}

/// Build one atomic upsert of the trusted hashes returned by Codex.
pub(super) fn write_session(
    program: String,
    timeout: std::time::Duration,
    trust_state: serde_json::Value,
) -> FrameworkRpcSession {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": ID_OPERATION,
        "method": "config/batchWrite",
        "params": {
            "edits": [{
                "keyPath": "hooks.state",
                "value": trust_state,
                "mergeStrategy": "upsert"
            }],
            "reloadUserConfig": true
        }
    });
    rpc_session(program, timeout, request.to_string())
}

fn rpc_session(
    program: String,
    timeout: std::time::Duration,
    operation: String,
) -> FrameworkRpcSession {
    FrameworkRpcSession {
        command: FrameworkCommand {
            program,
            args: vec!["app-server".to_string(), "--stdio".to_string()],
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout,
        },
        requests: vec![initialize_request(), operation],
        expected_responses: 2,
    }
}

fn initialize_request() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": ID_INITIALIZE,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "anolisa",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
    .to_string()
}

fn json_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Extract this plugin's hook hashes from a successful `hooks/list` reply.
///
/// Returns `None` when every matching hook is managed externally. A missing
/// plugin is an error because accepting an empty result would recreate the
/// silent failure this trust step is intended to prevent.
pub(super) fn plugin_trust_state(
    program: &str,
    output: &CliOutput,
    plugin_ref: &str,
) -> Result<Option<serde_json::Value>, AdapterError> {
    let result = operation_result(program, "hooks/list", output)?;
    let parsed: HooksListResult = serde_json::from_value(result).map_err(|source| {
        framework_error(program, "hooks/list", format!("unexpected reply: {source}"))
    })?;

    let mut found = false;
    let mut state = serde_json::Map::new();
    for entry in parsed.data {
        if let Some(diagnostic) = entry.errors.first().or_else(|| entry.warnings.first()) {
            return Err(framework_error(
                program,
                "hooks/list",
                format!(
                    "hook discovery for '{}' reported: {}",
                    entry.cwd.as_deref().unwrap_or("<unknown>"),
                    render_diagnostic(diagnostic)
                ),
            ));
        }
        for hook in entry.hooks {
            if hook.source != "plugin" || hook.plugin_id.as_deref() != Some(plugin_ref) {
                continue;
            }
            found = true;
            if !hook.is_managed {
                state.insert(
                    hook.key,
                    serde_json::json!({ "trusted_hash": hook.current_hash }),
                );
            }
        }
    }

    if !found {
        return Err(framework_error(
            program,
            "hooks/list",
            format!("reported no hooks for installed plugin '{plugin_ref}'"),
        ));
    }
    Ok((!state.is_empty()).then_some(serde_json::Value::Object(state)))
}

/// Confirm that Codex persisted the trust-state update.
pub(super) fn confirm_write(program: &str, output: &CliOutput) -> Result<(), AdapterError> {
    let result = operation_result(program, "config/batchWrite", output)?;
    match result.get("status").and_then(serde_json::Value::as_str) {
        Some("ok") => Ok(()),
        Some("okOverridden") => {
            let metadata = result.get("overriddenMetadata");
            let message = metadata
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("a higher-precedence configuration overrides hooks.state");
            let layer = metadata
                .and_then(|value| value.pointer("/overridingLayer/name"))
                .map(render_diagnostic)
                .unwrap_or_else(|| "<unknown>".to_string());
            Err(framework_error(
                program,
                "config/batchWrite",
                format!("hook trust was overridden by layer {layer}: {message}"),
            ))
        }
        Some(status) => Err(framework_error(
            program,
            "config/batchWrite",
            format!("reply returned unexpected write status '{status}'"),
        )),
        None => Err(framework_error(
            program,
            "config/batchWrite",
            "reply did not confirm a write status".to_string(),
        )),
    }
}

fn operation_result(
    program: &str,
    operation: &str,
    output: &CliOutput,
) -> Result<serde_json::Value, AdapterError> {
    for line in output.stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("id").and_then(serde_json::Value::as_u64) != Some(u64::from(ID_OPERATION)) {
            continue;
        }
        if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown app-server error");
            return Err(framework_error(program, operation, detail.to_string()));
        }
        return message.get("result").cloned().ok_or_else(|| {
            framework_error(
                program,
                operation,
                "reply contained neither result nor error".to_string(),
            )
        });
    }

    let reason = if output.timed_out {
        "app-server timed out".to_string()
    } else {
        format!(
            "app-server produced no reply (exit {:?}); stderr: {}",
            output.status,
            output
                .stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("<empty>")
        )
    };
    Err(framework_error(program, operation, reason))
}

fn framework_error(program: &str, operation: &str, reason: String) -> AdapterError {
    AdapterError::FrameworkCli {
        program: program.to_string(),
        reason: format!("codex {operation} failed: {reason}"),
    }
}

fn render_diagnostic(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

/// Whether the plugin bundle contains at least one declared hook handler.
pub(super) fn bundle_declares_hooks(
    resource_root: &std::path::Path,
    plugin_manifest: &std::path::Path,
) -> bool {
    let manifest = std::fs::read_to_string(plugin_manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let explicit = manifest
        .as_ref()
        .and_then(|value| value.get("hooks"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let hooks_file = match explicit {
        Some(path) => match safe_relative_path(path) {
            Some(path) => resource_root.join(path),
            None => return true,
        },
        None => resource_root.join(DEFAULT_HOOKS_FILE),
    };
    let Ok(contents) = std::fs::read_to_string(&hooks_file) else {
        return explicit.is_some();
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return true;
    };
    document
        .get("hooks")
        .and_then(serde_json::Value::as_object)
        .and_then(|events| {
            events.values().try_fold(false, |found, groups| {
                let groups = groups.as_array()?;
                Some(
                    found
                        || groups.iter().any(|group| {
                            group
                                .get("hooks")
                                .and_then(serde_json::Value::as_array)
                                .is_some_and(|hooks| !hooks.is_empty())
                        }),
                )
            })
        })
        .unwrap_or(true)
}

fn safe_relative_path(path: &str) -> Option<std::path::PathBuf> {
    let mut safe = std::path::PathBuf::new();
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => safe.push(part),
            _ => return None,
        }
    }
    (!safe.as_os_str().is_empty()).then_some(safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str) -> CliOutput {
        CliOutput {
            status: Some(0),
            timed_out: false,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn trusts_only_hooks_owned_by_the_target_plugin() {
        let stdout = concat!(
            "{\"id\":0,\"result\":{}}\n",
            "{\"id\":1,\"result\":{\"data\":[{\"hooks\":[",
            "{\"key\":\"mine\",\"currentHash\":\"sha256:a\",\"source\":\"plugin\",",
            "\"pluginId\":\"p@m\",\"isManaged\":false},",
            "{\"key\":\"other\",\"currentHash\":\"sha256:b\",\"source\":\"plugin\",",
            "\"pluginId\":\"q@m\",\"isManaged\":false},",
            "{\"key\":\"managed\",\"currentHash\":\"sha256:c\",\"source\":\"plugin\",",
            "\"pluginId\":\"p@m\",\"isManaged\":true}",
            "],\"warnings\":[],\"errors\":[]}]}}\n"
        );
        let state = plugin_trust_state("codex", &output(stdout), "p@m")
            .expect("valid reply")
            .expect("unmanaged hook");
        assert_eq!(
            state,
            serde_json::json!({ "mine": { "trusted_hash": "sha256:a" } })
        );
    }

    #[test]
    fn missing_target_plugin_is_not_silent_success() {
        let stdout = "{\"id\":1,\"result\":{\"data\":[{\"hooks\":[]}]}}\n";
        let error = plugin_trust_state("codex", &output(stdout), "p@m").expect_err("must fail");
        assert!(error.to_string().contains("reported no hooks"));
    }

    #[test]
    fn config_write_uses_one_safe_state_map_upsert() {
        let session = write_session(
            "codex".to_string(),
            std::time::Duration::from_secs(60),
            serde_json::json!({ "p@m:hooks/hooks.json:stop:0:0": {
                "trusted_hash": "sha256:a"
            }}),
        );
        let request: serde_json::Value =
            serde_json::from_str(&session.requests[1]).expect("valid request");
        assert_eq!(request["params"]["edits"][0]["keyPath"], "hooks.state");
        assert_eq!(request["params"]["edits"][0]["mergeStrategy"], "upsert");
    }

    #[test]
    fn empty_hooks_document_skips_trust_round_trip() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("hooks")).expect("hooks directory");
        std::fs::write(root.path().join("plugin.json"), "{}").expect("manifest");
        std::fs::write(root.path().join(DEFAULT_HOOKS_FILE), r#"{"hooks":{}}"#)
            .expect("hooks file");
        assert!(!bundle_declares_hooks(
            root.path(),
            &root.path().join("plugin.json")
        ));
    }
}
