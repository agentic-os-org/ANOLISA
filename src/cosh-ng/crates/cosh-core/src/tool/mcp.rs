//! Minimal stdio MCP client support for dynamically discovered agent tools.

mod http;
mod oauth;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, timeout_at, Duration, Instant};

use crate::cli::{McpArgs, McpCommand};
use crate::config::{CoreConfig, McpServerConfig};
use crate::state::{self, MCP_SERVERS_STATE};

use super::{Tool, ToolContext, ToolKind, ToolRegistry, ToolResult};

const CLIENT_NAME: &str = "cosh-ng";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub(super) const HTTP_MCP_PROTOCOL_VERSION: &str = MCP_PROTOCOL_VERSION;
const MCP_COMPATIBLE_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_TOOL_NAME_LEN: usize = 64;
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_TOOL_LIST_PAGES: usize = 100;
const MAX_DISCOVERED_TOOLS: usize = 1_000;
const MAX_DISCOVERED_TOOL_BYTES: usize = 1024 * 1024;
const DEFAULT_TASK_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_TASK_POLL_INTERVAL: Duration = Duration::from_secs(30);

pub(super) fn initialize_params(protocol_version: &str) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "roots": {}
        },
        "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
    })
}

/// Builds a JSON-RPC response for a `roots/list` request from the server.
///
/// Returns the given workspace root as the sole entry. The caller must pass
/// the session-level project root — not the process cwd — so that a
/// filesystem server resolves the correct project when the shell's cwd
/// differs from the cosh-core process directory.
pub(super) fn roots_response(id: &Value, workspace_root: &std::path::Path) -> Value {
    debug_assert!(
        workspace_root.is_absolute(),
        "workspace_root passed to roots_response must be absolute"
    );

    // Use Url::from_file_path for correct percent-encoding of special characters,
    // matching copilot-shell's pathToFileURL() behaviour. Callers now absolutize
    // via CliArgs::workspace_root(), so this conversion is expected to succeed.
    let uri = reqwest::Url::from_file_path(workspace_root)
        .expect("absolute workspace_root should produce a valid file:// URI")
        .to_string();
    let name = workspace_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "roots": [{ "uri": uri, "name": name }] }
    })
}

pub(super) fn validate_http_endpoint(endpoint: &str) -> Result<reqwest::Url, String> {
    let endpoint = reqwest::Url::parse(endpoint)
        .map_err(|error| format!("invalid MCP HTTP endpoint: {error}"))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err("MCP HTTP endpoint must use http or https".to_string());
    }
    let loopback = matches!(
        endpoint.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if endpoint.scheme() == "http" && !loopback {
        return Err("MCP HTTP endpoint must use HTTPS unless it is a loopback URL".to_string());
    }
    Ok(endpoint)
}

/// Runs an explicit MCP management command without starting the agent runtime.
pub(crate) async fn run_command(
    args: McpArgs,
    config: &CoreConfig,
    workspace_root: &std::path::Path,
) -> Result<(), String> {
    match args.command {
        McpCommand::List => print_server_list(config),
        McpCommand::Connect { server } => {
            let inspection =
                inspect_server(&server, config, "connected", true, workspace_root).await?;
            state::remove_disabled(MCP_SERVERS_STATE, &server)?;
            print_json(&inspection)
        }
        McpCommand::Inspect { server } => {
            let inspection =
                inspect_server(&server, config, "inspected", false, workspace_root).await?;
            print_json(&inspection)
        }
        McpCommand::Refresh { server } => {
            let inspection =
                inspect_server(&server, config, "refreshed", false, workspace_root).await?;
            print_json(&inspection)
        }
        McpCommand::Disconnect { server } => {
            configured_server(config, &server)?;
            state::add_disabled(MCP_SERVERS_STATE, &server)?;
            let credentials_removed = oauth::remove_credentials(&server)?;
            print_json(&McpDisconnectResult {
                server,
                disabled: true,
                credentials_removed,
            })
        }
        McpCommand::Login { server, manual } => {
            let server_config = configured_server(config, &server)?;
            oauth::login(&server, server_config, manual).await
        }
        McpCommand::Logout { server } => oauth::logout(&server),
    }
}

#[derive(Debug, Serialize)]
struct McpServerStatus {
    server: String,
    transport: &'static str,
    enabled: bool,
    has_credentials: bool,
}

#[derive(Debug, Serialize)]
struct McpToolStatus {
    name: String,
    exposed_name: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct McpServerInspection {
    server: String,
    action: &'static str,
    transport: &'static str,
    tools: Vec<McpToolStatus>,
}

#[derive(Debug, Serialize)]
struct McpDisconnectResult {
    server: String,
    disabled: bool,
    credentials_removed: bool,
}

fn configured_server<'a>(
    config: &'a CoreConfig,
    server: &str,
) -> Result<&'a McpServerConfig, String> {
    config
        .mcp
        .servers
        .get(server)
        .ok_or_else(|| format!("MCP server '{server}' is not configured"))
}

fn transport(config: &McpServerConfig) -> &'static str {
    if config.url.is_some() {
        "streamable_http"
    } else {
        "stdio"
    }
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    let output = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to serialize MCP status: {error}"))?;
    println!("{output}");
    Ok(())
}

