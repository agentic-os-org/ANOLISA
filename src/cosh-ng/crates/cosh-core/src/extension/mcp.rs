//! Core-owned stdio MCP lifecycle and namespaced tool adapters.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use super::identity::validate_local_id;
use super::settings::ExtensionSettings;
use super::{
    EffectiveState, Extension, ExtensionDiagnostic, ExtensionHealth, ExtensionManager,
    McpServerContribution,
};
use crate::tool::{Tool, ToolContext, ToolKind, ToolRegistry, ToolResult};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_STDOUT_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_STDERR_TAIL_BYTES: usize = 16 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 1024 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Redaction-safe state for one declared MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerStatus {
    /// Canonical extension server identity.
    pub id: String,
    /// Whether startup failure prevents extension activation.
    pub required: bool,
    /// True after initialize and tool discovery succeed.
    pub healthy: bool,
    /// Number of namespaced tools registered.
    pub tool_count: usize,
    /// Stable diagnostic code when unhealthy.
    pub diagnostic_code: Option<String>,
}

/// Runtime startup or protocol failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    code: &'static str,
    message: String,
}

impl McpError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpError {}

/// One immutable MCP generation.
#[derive(Default)]
pub struct McpRuntime {
    clients: Vec<Arc<McpClient>>,
    tools: Vec<McpTool>,
    statuses: Vec<McpServerStatus>,
    diagnostics: Vec<ExtensionDiagnostic>,
}

impl McpRuntime {
    /// Starts MCP servers for active extensions and applies required/optional policy.
    pub async fn start(manager: &mut ExtensionManager) -> Self {
        let settings = ExtensionSettings::new(manager.workspace_dir().to_path_buf());
        Self::start_with_settings(
            manager,
            settings.as_ref().map_err(|error| error.to_string()),
        )
        .await
    }

    pub(crate) async fn start_with_settings(
        manager: &mut ExtensionManager,
        settings: Result<&ExtensionSettings, String>,
    ) -> Self {
        let mut runtime = Self::default();
        let mut extensions = manager
            .list_mut()
            .iter_mut()
            .filter(|extension| extension.is_active)
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| left.name.cmp(&right.name));

        for extension in extensions {
            let mut clients = Vec::new();
            let mut tools = Vec::new();
            let mut statuses = Vec::new();
            let mut required_failure = None;
            for server in &extension.mcp_servers {
                let result = match settings.as_ref() {
                    Ok(settings) => start_server(extension, server, settings).await,
                    Err(error) => Err(McpError::new(
                        "extension_settings_path_unavailable",
                        error.to_string(),
                    )),
                };
                match result {
                    Ok((client, mut discovered)) => {
                        statuses.push(McpServerStatus {
                            id: server.id.clone(),
                            required: server.required,
                            healthy: true,
                            tool_count: discovered.len(),
                            diagnostic_code: None,
                        });
                        clients.push(client);
                        tools.append(&mut discovered);
                    }
                    Err(error) => {
                        let diagnostic = ExtensionDiagnostic::new(
                            error.code(),
                            format!("{}: {error}", server.id),
                        );
                        statuses.push(McpServerStatus {
                            id: server.id.clone(),
                            required: server.required,
                            healthy: false,
                            tool_count: 0,
                            diagnostic_code: Some(error.code().to_string()),
                        });
                        if server.required {
                            required_failure = Some(diagnostic);
                            break;
                        }
                        extension.health = ExtensionHealth::Degraded;
                        extension.diagnostics.push(diagnostic.clone());
                        runtime.diagnostics.push(diagnostic);
                    }
                }
            }
            if let Some(diagnostic) = required_failure {
                for client in &clients {
                    client.shutdown().await;
                }
                extension.is_active = false;
                extension.effective_state = EffectiveState::Disabled;
                extension.health = ExtensionHealth::Broken;
                extension.diagnostics.push(diagnostic.clone());
                runtime.diagnostics.push(diagnostic);
                runtime.statuses.extend(statuses);
                continue;
            }
            runtime.clients.extend(clients);
            runtime.tools.extend(tools);
            runtime.statuses.extend(statuses);
        }
        runtime
    }

    /// Registers discovered tools without allowing MCP to replace built-ins.
    pub fn register_tools(&self, registry: &mut ToolRegistry) -> Result<(), McpError> {
        for tool in &self.tools {
            if registry.get(tool.name()).is_some() {
                return Err(McpError::new(
                    "extension_mcp_tool_collision",
                    format!("MCP tool collides with an existing tool: {}", tool.name()),
                ));
            }
        }
        for tool in &self.tools {
            registry.register(Box::new(tool.clone()));
        }
        Ok(())
    }

    /// Returns redaction-safe server health.
    pub fn statuses(&self) -> &[McpServerStatus] {
        &self.statuses
    }

    /// Returns startup diagnostics.
    pub fn diagnostics(&self) -> &[ExtensionDiagnostic] {
        &self.diagnostics
    }

    /// Stops accepting calls, drains in-flight requests, and terminates children.
    pub async fn shutdown(&self) {
        for client in &self.clients {
            client.shutdown().await;
        }
    }
}

