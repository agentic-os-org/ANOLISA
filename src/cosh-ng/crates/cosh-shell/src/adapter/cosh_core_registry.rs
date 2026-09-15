use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use super::CoshCoreAdapter;

pub(super) const REGISTRY_READ_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REGISTRY_MUTATION_TIMEOUT: Duration = Duration::from_secs(120);
pub(super) const AUTH_CONFIGURE_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, PartialEq, Eq)]
/// Distinguishes registry protocol failures from transport failures.
pub(crate) enum RegistryQueryError {
    /// The request could not produce a valid, correlated registry response.
    Transport(String),
    /// The core returned a valid registry response that rejected the request.
    Response {
        message: String,
        code: Option<String>,
    },
}

impl RegistryQueryError {
    /// Preserves the existing string-based adapter API for ordinary callers.
    pub(crate) fn into_message(self) -> String {
        match self {
            Self::Transport(message) | Self::Response { message, .. } => message,
        }
    }
}

/// Outcome of the `/health` live-core probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoreLiveness {
    /// The persistent core answered a live registry query.
    Live,
    /// A persistent core exists but did not answer within the timeout:
    /// busy inside a turn, hung, or transport broken.
    NoResponse(String),
    /// No persistent core runtime has been started in this session.
    NoRuntime,
}

impl CoshCoreAdapter {
    /// Probes the persistent core without falling back to a short-lived
    /// child process.
    ///
    /// A `--registry` subprocess response proves the binary works but not
    /// that the persistent agent process is alive, so it must never count as
    /// liveness evidence (issue #3055 `/health` live probe).
    pub(crate) fn core_liveness(&self) -> CoreLiveness {
        match self
            .runtime
            .live_registry_query("auth", "state", Value::Null)
        {
            Some(Ok(_)) => CoreLiveness::Live,
            Some(Err(error)) => CoreLiveness::NoResponse(error.into_message()),
            None => {
                if self.runtime.is_live() {
                    CoreLiveness::NoResponse("core is busy in a turn".to_string())
                } else {
                    CoreLiveness::NoRuntime
                }
            }
        }
    }
}

impl CoshCoreAdapter {
    /// Routes registry requests through the live core, falling back before a runtime exists.
    pub fn registry_query(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.registry_query_classified(domain, action, params)
            .map_err(RegistryQueryError::into_message)
    }

    /// Returns a classified error for callers that may recover transport failures.
    pub(crate) fn registry_query_classified(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Result<Value, RegistryQueryError> {
        if let Some(result) = self
            .runtime
            .live_registry_query(domain, action, params.clone())
        {
            return result;
        }
        let result = self.registry_query_short(domain, action, params);
        if result.is_ok() {
            self.runtime.note_external_mutation(domain, action);
        }
        result
    }

    /// Schedules a safe-point snapshot reload after an out-of-band MCP
    /// mutation: `cosh-core mcp` subprocesses only touch on-disk state, and
    /// the live core rebuilds MCP tools solely during extension snapshot
    /// rebuilds, so without this the running agent would keep stale tools,
    /// connections, and credentials until the next restart.
    pub(crate) fn note_mcp_mutation(&self) {
        self.runtime.note_external_mutation("extensions", "reload");
    }

    fn registry_query_short(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Result<Value, RegistryQueryError> {
        let request_id = format!("reg-{}", std::process::id());
        let request = serde_json::json!({
            "type": "registry_request",
            "request_id": request_id,
            "domain": domain,
            "action": action,
            "params": params,
        });

        let request_json = serde_json::to_string(&request)
            .map_err(|error| RegistryQueryError::Transport(format!("serialize error: {error}")))?;

        let mut command = Command::new(&self.program);
        command
            .arg("--registry")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);

        // Forward the user's shell cwd so the registry-mode cosh-core
        // resolves the same project root as the headless runtime. Falls
        // back to the active session's workspace scope when the shell
        // cwd is unavailable (e.g. programmatic callers without a PTY).
        let workspace = self
            .shell_cwd
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .or_else(|| {
                self.session
                    .lock()
                    .ok()
                    .and_then(|s| s.active_workspace_scope().map(str::to_string))
            });
        if let Some(workspace) = workspace {
            command.arg("--workspace").arg(workspace);
        }

        let mut child = command.spawn().map_err(|error| {
            RegistryQueryError::Transport(format!("failed to spawn cosh-core --registry: {error}"))
        })?;

        // Write request to stdin
        let write_result = child
            .stdin
            .as_mut()
            .ok_or_else(|| RegistryQueryError::Transport("failed to open stdin".to_string()))
            .and_then(|stdin| {
                writeln!(stdin, "{request_json}")
                    .map_err(|error| RegistryQueryError::Transport(format!("write error: {error}")))
            });
        if let Err(error) = write_result {
            super::terminate_and_reap_process(&mut child);
            return Err(error);
        }
        // Drop stdin to signal EOF.
        drop(child.stdin.take());

        // Read response from stdout with timeout
        let Some(stdout) = child.stdout.take() else {
            super::terminate_and_reap_process(&mut child);
            return Err(RegistryQueryError::Transport(
                "failed to open stdout".to_string(),
            ));
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let reader_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) if !l.trim().is_empty() => {
                        let _ = tx.send(Ok(l));
                        return;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        let _ = tx.send(Err(format!("read error: {e}")));
                        return;
                    }
                }
            }
            let _ = tx.send(Err("no response received (EOF)".to_string()));
        });

        let response_line = match rx.recv_timeout(registry_timeout(domain, action)) {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => {
                super::terminate_and_reap_process(&mut child);
                let _ = reader_handle.join();
                return Err(RegistryQueryError::Transport(e));
            }
            Err(_) => {
                super::terminate_and_reap_process(&mut child);
                let _ = reader_handle.join();
                return Err(RegistryQueryError::Transport(
                    "registry query timed out".to_string(),
                ));
            }
        };

        let _ = reader_handle.join();
        let _ = child.wait();

        // Parse the response
        let resp: Value = serde_json::from_str(&response_line)
            .map_err(|error| RegistryQueryError::Transport(format!("parse error: {error}")))?;

        let success = resp
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if success {
            Ok(resp.get("data").cloned().unwrap_or(Value::Null))
        } else {
            let error = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Err(RegistryQueryError::Response {
                message: error,
                code: resp
                    .get("data")
                    .and_then(|data| data.get("error_code"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
    }
}

pub(super) fn extension_mutation_requires_reload(domain: &str, action: &str) -> bool {
    domain == "extensions"
        && matches!(
            action,
            "enable"
                | "disable"
                | "select-source"
                | "settings-set"
                | "settings-unset"
                | "commit"
                | "update-all-commit"
                | "uninstall"
                | "recover"
                | "reload"
        )
}

pub(super) fn registry_timeout(domain: &str, action: &str) -> Duration {
    if domain == "auth" && action == "configure" {
        return AUTH_CONFIGURE_TIMEOUT;
    }
    let long_running_extension_action = extension_mutation_requires_reload(domain, action)
        || (domain == "extensions"
            && matches!(
                action,
                "install-preflight"
                    | "link-preflight"
                    | "update-preflight"
                    | "update-all-preflight"
                    | "doctor"
                    | "new"
            ));
    if long_running_extension_action {
        REGISTRY_MUTATION_TIMEOUT
    } else {
        REGISTRY_READ_TIMEOUT
    }
}