fn print_server_list(config: &CoreConfig) -> Result<(), String> {
    let disabled = state::load_disabled(MCP_SERVERS_STATE);
    let mut servers = Vec::new();
    for (server, server_config) in &config.mcp.servers {
        servers.push(McpServerStatus {
            server: server.clone(),
            transport: transport(server_config),
            enabled: !disabled.contains(server),
            has_credentials: server_config.bearer_token.is_some()
                || oauth::has_credentials(server)?,
        });
    }
    servers.sort_by(|left, right| left.server.cmp(&right.server));
    print_json(&servers)
}

async fn inspect_server(
    server: &str,
    config: &CoreConfig,
    action: &'static str,
    allow_disconnected: bool,
    workspace_root: &std::path::Path,
) -> Result<McpServerInspection, String> {
    let server_config = configured_server(config, server)?;
    if !allow_disconnected && state::load_disabled(MCP_SERVERS_STATE).contains(server) {
        return Err(format!(
            "MCP server '{server}' is disconnected; run 'cosh-core mcp connect {server}'"
        ));
    }
    let (client, tools) = McpClient::connect(server, server_config, workspace_root).await?;
    let tools = tools
        .into_iter()
        .filter(|tool| tool_is_allowed(&tool.name, server_config.allowed_tools.as_deref()))
        .map(|tool| McpToolStatus {
            exposed_name: exposed_tool_name(server, &tool.name),
            name: tool.name,
            description: tool.description,
        })
        .collect();
    client.close().await?;
    Ok(McpServerInspection {
        server: server.to_string(),
        action,
        transport: transport(server_config),
        tools,
    })
}

/// Connects to every configured trusted server and registers its discovered tools.
///
/// A failed server is logged and skipped so one optional integration does not stop
/// the agent from starting.
pub async fn register_configured_tools(
    registry: &mut ToolRegistry,
    servers: &HashMap<String, McpServerConfig>,
    workspace_root: &std::path::Path,
) {
    let disabled = state::load_disabled(MCP_SERVERS_STATE);
    let mut names: Vec<_> = servers.keys().collect();
    names.sort();

    for server_name in names {
        if disabled.contains(server_name) {
            tracing::info!(server = server_name, "MCP server is disconnected");
            continue;
        }
        let Some(config) = servers.get(server_name) else {
            continue;
        };
        match McpClient::connect(server_name, config, workspace_root).await {
            Ok((client, tools)) => {
                let client = Arc::new(client);
                for tool in tools {
                    if !tool_is_allowed(&tool.name, config.allowed_tools.as_deref()) {
                        tracing::info!(
                            server = server_name,
                            tool = tool.name,
                            "MCP tool excluded by allowlist"
                        );
                        continue;
                    }

                    let exposed_name = exposed_tool_name(server_name, &tool.name);
                    if registry.contains(&exposed_name) {
                        tracing::warn!(
                            server = server_name,
                            tool = tool.name,
                            exposed_name,
                            "skipping MCP tool because its exposed name collides"
                        );
                        continue;
                    }
                    registry.register(Box::new(McpTool {
                        exposed_name,
                        remote_name: tool.name,
                        description: tool.description,
                        input_schema: tool.input_schema,
                        task_required: tool.task_required,
                        client: Arc::clone(&client),
                    }));
                }
            }
            Err(error) => {
                tracing::warn!(server = server_name, %error, "failed to start MCP server");
            }
        }
    }
}

fn tool_is_allowed(tool_name: &str, allowed_tools: Option<&[String]>) -> bool {
    allowed_tools
        .map(|tools| tools.iter().any(|allowed| allowed == tool_name))
        .unwrap_or(true)
}

fn exposed_tool_name(server_name: &str, tool_name: &str) -> String {
    let mut name = format!(
        "mcp__{}__{}",
        sanitize_identifier(server_name),
        sanitize_identifier(tool_name)
    );
    if name.len() > MAX_TOOL_NAME_LEN {
        name.truncate(MAX_TOOL_NAME_LEN);
    }
    name
}

fn sanitize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}

#[derive(Debug)]
struct DiscoveredTool {
    name: String,
    description: String,
    input_schema: Value,
    task_required: bool,
}

struct McpTool {
    exposed_name: String,
    remote_name: String,
    description: String,
    input_schema: Value,
    task_required: bool,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mcp
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        if self.task_required {
            self.client.call_task_tool(&self.remote_name, params).await
        } else {
            self.client.call_tool(&self.remote_name, params).await
        }
    }
}

struct McpClient {
    server_name: String,
    timeout: Duration,
    startup_timeout: Duration,
    protocol_version: Mutex<String>,
    supports_tool_tasks: Mutex<bool>,
    connection: Mutex<McpConnection>,
}