async fn start_server(
    extension: &Extension,
    server: &McpServerContribution,
    settings: &ExtensionSettings,
) -> Result<(Arc<McpClient>, Vec<McpTool>), McpError> {
    let command = resolve_extension_path(&server.command, &extension.path)?;
    let args = server
        .args
        .iter()
        .map(|argument| resolve_extension_path(argument, &extension.path))
        .collect::<Result<Vec<_>, _>>()?;
    let env = resolve_server_env(extension, server, settings)?;
    let client = Arc::new(McpClient::spawn(&command, &args, &env).await?);
    let definitions = match client.initialize_and_list().await {
        Ok(definitions) => definitions,
        Err(error) => {
            client.shutdown().await;
            return Err(error);
        }
    };
    let mut names = BTreeSet::new();
    let mut tools = Vec::new();
    for definition in definitions {
        validate_local_id(&definition.name).map_err(|error| {
            McpError::new(
                "extension_mcp_tool_identity_invalid",
                format!("invalid MCP tool identity {}: {error}", definition.name),
            )
        })?;
        if !names.insert(definition.name.clone()) {
            return Err(McpError::new(
                "extension_mcp_tool_duplicate",
                format!("MCP server returned duplicate tool: {}", definition.name),
            ));
        }
        tools.push(McpTool {
            canonical_name: format!("{}/{}", server.id, definition.name),
            remote_name: definition.name,
            description: definition.description,
            input_schema: definition.input_schema,
            client: Arc::clone(&client),
        });
    }
    Ok((client, tools))
}

fn resolve_extension_path(value: &str, extension_path: &Path) -> Result<String, McpError> {
    let marker = "${extensionPath}";
    if value.contains("${") && !value.contains(marker) {
        return Err(McpError::new(
            "extension_mcp_variable_unsupported",
            "MCP command and args only support ${extensionPath}",
        ));
    }
    if value.matches(marker).count() > 1 {
        return Err(McpError::new(
            "extension_mcp_path_invalid",
            "MCP command and each argument may reference ${extensionPath} at most once",
        ));
    }
    let root = extension_path.canonicalize().map_err(|error| {
        McpError::new(
            "extension_mcp_path_unreadable",
            format!("failed to resolve extension package root: {error}"),
        )
    })?;
    let mut remainder = value;
    while let Some(index) = remainder.find(marker) {
        let suffix = &remainder[index + marker.len()..];
        if !suffix.is_empty() && !suffix.starts_with('/') {
            return Err(McpError::new(
                "extension_mcp_path_invalid",
                "${extensionPath} must be followed by '/' or end the argument",
            ));
        }
        let relative = suffix.strip_prefix('/').unwrap_or_default();
        if Path::new(relative)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(McpError::new(
                "extension_mcp_path_escape",
                "MCP command or argument escapes the extension package",
            ));
        }
        let candidate = root.join(relative);
        if candidate.exists() {
            let resolved = candidate.canonicalize().map_err(|error| {
                McpError::new(
                    "extension_mcp_path_unreadable",
                    format!("failed to resolve MCP package path: {error}"),
                )
            })?;
            if !resolved.starts_with(&root) {
                return Err(McpError::new(
                    "extension_mcp_path_escape",
                    "MCP command or argument resolves outside the extension package",
                ));
            }
        }
        remainder = suffix;
        if remainder.is_empty() {
            break;
        }
    }
    Ok(value.replace(marker, &root.to_string_lossy()))
}