impl McpClient {
    async fn connect(
        server_name: &str,
        config: &McpServerConfig,
        workspace_root: &std::path::Path,
    ) -> Result<(Self, Vec<DiscoveredTool>), String> {
        if config.timeout_ms == 0 {
            return Err("MCP timeout_ms must be greater than zero".to_string());
        }
        if config.startup_timeout_ms == 0 {
            return Err("MCP startup_timeout_ms must be greater than zero".to_string());
        }

        let (connection, protocol_version) = match (&config.url, config.command.trim().is_empty()) {
            (Some(_), false) => {
                return Err(
                    "MCP server config must specify either url or command, not both".to_string(),
                )
            }
            (Some(url), true) => (
                McpConnection::Http(http::HttpConnection::new(
                    server_name,
                    url,
                    config.bearer_token.as_deref(),
                    config.oauth.resource.as_deref(),
                    workspace_root.to_path_buf(),
                )?),
                MCP_PROTOCOL_VERSION,
            ),
            (None, true) => return Err("MCP command or url must be configured".to_string()),
            (None, false) => (
                McpConnection::Stdio(
                    StdioConnection::spawn(config, workspace_root.to_path_buf()).await?,
                ),
                MCP_PROTOCOL_VERSION,
            ),
        };
        let client = Self {
            server_name: server_name.to_string(),
            timeout: Duration::from_millis(config.timeout_ms),
            startup_timeout: Duration::from_millis(config.startup_timeout_ms),
            protocol_version: Mutex::new(protocol_version.to_string()),
            supports_tool_tasks: Mutex::new(false),
            connection: Mutex::new(connection),
        };

        if let Err(error) = client.initialize().await {
            if error != http::LEGACY_FALLBACK_ERROR {
                return Err(error);
            }
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| "MCP legacy fallback requires an HTTP endpoint".to_string())?;
            let legacy = http::LegacyHttpConnection::connect(
                server_name,
                url,
                config.bearer_token.as_deref(),
                config.oauth.resource.as_deref(),
                workspace_root.to_path_buf(),
            )
            .await?;
            *client.connection.lock().await = McpConnection::LegacyHttp(legacy);
            *client.protocol_version.lock().await = "2024-11-05".to_string();
            client.initialize().await?;
        }
        let tools = timeout(client.startup_timeout, client.list_tools())
            .await
            .map_err(|_| {
                format!(
                    "MCP tool discovery timed out for server {}",
                    client.server_name
                )
            })??;
        Ok((client, tools))
    }

    async fn initialize(&self) -> Result<(), String> {
        let requested_protocol_version = self.protocol_version().await;
        let result = self
            .request_once_with_timeout(
                self.startup_timeout,
                "initialize",
                initialize_params(&requested_protocol_version),
            )
            .await?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| "MCP initialize response has no protocolVersion".to_string())?;
        if !supports_protocol_version(&requested_protocol_version, protocol_version) {
            return Err(format!(
                "MCP server {} negotiated unsupported protocol version {protocol_version}",
                self.server_name
            ));
        }
        *self.protocol_version.lock().await = protocol_version.to_string();
        if result.pointer("/capabilities/tools").is_none() {
            return Err(format!(
                "MCP server {} does not advertise tools capability",
                self.server_name
            ));
        }
        *self.supports_tool_tasks.lock().await =
            server_supports_tool_tasks(protocol_version, &result);
        self.notify_once("notifications/initialized", json!({}))
            .await
    }

    async fn list_tools(&self) -> Result<Vec<DiscoveredTool>, String> {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut tools = Vec::new();
        let mut tool_bytes = 0;

        for _ in 0..MAX_TOOL_LIST_PAGES {
            let mut params = serde_json::Map::new();
            if let Some(ref cursor) = cursor {
                params.insert("cursor".to_string(), Value::String(cursor.clone()));
            }
            let result = self
                .request_with_timeout(self.startup_timeout, "tools/list", Value::Object(params))
                .await?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| "MCP tools/list response has no tools array".to_string())?;

            for raw_tool in page {
                let serialized = serde_json::to_vec(raw_tool)
                    .map_err(|error| format!("failed to measure MCP tools/list item: {error}"))?;
                tool_bytes += serialized.len();
                if tool_bytes > MAX_DISCOVERED_TOOL_BYTES {
                    return Err(format!(
                        "MCP tools/list exceeded maximum total size of {MAX_DISCOVERED_TOOL_BYTES} bytes"
                    ));
                }
                if tools.len() == MAX_DISCOVERED_TOOLS {
                    return Err(format!(
                        "MCP tools/list exceeded maximum tool count of {MAX_DISCOVERED_TOOLS}"
                    ));
                }
                let name = raw_tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| "MCP tools/list item has no name".to_string())?;
                let description = raw_tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("No description provided by MCP server.");
                let input_schema = raw_tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                let task_required = tool_requires_task(raw_tool);
                tools.push(DiscoveredTool {
                    name: name.to_string(),
                    description: format!("{description} (from {} MCP server)", self.server_name),
                    input_schema,
                    task_required,
                });
            }

            let next_cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(ToString::to_string);
            match next_cursor {
                Some(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                    cursor = Some(next_cursor);
                }
                Some(next_cursor) => {
                    return Err(format!(
                        "MCP tools/list returned repeated cursor: {next_cursor}"
                    ));
                }
                None => return Ok(tools),
            }
        }

        Err(format!(
            "MCP tools/list exceeded maximum page count of {MAX_TOOL_LIST_PAGES}"
        ))
    }

    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": tool_name, "arguments": arguments }),
            )
            .await?;
        Ok(format_tool_result(&result))
    }

    async fn call_task_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolResult, String> {
        if !*self.supports_tool_tasks.lock().await {
            return Err(format!(
                "MCP tool '{tool_name}' requires task execution, but server {} does not support \
                 task-augmented tools/call in the negotiated MCP session",
                self.server_name
            ));
        }

        let result = self
            .request(
                "tools/call",
                json!({ "name": tool_name, "arguments": arguments, "task": {} }),
            )
            .await?;
        let task_id = result
            .pointer("/task/taskId")
            .and_then(Value::as_str)
            .filter(|task_id| !task_id.is_empty())
            .ok_or_else(|| {
                format!(
                    "MCP task response for tool '{tool_name}' from server {} has no task.taskId",
                    self.server_name
                )
            })?
            .to_string();
        self.wait_for_task(&task_id, &result["task"], tool_name)
            .await?;
        let result = self
            .request("tasks/result", json!({ "taskId": task_id }))
            .await?;
        Ok(format_tool_result(&result))
    }

    async fn wait_for_task(
        &self,
        task_id: &str,
        initial_task: &Value,
        tool_name: &str,
    ) -> Result<(), String> {
        let ttl = match initial_task.get("ttl") {
            Some(Value::Null) => None,
            Some(ttl) => Some(Duration::from_millis(ttl.as_u64().ok_or_else(|| {
                format!(
                    "MCP task '{task_id}' for tool '{tool_name}' from server {} has invalid ttl",
                    self.server_name
                )
            })?)),
            None => {
                return Err(format!(
                    "MCP task '{task_id}' for tool '{tool_name}' from server {} has no ttl",
                    self.server_name
                ));
            }
        };
        // Measure from receipt to avoid relying on clock synchronization with the server.
        let deadline = match ttl {
            Some(ttl) => Some(Instant::now().checked_add(ttl).ok_or_else(|| {
                format!(
                    "MCP task '{task_id}' for tool '{tool_name}' from server {} has ttl too large",
                    self.server_name
                )
            })?),
            None => None,
        };
        let ttl_expired = || {
            format!(
                "MCP task '{task_id}' for tool '{tool_name}' on server {} exceeded its ttl",
                self.server_name
            )
        };
        let mut task = initial_task.clone();
        loop {
            let status = task.get("status").and_then(Value::as_str).ok_or_else(|| {
                format!(
                    "MCP task '{task_id}' for tool '{tool_name}' from server {} has no status",
                    self.server_name
                )
            })?;
            match status {
                "completed" => return Ok(()),
                "working" => {}
                "failed" | "cancelled" | "input_required" => {
                    let message = task
                        .get("statusMessage")
                        .and_then(Value::as_str)
                        .filter(|message| !message.is_empty())
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default();
                    return Err(format!(
                        "MCP task '{task_id}' for tool '{tool_name}' on server {} reached status \
                         '{status}'{message}",
                        self.server_name
                    ));
                }
                other => {
                    return Err(format!(
                        "MCP task '{task_id}' for tool '{tool_name}' from server {} has unknown \
                         status '{other}'",
                        self.server_name
                    ));
                }
            }

            let poll_interval = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(ttl_expired());
                    }
                    task_poll_interval(&task).min(remaining)
                }
                None => task_poll_interval(&task),
            };
            sleep(poll_interval).await;
            let request = self.request("tasks/get", json!({ "taskId": task_id }));
            task = match deadline {
                Some(deadline) => timeout_at(deadline, request)
                    .await
                    .map_err(|_| ttl_expired())??,
                None => request.await?,
            };
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(self.timeout, method, params)
            .await
    }

    async fn request_with_timeout(
        &self,
        request_timeout: Duration,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let result = self
            .request_once_with_timeout(request_timeout, method, params.clone())
            .await;
        if matches!(&result, Err(error) if error == http::SESSION_EXPIRED_ERROR) {
            self.initialize().await?;
            return self
                .request_once_with_timeout(request_timeout, method, params)
                .await;
        }
        result
    }

    async fn request_once_with_timeout(
        &self,
        request_timeout: Duration,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let protocol_version = self.protocol_version().await;
        let mut connection = self.connection.lock().await;
        timeout(
            request_timeout,
            connection.request(&protocol_version, method, params),
        )
        .await
        .map_err(|_| {
            format!(
                "MCP request '{method}' timed out for server {}",
                self.server_name
            )
        })?
    }

    async fn notify_once(&self, method: &str, params: Value) -> Result<(), String> {
        let protocol_version = self.protocol_version().await;
        let mut connection = self.connection.lock().await;
        timeout(
            self.timeout,
            connection.notify(&protocol_version, method, params),
        )
        .await
        .map_err(|_| {
            format!(
                "MCP notification '{method}' timed out for server {}",
                self.server_name
            )
        })?
    }

    async fn protocol_version(&self) -> String {
        self.protocol_version.lock().await.clone()
    }

    async fn close(&self) -> Result<(), String> {
        let protocol_version = self.protocol_version().await;
        let mut connection = self.connection.lock().await;
        timeout(self.timeout, connection.close(&protocol_version))
            .await
            .map_err(|_| {
                format!(
                    "MCP session close timed out for server {}",
                    self.server_name
                )
            })?
    }
}