fn resolve_server_env(
    extension: &Extension,
    server: &McpServerContribution,
    settings: &ExtensionSettings,
) -> Result<BTreeMap<String, String>, McpError> {
    let mut resolved = BTreeMap::new();
    for (key, value) in &server.env {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err(McpError::new(
                "extension_mcp_env_invalid",
                format!("invalid environment declaration for {}", server.id),
            ));
        }
        let value = if let Some(setting_key) = value
            .strip_prefix("${setting:")
            .and_then(|value| value.strip_suffix('}'))
        {
            let value = settings
                .resolve(extension, setting_key)
                .map_err(|error| McpError::new(error.code(), error.to_string()))?
                .ok_or_else(|| {
                    McpError::new(
                        "extension_setting_required_missing",
                        format!("MCP server {} requires setting {setting_key}", server.id),
                    )
                })?;
            scalar_to_env(value).ok_or_else(|| {
                McpError::new(
                    "extension_mcp_setting_type_invalid",
                    format!("MCP environment setting is not scalar: {setting_key}"),
                )
            })?
        } else if value.contains("${") {
            return Err(McpError::new(
                "extension_mcp_variable_unsupported",
                format!("unsupported MCP environment variable in {}", server.id),
            ));
        } else {
            value.clone()
        };
        resolved.insert(key.clone(), value);
    }
    Ok(resolved)
}

fn scalar_to_env(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug)]
struct McpToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Clone)]
struct McpTool {
    canonical_name: String,
    remote_name: String,
    description: String,
    input_schema: Value,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.canonical_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn kind(&self) -> ToolKind {
        ToolKind::External
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        self.client
            .call_tool(&self.remote_name, params)
            .await
            .map_err(|error| format!("{}: {error}", error.code()))
    }
}

struct McpIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct McpClient {
    child: Mutex<Child>,
    io: AsyncMutex<McpIo>,
    next_id: AtomicU64,
    accepting: AtomicBool,
    in_flight: AtomicUsize,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
}

impl McpClient {
    async fn spawn(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut command = Command::new(executable);
        command
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in ["PATH", "TMPDIR", "TMP", "TEMP", "SYSTEMROOT"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs(env);
        let mut child = command.spawn().map_err(|error| {
            McpError::new(
                "extension_mcp_spawn_failed",
                format!("failed to start MCP executable: {error}"),
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::new(
                "extension_mcp_stdio_unavailable",
                "MCP stdin is unavailable",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::new(
                "extension_mcp_stdio_unavailable",
                "MCP stdout is unavailable",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            McpError::new(
                "extension_mcp_stdio_unavailable",
                "MCP stderr is unavailable",
            )
        })?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(capture_stderr(stderr, Arc::clone(&stderr_tail)));
        Ok(Self {
            child: Mutex::new(child),
            io: AsyncMutex::new(McpIo {
                stdin,
                stdout: BufReader::new(stdout),
            }),
            next_id: AtomicU64::new(1),
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            stderr_tail,
        })
    }

    async fn initialize_and_list(&self) -> Result<Vec<McpToolDefinition>, McpError> {
        let initialized = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "cosh-core",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
                STARTUP_TIMEOUT,
            )
            .await?;
        let negotiated = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::new(
                    "extension_mcp_protocol_invalid",
                    "initialize response is missing protocolVersion",
                )
            })?;
        if !matches!(
            negotiated,
            "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05"
        ) {
            return Err(McpError::new(
                "extension_mcp_protocol_unsupported",
                format!("MCP server selected unsupported protocol version {negotiated}"),
            ));
        }
        self.notify("notifications/initialized", None).await?;
        let mut cursor = None;
        let mut definitions = Vec::new();
        loop {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({"cursor": cursor}))
                .unwrap_or_else(|| json!({}));
            let result = self.request("tools/list", params, STARTUP_TIMEOUT).await?;
            let tools = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    McpError::new(
                        "extension_mcp_tools_invalid",
                        "tools/list response is missing tools",
                    )
                })?;
            for tool in tools {
                definitions.push(McpToolDefinition {
                    name: tool
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            McpError::new("extension_mcp_tools_invalid", "MCP tool is missing name")
                        })?
                        .to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("Extension MCP tool")
                        .to_string(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(definitions)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                CALL_TIMEOUT,
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output_value = result
            .get("structuredContent")
            .or_else(|| result.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        let output = serde_json::to_string(&output_value).map_err(|error| {
            McpError::new(
                "extension_mcp_result_invalid",
                format!("failed to serialize MCP tool result: {error}"),
            )
        })?;
        if output.len() > MAX_TOOL_RESULT_BYTES {
            return Err(McpError::new(
                "extension_mcp_result_too_large",
                "MCP tool result exceeds the configured size limit",
            ));
        }
        Ok(if is_error {
            ToolResult::error(output)
        } else {
            ToolResult::success(output)
        })
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, McpError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(McpError::new(
                "extension_mcp_draining",
                "MCP generation is no longer accepting calls",
            ));
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let result = timeout(deadline, self.request_inner(method, params)).await;
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        match result {
            Ok(result) => result,
            Err(_) => {
                self.accepting.store(false, Ordering::Release);
                if let Ok(mut child) = self.child.lock() {
                    let _ = child.start_kill();
                }
                Err(McpError::new(
                    "extension_mcp_timeout",
                    format!("MCP request timed out: {method}"),
                ))
            }
        }
    }

    async fn request_inner(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let bytes = serde_json::to_vec(&message).map_err(|error| {
            McpError::new(
                "extension_mcp_protocol_invalid",
                format!("failed to encode MCP request: {error}"),
            )
        })?;
        let mut io = self.io.lock().await;
        io.stdin.write_all(&bytes).await.map_err(mcp_write_error)?;
        io.stdin.write_all(b"\n").await.map_err(mcp_write_error)?;
        io.stdin.flush().await.map_err(mcp_write_error)?;
        loop {
            let response = read_message(&mut io.stdout).await?;
            if response.get("id").is_none() {
                continue;
            }
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(McpError::new(
                    "extension_mcp_response_id_mismatch",
                    "MCP response ID does not match the outstanding request",
                ));
            }
            if let Some(error) = response.get("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-1);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP protocol error");
                return Err(McpError::new(
                    "extension_mcp_remote_error",
                    format!("MCP server error {code}: {}", truncate(message, 512)),
                ));
            }
            return response.get("result").cloned().ok_or_else(|| {
                McpError::new(
                    "extension_mcp_protocol_invalid",
                    "MCP response has neither result nor error",
                )
            });
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        let bytes = serde_json::to_vec(&message).map_err(|error| {
            McpError::new(
                "extension_mcp_protocol_invalid",
                format!("failed to encode MCP notification: {error}"),
            )
        })?;
        let mut io = self.io.lock().await;
        io.stdin.write_all(&bytes).await.map_err(mcp_write_error)?;
        io.stdin.write_all(b"\n").await.map_err(mcp_write_error)?;
        io.stdin.flush().await.map_err(mcp_write_error)
    }

    async fn shutdown(&self) {
        if !self.accepting.swap(false, Ordering::AcqRel) {
            return;
        }
        let drain = async {
            while self.in_flight.load(Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        };
        let _ = timeout(SHUTDOWN_TIMEOUT, drain).await;
        let _ = timeout(SHUTDOWN_TIMEOUT, self.request_inner("shutdown", json!({}))).await;
        let _ = self.notify("exit", None).await;
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
    }
}

async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value, McpError> {
    let mut bytes = Vec::new();
    loop {
        let byte = stdout.read_u8().await.map_err(|error| {
            McpError::new(
                "extension_mcp_stdout_failed",
                format!("failed to read MCP stdout: {error}"),
            )
        })?;
        if byte == b'\n' {
            break;
        }
        bytes.push(byte);
        if bytes.len() > MAX_STDOUT_MESSAGE_BYTES {
            return Err(McpError::new(
                "extension_mcp_stdout_too_large",
                "MCP stdout message exceeds the configured size limit",
            ));
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        McpError::new(
            "extension_mcp_malformed_json",
            format!("MCP stdout is not valid JSON: {error}"),
        )
    })
}

async fn capture_stderr(mut stderr: ChildStderr, tail: Arc<Mutex<VecDeque<u8>>>) {
    let mut buffer = [0u8; 1024];
    while let Ok(count) = stderr.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let Ok(mut tail) = tail.lock() else {
            break;
        };
        for byte in &buffer[..count] {
            tail.push_back(*byte);
            if tail.len() > MAX_STDERR_TAIL_BYTES {
                tail.pop_front();
            }
        }
    }
}

fn mcp_write_error(error: std::io::Error) -> McpError {
    McpError::new(
        "extension_mcp_stdin_failed",
        format!("failed to write MCP stdin: {error}"),
    )
}

fn truncate(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        value
    } else {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        &value[..end]
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::extension::{ExtensionManager, EXTENSION_CONFIG_FILENAME};

    #[test]
    fn extension_path_resolver_rejects_lexical_escape() {
        let temporary = tempfile::tempdir().unwrap();
        let error =
            resolve_extension_path("--config=${extensionPath}/../outside", temporary.path())
                .unwrap_err();
        assert_eq!(error.code(), "extension_mcp_path_escape");
    }

    const FIXTURE: &str = r#"
import json
import os
import sys
import time

mode = os.environ.get("FIXTURE_MODE", "normal")
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    request_id = request.get("id")
    if method == "initialize":
        if mode == "malformed":
            print("{bad-json", flush=True)
            continue
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "1.0.0"}
        }
    elif method == "notifications/initialized":
        sys.stderr.write("x" * 32768)
        sys.stderr.flush()
        continue
    elif method == "tools/list":
        if mode == "oversize":
            print(json.dumps({"jsonrpc":"2.0","id":request_id,"result":{"tools":[],"padding":"x" * 1100000}}), flush=True)
            continue
        result = {"tools":[{
            "name":"read_file",
            "description":"fixture tool",
            "inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}
        }]}
    elif method == "tools/call":
        if mode == "timeout":
            time.sleep(1)
        result = {
            "content":[{
                "type":"text",
                "text":json.dumps({
                    "value":request["params"].get("arguments", {}).get("value"),
                    "declared":os.environ.get("DECLARED"),
                    "env_keys":sorted(os.environ.keys())
                })
            }],
            "isError": mode == "tool-error"
        }
    elif method == "shutdown":
        marker = os.environ.get("SHUTDOWN_MARKER")
        if marker:
            with open(marker, "w", encoding="utf-8") as handle:
                handle.write("shutdown")
        result = {}
    elif method == "exit":
        break
    else:
        continue
    print(json.dumps({"jsonrpc":"2.0","id":request_id,"result":result}), flush=True)