fn tool_requires_task(tool: &Value) -> bool {
    tool.pointer("/execution/taskSupport")
        .and_then(Value::as_str)
        == Some("required")
}

fn server_supports_tool_tasks(protocol_version: &str, initialize_result: &Value) -> bool {
    protocol_version == MCP_PROTOCOL_VERSION
        && initialize_result
            .pointer("/capabilities/tasks/requests/tools/call")
            .is_some_and(Value::is_object)
}

fn task_poll_interval(task: &Value) -> Duration {
    task.get("pollInterval")
        .and_then(Value::as_u64)
        .filter(|interval| *interval > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TASK_POLL_INTERVAL)
        .min(MAX_TASK_POLL_INTERVAL)
}

fn supports_protocol_version(requested: &str, negotiated: &str) -> bool {
    negotiated == requested
        || (requested == MCP_PROTOCOL_VERSION
            && MCP_COMPATIBLE_PROTOCOL_VERSIONS.contains(&negotiated))
}

enum McpConnection {
    Stdio(StdioConnection),
    Http(http::HttpConnection),
    LegacyHttp(http::LegacyHttpConnection),
}

impl McpConnection {
    async fn request(
        &mut self,
        protocol_version: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match self {
            Self::Stdio(connection) => connection.request(method, params).await,
            Self::Http(connection) => connection.request(protocol_version, method, params).await,
            Self::LegacyHttp(connection) => connection.request(method, params).await,
        }
    }