"#;

    fn fixture(root: &Path) -> (String, Vec<String>) {
        let script = root.join("fixture.py");
        fs::write(&script, FIXTURE).unwrap();
        (
            which::which("python3")
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            vec![script.to_string_lossy().into_owned()],
        )
    }

    fn manager(root: &Path, command: &str, args: &[String], required: bool) -> ExtensionManager {
        let user = root.join("extensions");
        let system = root.join("system");
        let package = user.join("example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "mcpServers": {
                    "fixture": {
                        "transport": "stdio",
                        "command": command,
                        "args": args,
                        "env": {"DECLARED": "visible"},
                        "required": required
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            root.join("workspace"),
            Some(user),
            Some(system),
            root.join("state"),
        );
        manager.refresh();
        manager
    }

    fn tool_context() -> ToolContext {
        ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn runtime_discovers_namespaced_other_tools_and_calls_them() {
        let temporary = tempfile::tempdir().unwrap();
        let (command, args) = fixture(temporary.path());
        let mut manager = manager(temporary.path(), &command, &args, true);
        let runtime = McpRuntime::start(&mut manager).await;
        assert!(
            runtime.diagnostics().is_empty(),
            "{:?}",
            runtime.diagnostics()
        );
        assert_eq!(runtime.statuses()[0].tool_count, 1);

        let mut registry = ToolRegistry::new();
        runtime.register_tools(&mut registry).unwrap();
        let tool = registry
            .get("example.ops/mcp/fixture/read_file")
            .expect("namespaced MCP tool");
        assert_eq!(tool.kind(), ToolKind::External);
        assert!(registry.get("read_file").is_none());
        let result = tool
            .invoke(json!({"value": "hello"}), &tool_context())
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
        assert!(result.output.contains("DECLARED"));
        assert!(!result.output.contains("HOME"));
        assert!(runtime.clients[0].stderr_tail.lock().unwrap().len() <= MAX_STDERR_TAIL_BYTES);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn optional_spawn_failure_degrades_but_required_failure_disables() {
        let optional_root = tempfile::tempdir().unwrap();
        let mut optional = manager(optional_root.path(), "/definitely/missing/mcp", &[], false);
        let runtime = McpRuntime::start(&mut optional).await;
        assert!(optional.list()[0].is_active);
        assert_eq!(optional.list()[0].health, ExtensionHealth::Degraded);
        assert!(!runtime.statuses()[0].healthy);

        let required_root = tempfile::tempdir().unwrap();
        let mut required = manager(required_root.path(), "/definitely/missing/mcp", &[], true);
        let runtime = McpRuntime::start(&mut required).await;
        assert!(!required.list()[0].is_active);
        assert_eq!(required.list()[0].health, ExtensionHealth::Broken);
        assert_eq!(runtime.diagnostics()[0].code, "extension_mcp_spawn_failed");
    }

    #[tokio::test]
    async fn malformed_oversize_timeout_and_tool_error_are_bounded() {
        let temporary = tempfile::tempdir().unwrap();
        let (command, args) = fixture(temporary.path());

        let malformed = McpClient::spawn(
            &command,
            &args,
            &BTreeMap::from([("FIXTURE_MODE".to_string(), "malformed".to_string())]),
        )
        .await
        .unwrap();
        assert_eq!(
            malformed.initialize_and_list().await.unwrap_err().code(),
            "extension_mcp_malformed_json"
        );

        let oversize = McpClient::spawn(
            &command,
            &args,
            &BTreeMap::from([("FIXTURE_MODE".to_string(), "oversize".to_string())]),
        )
        .await
        .unwrap();
        assert_eq!(
            oversize.initialize_and_list().await.unwrap_err().code(),
            "extension_mcp_stdout_too_large"
        );

        let timed = McpClient::spawn(
            &command,
            &args,
            &BTreeMap::from([("FIXTURE_MODE".to_string(), "timeout".to_string())]),
        )
        .await
        .unwrap();
        timed.initialize_and_list().await.unwrap();
        let error = timed
            .request(
                "tools/call",
                json!({"name":"read_file","arguments":{}}),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "extension_mcp_timeout");

        let tool_error = McpClient::spawn(
            &command,
            &args,
            &BTreeMap::from([("FIXTURE_MODE".to_string(), "tool-error".to_string())]),
        )
        .await
        .unwrap();
        tool_error.initialize_and_list().await.unwrap();
        let result = tool_error.call_tool("read_file", json!({})).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn shutdown_drains_and_notifies_server() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("shutdown.marker");
        let (command, args) = fixture(temporary.path());
        let client = McpClient::spawn(
            &command,
            &args,
            &BTreeMap::from([(
                "SHUTDOWN_MARKER".to_string(),
                marker.to_string_lossy().into_owned(),
            )]),
        )
        .await
        .unwrap();
        client.initialize_and_list().await.unwrap();
        client.shutdown().await;
        assert_eq!(fs::read_to_string(marker).unwrap(), "shutdown");
        let error = client.call_tool("read_file", json!({})).await.unwrap_err();
        assert_eq!(error.code(), "extension_mcp_draining");
    }
}