    async fn notify(
        &mut self,
        protocol_version: &str,
        method: &str,
        params: Value,
    ) -> Result<(), String> {
        match self {
            Self::Stdio(connection) => connection.notify(method, params).await,
            Self::Http(connection) => connection.notify(protocol_version, method, params).await,
            Self::LegacyHttp(connection) => connection.notify(method, params).await,
        }
    }

    async fn close(&mut self, protocol_version: &str) -> Result<(), String> {
        match self {
            Self::Stdio(_) => Ok(()),
            Self::Http(connection) => connection.close(protocol_version).await,
            Self::LegacyHttp(_) => Ok(()),
        }
    }
}

struct StdioConnection {
    // Keep the process handle alive for the lifetime of its stdio connection.
    #[allow(dead_code)]
    child: tokio::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: u64,
    workspace_root: std::path::PathBuf,
}

impl StdioConnection {
    async fn spawn(
        config: &McpServerConfig,
        workspace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        let executable = expand_env_vars(&config.command);
        let args: Vec<_> = config.args.iter().map(|arg| expand_env_vars(arg)).collect();
        let mut command = Command::new(&executable);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        configure_child_environment(&mut command, &config.env)?;

        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start MCP command '{executable}': {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to capture MCP process stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture MCP process stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
            workspace_root,
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let line = self
                .read_message()
                .await?
                .ok_or_else(|| "MCP server closed stdout before responding".to_string())?;
            let message: Value = serde_json::from_slice(&line)
                .map_err(|error| format!("MCP server emitted invalid JSON-RPC: {error}"))?;

            let messages = match message {
                Value::Array(messages) => messages,
                message => vec![message],
            };
            for message in messages {
                if message.get("method").is_some() && message.get("id").is_some() {
                    self.respond_to_server_request(message).await?;
                    continue;
                }
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    let description = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown MCP server error");
                    return Err(format!("MCP request '{method}' failed: {description}"));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| format!("MCP response to '{method}' has no result"));
            }
        }
    }

    /// Reads one newline-delimited MCP message without allowing a server to grow memory without
    /// bound before JSON parsing begins.
    async fn read_message(&mut self) -> Result<Option<Vec<u8>>, String> {
        let mut line = Vec::new();

        loop {
            let (consumed, has_newline) = {
                let buffer = self
                    .stdout
                    .fill_buf()
                    .await
                    .map_err(|error| format!("failed to read MCP response: {error}"))?;
                if buffer.is_empty() {
                    return if line.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(line))
                    };
                }

                let take = buffer
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(buffer.len(), |position| position + 1);
                if line.len() + take > MAX_MCP_MESSAGE_BYTES {
                    return Err(format!(
                        "MCP message exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
                    ));
                }
                line.extend_from_slice(&buffer[..take]);
                (take, line.last() == Some(&b'\n'))
            };
            self.stdout.consume(consumed);
            if has_newline {
                line.pop();
                return Ok(Some(line));
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn respond_to_server_request(&mut self, message: Value) -> Result<(), String> {
        let id = message
            .get("id")
            .cloned()
            .ok_or_else(|| "MCP server request has no id".to_string())?;
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| "MCP server request has no method".to_string())?;
        let response = match method {
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "roots/list" => roots_response(&id, &self.workspace_root),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            }),
        };
        self.write_message(response).await
    }

    async fn write_message(&mut self, message: Value) -> Result<(), String> {
        let mut encoded = serde_json::to_vec(&message)
            .map_err(|error| format!("failed to encode MCP request: {error}"))?;
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .await
            .map_err(|error| format!("failed to write MCP request: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("failed to flush MCP request: {error}"))
    }
}

fn configure_child_environment(
    command: &mut Command,
    configured_env: &HashMap<String, String>,
) -> Result<(), String> {
    command.env_clear();
    for name in ["HOME", "PATH", "TMPDIR", "LANG"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in configured_env {
        if !crate::config::is_valid_env_name(name) {
            return Err(format!("invalid MCP environment variable name: {name}"));
        }
        command.env(name, expand_env_vars(value));
    }
    Ok(())
}

fn expand_env_vars(value: &str) -> String {
    let mut expanded = value.to_string();
    while let Some(start) = expanded.find("${") {
        let Some(end_offset) = expanded[start..].find('}') else {
            break;
        };
        let end = start + end_offset;
        let variable = &expanded[start + 2..end];
        let replacement = std::env::var(variable).unwrap_or_default();
        expanded.replace_range(start..=end, &replacement);
    }
    expanded
}

fn format_tool_result(result: &Value) -> ToolResult {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut parts = Vec::new();

    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("image") => {
                    let mime_type = block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    parts.push(format!("[MCP image result: {mime_type}]"));
                }
                Some("audio") => {
                    let mime_type = block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    parts.push(format!("[MCP audio result: {mime_type}]"));
                }
                Some("resource_link") => {
                    let title = block
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("resource");
                    let uri = block
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown URI");
                    parts.push(format!("[MCP resource: {title} at {uri}]"));
                }
                Some("resource") => {
                    let resource = block.get("resource").unwrap_or(block);
                    if let Some(text) = resource.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    } else {
                        let mime_type = resource
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        parts.push(format!("[MCP embedded resource: {mime_type}]"));
                    }
                }
                _ => parts.push(block.to_string()),
            }
        }
    }
    if let Some(structured) = result.get("structuredContent") {
        parts.push(structured.to_string());
    }
    if parts.is_empty() {
        parts.push(result.to_string());
    }

    ToolResult {
        output: truncate_output(parts.join("\n")),
        is_error,
    }
}

fn truncate_output(output: String) -> String {
    if output.len() <= MAX_TOOL_OUTPUT_BYTES {
        return output;
    }

    let mut end = MAX_TOOL_OUTPUT_BYTES;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[MCP output truncated]", &output[..end])
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::*;

    fn test_context() -> ToolContext {
        ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        )
    }

    fn fake_server(script: &str) -> (tempfile::TempDir, McpServerConfig) {
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("fake-mcp.sh");
        std::fs::write(&script_path, script).unwrap();
        let config = McpServerConfig {
            command: "sh".to_string(),
            url: None,
            args: vec![script_path.to_string_lossy().to_string()],
            env: HashMap::new(),
            bearer_token: None,
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        (dir, config)
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn exposes_valid_provider_tool_name() {
        assert_eq!(
            exposed_tool_name("github tools", "read/issue"),
            "mcp__github_tools__read_issue"
        );
    }

    #[test]
    fn accepts_known_protocol_versions() {
        for version in MCP_COMPATIBLE_PROTOCOL_VERSIONS {
            assert!(supports_protocol_version(MCP_PROTOCOL_VERSION, version));
        }
        assert!(!supports_protocol_version(
            MCP_PROTOCOL_VERSION,
            "2099-01-01"
        ));
        assert!(supports_protocol_version(
            MCP_PROTOCOL_VERSION,
            "2024-11-05"
        ));
    }

    #[test]
    fn initialize_params_include_required_client_fields() {
        let params = initialize_params(MCP_PROTOCOL_VERSION);
        assert_eq!(
            params["protocolVersion"],
            Value::String(MCP_PROTOCOL_VERSION.to_string())
        );
        assert!(params["capabilities"].is_object());
        assert!(params["capabilities"]["roots"].is_object());
        assert!(params["capabilities"].get("tasks").is_none());
        assert!(params["capabilities"].get("taskSupport").is_none());
        assert!(params["clientInfo"]["name"].is_string());
        assert!(params["clientInfo"]["version"].is_string());
    }

    #[test]
    fn only_required_tools_use_tasks() {
        assert!(tool_requires_task(&json!({
            "execution": { "taskSupport": "required" }
        })));
        for task_support in ["optional", "forbidden", "unknown"] {
            assert!(!tool_requires_task(&json!({
                "execution": { "taskSupport": task_support }
            })));
        }
        assert!(!tool_requires_task(&json!({})));
    }

    #[test]
    fn task_capability_requires_current_protocol_and_object_leaf() {
        let mut result = json!({
            "capabilities": { "tasks": { "requests": { "tools": { "call": {} } } } }
        });
        assert!(server_supports_tool_tasks(MCP_PROTOCOL_VERSION, &result));
        assert!(!server_supports_tool_tasks("2025-06-18", &result));

        for invalid_leaf in [Value::Null, json!("supported")] {
            result["capabilities"]["tasks"]["requests"]["tools"]["call"] = invalid_leaf;
            assert!(!server_supports_tool_tasks(MCP_PROTOCOL_VERSION, &result));
        }
    }

    #[test]
    fn task_poll_interval_uses_server_hint_or_safe_default() {
        assert_eq!(
            task_poll_interval(&json!({ "pollInterval": 25 })),
            Duration::from_millis(25)
        );
        assert_eq!(
            task_poll_interval(&json!({ "pollInterval": 3_600_000 })),
            MAX_TASK_POLL_INTERVAL
        );
        assert_eq!(task_poll_interval(&json!({})), DEFAULT_TASK_POLL_INTERVAL);
        assert_eq!(
            task_poll_interval(&json!({ "pollInterval": 0 })),
            DEFAULT_TASK_POLL_INTERVAL
        );
    }

    #[test]
    fn roots_response_returns_workspace_root() {
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let response = roots_response(&json!(42), &workspace);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 42);
        let roots = response["result"]["roots"]
            .as_array()
            .expect("roots should be an array");
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0]["uri"].as_str().unwrap(),
            "file:///tmp/test-workspace"
        );
        assert_eq!(roots[0]["name"].as_str().unwrap(), "test-workspace");
    }

    #[test]
    fn formats_error_result() {
        let result = format_tool_result(&json!({
            "isError": true,
            "content": [{"type": "text", "text": "not found"}]
        }));
        assert!(result.is_error);
        assert_eq!(result.output, "not found");
    }

    #[test]
    fn truncates_large_tool_output() {
        let result = format_tool_result(&json!({
            "content": [{"type": "text", "text": "x".repeat(MAX_TOOL_OUTPUT_BYTES + 1)}]
        }));
        assert!(result.output.ends_with("[MCP output truncated]"));
    }

    #[test]
    fn server_status_redacts_credentials() {
        let status = McpServerStatus {
            server: "remote".to_string(),
            transport: "streamable_http",
            enabled: true,
            has_credentials: true,
        };
        let output = serde_json::to_string(&status).unwrap();
        assert!(output.contains("has_credentials"));
        assert!(!output.contains("access_token"));
        assert!(!output.contains("refresh_token"));
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_lock,
        reason = "the process-wide test environment must remain isolated while the client connects"
    )]
    async fn lifecycle_commands_manage_server_state() {
        let _lock = crate::state::TEST_STATE_LOCK.lock().unwrap();
        let states = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _states_dir = EnvVarGuard::set("COSH_STATES_DIR", states.path());
        let _home_dir = EnvVarGuard::set("HOME", home.path());
        let (_dir, server) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}' ;;
    *'"method":"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}]}}' ;;
  esac
done
"#,
        );
        let mut config = CoreConfig::default();
        config.mcp.servers.insert("fake".to_string(), server);
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");

        let inspection = inspect_server("fake", &config, "inspected", false, &workspace)
            .await
            .unwrap();
        assert_eq!(inspection.tools.len(), 1);
        assert_eq!(inspection.tools[0].exposed_name, "mcp__fake__echo");

        run_command(
            McpArgs {
                command: McpCommand::Disconnect {
                    server: "fake".to_string(),
                },
            },
            &config,
            &workspace,
        )
        .await
        .unwrap();
        assert!(state::load_disabled(MCP_SERVERS_STATE).contains("fake"));
        assert!(
            inspect_server("fake", &config, "inspected", false, &workspace)
                .await
                .unwrap_err()
                .contains("is disconnected")
        );

        run_command(
            McpArgs {
                command: McpCommand::Connect {
                    server: "fake".to_string(),
                },
            },
            &config,
            &workspace,
        )
        .await
        .unwrap();
        assert!(!state::load_disabled(MCP_SERVERS_STATE).contains("fake"));

        let refreshed = inspect_server("fake", &config, "refreshed", false, &workspace)
            .await
            .unwrap();
        assert_eq!(refreshed.action, "refreshed");
    }

    #[tokio::test]
    async fn discovers_and_invokes_stdio_tool() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      case "$line" in
        *'"protocolVersion":"2025-11-25"'*) ;;
        *) exit 1 ;;
      esac
      printf '%s\n' '[{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0"}}}]'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '[{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}}]}}]'
      ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"task"'*) exit 1 ;;
      esac
      printf '%s\n' '[{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"called"}]}}]'
      ;;
  esac
done
"#,
        );
        let mut servers = HashMap::new();
        servers.insert("fake".to_string(), config);
        let mut registry = ToolRegistry::new();
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");

        register_configured_tools(&mut registry, &servers, &workspace).await;
        let tool = registry
            .get("mcp__fake__echo")
            .expect("discovered MCP tool");
        let result = tool
            .invoke(json!({"message": "hello"}), &test_context())
            .await
            .unwrap();
        assert_eq!(result.output, "called");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn invokes_required_tool_as_task_and_gets_result() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"task":{}'*) ;;
        *) exit 1 ;;
      esac
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"task":{"taskId":"task-1","status":"working","createdAt":"2026-01-01T00:00:00Z","lastUpdatedAt":"2026-01-01T00:00:00Z","ttl":60000,"pollInterval":1}}}'
      ;;
    *'"method":"tasks/get"'*)
      case "$line" in
        *'"taskId":"task-1"'*) ;;
        *) exit 1 ;;
      esac
      printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"taskId":"task-1","status":"completed","createdAt":"2026-01-01T00:00:00Z","lastUpdatedAt":"2026-01-01T00:00:01Z","ttl":60000,"pollInterval":1}}'
      ;;
    *'"method":"tasks/result"'*)
      case "$line" in
        *'"taskId":"task-1"'*) ;;
        *) exit 1 ;;
      esac
      printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"research complete"}]}}'
      ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();

        let discovered = tools.into_iter().next().expect("required MCP tool");
        assert!(discovered.task_required);
        let tool = McpTool {
            exposed_name: exposed_tool_name("fake", &discovered.name),
            remote_name: discovered.name,
            description: discovered.description,
            input_schema: discovered.input_schema,
            task_required: discovered.task_required,
            client: Arc::new(client),
        };
        let result = tool.invoke(json!({}), &test_context()).await.unwrap();

        assert_eq!(result.output, "research complete");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn stops_polling_when_task_ttl_expires() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"task":{"taskId":"task-1","status":"working","createdAt":"2026-01-01T00:00:00Z","lastUpdatedAt":"2026-01-01T00:00:00Z","ttl":20,"pollInterval":3600000}}}'
      ;;
    *'"method":"tasks/get"'*|*'"method":"tasks/result"'*) exit 1 ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools[0].task_required);

        let started = Instant::now();
        let error = client
            .call_task_tool("research", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("task-1"));
        assert!(error.contains("exceeded its ttl"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn rejects_required_tool_without_server_task_capability() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*) exit 1 ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools[0].task_required);

        let error = client
            .call_task_tool("research", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("requires task execution"));
        assert!(error.contains("does not support task-augmented tools/call"));
    }

    #[tokio::test]
    async fn returns_terminal_task_failure_without_fetching_result() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"task":{"taskId":"task-1","status":"failed","statusMessage":"backend unavailable","createdAt":"2026-01-01T00:00:00Z","lastUpdatedAt":"2026-01-01T00:00:01Z","ttl":60000}}}'
      ;;
    *'"method":"tasks/get"'*|*'"method":"tasks/result"'*) exit 1 ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools[0].task_required);

        let error = client
            .call_task_tool("research", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("reached status 'failed'"));
        assert!(error.contains("backend unavailable"));
    }

    #[tokio::test]
    async fn rejects_task_capability_from_older_protocol() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*) exit 1 ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools[0].task_required);

        let error = client
            .call_task_tool("research", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("requires task execution"));
        assert!(error.contains("does not support task-augmented tools/call"));
    }

    #[tokio::test]
    async fn rejects_task_creation_response_without_task_id() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"research","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"task":{"status":"working"}}}'
      ;;
  esac
done
"#,
        );
        let workspace = PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools[0].task_required);

        let error = client
            .call_task_tool("research", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("has no task.taskId"));
    }

    #[tokio::test]
    async fn answers_server_ping_while_waiting_for_stdio_response() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":"server-ping","method":"ping","params":{}}'
      IFS= read -r response
      case "$response" in
        *'"id":"server-ping"'*'"result":{}'*) ;;
        *) exit 1 ;;
      esac
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[]}}\n' "$id"
      ;;
  esac
done
"#,
        );

        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let (_, tools) = McpClient::connect("fake", &config, &workspace)
            .await
            .unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn rejects_repeated_tool_list_cursor() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[],"nextCursor":"again"}}\n' "$id"
      ;;
  esac
done
"#,
        );

        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let error = match McpClient::connect("fake", &config, &workspace).await {
            Ok(_) => panic!("repeated cursor should fail MCP tool discovery"),
            Err(error) => error,
        };
        assert!(error.contains("repeated cursor: again"));
    }

    #[tokio::test]
    async fn empty_allowlist_registers_no_tools() {
        let (_dir, mut config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}' ;;
    *'"method":"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}' ;;
  esac
done
"#,
        );
        config.allowed_tools = Some(vec![]);
        let mut registry = ToolRegistry::new();
        let mut servers = HashMap::new();
        servers.insert("fake".to_string(), config);
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");

        register_configured_tools(&mut registry, &servers, &workspace).await;
        assert!(registry.get("mcp__fake__echo").is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_mcp_message() {
        let (_dir, config) = fake_server(
            r#"#!/bin/sh
while IFS= read -r line; do
  head -c 1048577 /dev/zero | tr '\0' x
  printf '\n'
done
"#,
        );

        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let error = match McpClient::connect("fake", &config, &workspace).await {
            Ok(_) => panic!("oversized MCP message should fail"),
            Err(error) => error,
        };
        assert!(error.contains("MCP message exceeds maximum size"));
    }

    #[test]
    fn roots_response_uses_workspace_root_not_process_cwd() {
        // Regression: roots/list must reflect the session workspace root passed
        // by cosh-shell, not std::env::current_dir().
        let workspace = std::path::PathBuf::from("/tmp/provided-workspace");
        let cwd = std::env::current_dir().expect("cwd is available");
        assert_ne!(
            workspace, cwd,
            "test requires workspace to differ from the cosh-core cwd"
        );

        let response = roots_response(&json!("request-id"), &workspace);
        let roots = response["result"]["roots"].as_array().expect("roots array");
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0]["uri"].as_str().unwrap(),
            "file:///tmp/provided-workspace"
        );
        assert_eq!(roots[0]["name"].as_str().unwrap(), "provided-workspace");
    }
}
