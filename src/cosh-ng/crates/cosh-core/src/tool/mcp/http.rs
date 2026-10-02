//! Streamable HTTP transport for configured MCP servers.

use std::pin::Pin;

use futures::{Stream, StreamExt};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};

use super::{
    expand_env_vars, oauth, roots_response, validate_http_endpoint, MAX_MCP_MESSAGE_BYTES,
};

pub(super) const SESSION_EXPIRED_ERROR: &str = "MCP HTTP session expired";
pub(super) const LEGACY_FALLBACK_ERROR: &str = "MCP server requires legacy HTTP+SSE transport";

/// Stateful HTTP connection that carries the MCP session between requests.
pub(super) struct HttpConnection {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    server_name: String,
    bearer_token: Option<String>,
    oauth_credentials: Option<oauth::OAuthCredentials>,
    session_id: Option<String>,
    initialized: bool,
    next_request_id: u64,
    workspace_root: std::path::PathBuf,
}

impl HttpConnection {
    /// Creates a connection without sending traffic until MCP initialization.
    pub(super) fn new(
        server_name: &str,
        endpoint: &str,
        bearer_token: Option<&str>,
        oauth_resource: Option<&str>,
        workspace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        let endpoint = validate_http_endpoint(&expand_env_vars(endpoint))?;
        let oauth_resource = oauth_resource.map(expand_env_vars);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to build MCP HTTP client: {error}"))?;
        let oauth_credentials = if bearer_token.is_none() {
            oauth::load_credentials(server_name, &endpoint, oauth_resource.as_deref())?
        } else {
            None
        };

        Ok(Self {
            client,
            endpoint,
            server_name: server_name.to_string(),
            bearer_token: bearer_token.map(expand_env_vars),
            oauth_credentials,
            session_id: None,
            initialized: false,
            next_request_id: 1,
            workspace_root,
        })
    }

    /// Sends one JSON-RPC request and returns its matching MCP result.
    pub(super) async fn request(
        &mut self,
        protocol_version: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.post(protocol_version, method, message).await?;
        let result = self
            .read_response(response, protocol_version, id, method)
            .await?;
        if method == "initialize" {
            self.initialized = true;
        }
        Ok(result)
    }

    /// Sends an MCP notification and verifies that the server accepted it.
    pub(super) async fn notify(
        &mut self,
        protocol_version: &str,
        method: &str,
        params: Value,
    ) -> Result<(), String> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let response = self.post(protocol_version, method, message).await?;
        if !response.status().is_success() {
            return Err(format!(
                "MCP HTTP notification '{method}' failed with status {}",
                response.status()
            ));
        }
        Ok(())
    }

    async fn post(
        &mut self,
        protocol_version: &str,
        method: &str,
        message: Value,
    ) -> Result<reqwest::Response, String> {
        let mut response = self.send(protocol_version, message.clone()).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && self.bearer_token.is_none() {
            if let Some(credentials) = self.oauth_credentials.as_mut() {
                oauth::refresh_credentials(&self.server_name, credentials, true).await?;
                response = self.send(protocol_version, message).await?;
            }
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND && self.session_id.is_some() {
            self.session_id = None;
            self.initialized = false;
            return Err(SESSION_EXPIRED_ERROR.to_string());
        }
        if method == "initialize"
            && matches!(
                response.status(),
                reqwest::StatusCode::BAD_REQUEST
                    | reqwest::StatusCode::NOT_FOUND
                    | reqwest::StatusCode::METHOD_NOT_ALLOWED
            )
        {
            return Err(LEGACY_FALLBACK_ERROR.to_string());
        }
        if !response.status().is_success() {
            if response.status() == reqwest::StatusCode::FORBIDDEN
                && self.bearer_token.is_none()
                && self.oauth_credentials.is_some()
            {
                let scope_hint = oauth::challenged_scopes(response.headers())
                    .filter(|scopes| !scopes.is_empty())
                    .map(|scopes| format!(" for scopes {}", scopes.join(" ")))
                    .unwrap_or_default();
                return Err(format!(
                    "MCP HTTP request '{method}' requires additional OAuth authorization{scope_hint}; run 'cosh-core mcp login {}'",
                    self.server_name
                ));
            }
            let login_hint = if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && self.bearer_token.is_none()
                && self.oauth_credentials.is_none()
            {
                format!("; run 'cosh-core mcp login {}'", self.server_name)
            } else {
                String::new()
            };
            return Err(format!(
                "MCP HTTP request '{method}' failed with status {}{login_hint}",
                response.status()
            ));
        }
        if let Some(session_id) = response
            .headers()
            .get("MCP-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }
        Ok(response)
    }

    async fn send(
        &mut self,
        protocol_version: &str,
        message: Value,
    ) -> Result<reqwest::Response, String> {
        if let Some(credentials) = self.oauth_credentials.as_mut() {
            oauth::refresh_credentials(&self.server_name, credentials, false).await?;
        }
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .json(&message);
        if self.initialized {
            request = request.header("MCP-Protocol-Version", protocol_version);
        }
        if let Some(session_id) = &self.session_id {
            request = request.header("MCP-Session-Id", session_id);
        }
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        } else if let Some(credentials) = &self.oauth_credentials {
            request = request.bearer_auth(&credentials.access_token);
        }
        request
            .send()
            .await
            .map_err(|error| format!("MCP HTTP request failed: {error}"))
    }

    async fn read_response(
        &mut self,
        response: reqwest::Response,
        protocol_version: &str,
        id: u64,
        method: &str,
    ) -> Result<Value, String> {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            return self
                .read_sse_response(response, protocol_version, id, method)
                .await;
        }
        if !content_type.starts_with("application/json") {
            return Err(format!(
                "MCP HTTP response to '{method}' has unsupported content type '{content_type}'"
            ));
        }

        let bytes = read_limited_body(response).await?;
        let message: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("MCP HTTP response to '{method}' is invalid JSON: {error}"))?;
        extract_result(message, id, method)
    }

    /// Closes an HTTP session after a short-lived management command.
    pub(super) async fn close(&mut self, protocol_version: &str) -> Result<(), String> {
        let Some(session_id) = self.session_id.take() else {
            return Ok(());
        };
        if let Some(credentials) = self.oauth_credentials.as_mut() {
            oauth::refresh_credentials(&self.server_name, credentials, false).await?;
        }
        let mut request = self
            .client
            .delete(self.endpoint.clone())
            .header("MCP-Session-Id", session_id);
        if self.initialized {
            request = request.header("MCP-Protocol-Version", protocol_version);
        }
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        } else if let Some(credentials) = &self.oauth_credentials {
            request = request.bearer_auth(&credentials.access_token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("failed to close MCP HTTP session: {error}"))?;
        if response.status().is_success()
            || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            return Ok(());
        }
        Err(format!(
            "failed to close MCP HTTP session: server returned {}",
            response.status()
        ))
    }

    async fn read_sse_response(
        &mut self,
        response: reqwest::Response,
        protocol_version: &str,
        id: u64,
        method: &str,
    ) -> Result<Value, String> {
        let mut response = response;
        let mut last_event_id = None;
        let mut retry_delay = None;

        loop {
            let mut stream = response.bytes_stream();
            let mut line = Vec::new();
            let mut event_data = Vec::new();
            let mut event_id = None;

            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| format!("failed to read MCP SSE response: {error}"))?;
                if line.len() + chunk.len() > MAX_MCP_MESSAGE_BYTES {
                    return Err(format!(
                        "MCP SSE response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
                    ));
                }
                line.extend_from_slice(&chunk);

                while let Some(newline) = line.iter().position(|byte| *byte == b'\n') {
                    let mut event_line: Vec<u8> = line.drain(..=newline).collect();
                    event_line.pop();
                    if event_line.last() == Some(&b'\r') {
                        event_line.pop();
                    }
                    if event_line.is_empty() {
                        if let Some(event_id) = event_id.take() {
                            last_event_id = Some(event_id);
                        }
                        if event_data.is_empty() {
                            continue;
                        }
                        let data = event_data.join("\n");
                        event_data.clear();
                        let message: Value = serde_json::from_str(&data).map_err(|error| {
                            format!("MCP SSE response to '{method}' is invalid JSON: {error}")
                        })?;
                        if message.get("id").and_then(Value::as_u64) == Some(id) {
                            return extract_result(message, id, method);
                        }
                        if message.get("method").is_some() && message.get("id").is_some() {
                            self.respond_to_server_request(protocol_version, message)
                                .await?;
                        }
                        continue;
                    }
                    if let Some(event_id_value) = event_line.strip_prefix(b"id:") {
                        let event_id_value = std::str::from_utf8(
                            event_id_value.strip_prefix(b" ").unwrap_or(event_id_value),
                        )
                        .map_err(|error| format!("MCP SSE event id is not UTF-8: {error}"))?;
                        if !event_id_value.contains('\0') {
                            event_id = Some(event_id_value.to_string());
                        }
                    } else if let Some(retry) = event_line.strip_prefix(b"retry:") {
                        let retry = std::str::from_utf8(retry.strip_prefix(b" ").unwrap_or(retry))
                            .map_err(|error| {
                                format!("MCP SSE retry value is not UTF-8: {error}")
                            })?;
                        if let Ok(milliseconds) = retry.parse::<u64>() {
                            retry_delay = Some(std::time::Duration::from_millis(milliseconds));
                        }
                    } else if let Some(data) = event_line.strip_prefix(b"data:") {
                        let data = std::str::from_utf8(data.strip_prefix(b" ").unwrap_or(data))
                            .map_err(|error| format!("MCP SSE response is not UTF-8: {error}"))?;
                        if event_data.iter().map(String::len).sum::<usize>() + data.len()
                            > MAX_MCP_MESSAGE_BYTES
                        {
                            return Err(format!(
                                "MCP SSE response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
                            ));
                        }
                        event_data.push(data.to_string());
                    }
                }
            }

            let Some(event_id) = last_event_id.as_deref() else {
                return Err(format!(
                    "MCP SSE response to '{method}' ended before response id {id}"
                ));
            };
            if let Some(delay) = retry_delay {
                tokio::time::sleep(delay).await;
            }
            response = self.resume_sse(protocol_version, event_id).await?;
        }
    }

    async fn respond_to_server_request(
        &mut self,
        protocol_version: &str,
        message: Value,
    ) -> Result<(), String> {
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
        self.post(protocol_version, "server request response", response)
            .await
            .map(|_| ())
    }

    async fn resume_sse(
        &mut self,
        protocol_version: &str,
        last_event_id: &str,
    ) -> Result<reqwest::Response, String> {
        if let Some(credentials) = self.oauth_credentials.as_mut() {
            oauth::refresh_credentials(&self.server_name, credentials, false).await?;
        }
        let mut request = self
            .client
            .get(self.endpoint.clone())
            .header(ACCEPT, "text/event-stream")
            .header("Last-Event-ID", last_event_id);
        if self.initialized {
            request = request.header("MCP-Protocol-Version", protocol_version);
        }
        if let Some(session_id) = &self.session_id {
            request = request.header("MCP-Session-Id", session_id);
        }
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        } else if let Some(credentials) = &self.oauth_credentials {
            request = request.bearer_auth(&credentials.access_token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("failed to resume MCP SSE response: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "failed to resume MCP SSE response: server returned {}",
                response.status()
            ));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.starts_with("text/event-stream") {
            return Err("resumed MCP SSE response has unsupported content type".to_string());
        }
        Ok(response)
    }
}

type SseStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;

/// Deprecated HTTP+SSE transport used by MCP 2024-11-05 servers.
pub(super) struct LegacyHttpConnection {
    client: reqwest::Client,
    sse_endpoint: reqwest::Url,
    post_endpoint: reqwest::Url,
    server_name: String,
    bearer_token: Option<String>,
    oauth_credentials: Option<oauth::OAuthCredentials>,
    sse_stream: SseStream,
    line: Vec<u8>,
    event_data: Vec<String>,
    event_name: Option<String>,
    next_request_id: u64,
    workspace_root: std::path::PathBuf,
}

impl LegacyHttpConnection {
    /// Opens the legacy SSE endpoint and learns the POST endpoint from its first event.
    pub(super) async fn connect(
        server_name: &str,
        endpoint: &str,
        bearer_token: Option<&str>,
        oauth_resource: Option<&str>,
        workspace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        let sse_endpoint = validate_http_endpoint(&expand_env_vars(endpoint))?;
        let oauth_resource = oauth_resource.map(expand_env_vars);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to build MCP HTTP client: {error}"))?;
        let oauth_credentials = if bearer_token.is_none() {
            oauth::load_credentials(server_name, &sse_endpoint, oauth_resource.as_deref())?
        } else {
            None
        };
        let mut connection = Self {
            client,
            sse_endpoint: sse_endpoint.clone(),
            post_endpoint: sse_endpoint,
            server_name: server_name.to_string(),
            bearer_token: bearer_token.map(expand_env_vars),
            oauth_credentials,
            sse_stream: Box::pin(futures::stream::empty()),
            line: Vec::new(),
            event_data: Vec::new(),
            event_name: None,
            next_request_id: 1,
            workspace_root,
        };
        let response = connection.open_sse().await?;
        connection.sse_stream = Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|chunk| chunk.to_vec())
                .map_err(|error| format!("failed to read legacy MCP SSE response: {error}"))
        }));
        connection.post_endpoint = connection.read_post_endpoint().await?;
        Ok(connection)
    }

    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.post(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        loop {
            let (event, data) = self.next_event().await?;
            if event.as_deref() != Some("message") {
                continue;
            }
            let message: Value = serde_json::from_str(&data)
                .map_err(|error| format!("legacy MCP SSE message is invalid JSON: {error}"))?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return extract_result(message, id, method);
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                self.respond_to_server_request(message).await?;
            }
        }
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.post(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn open_sse(&mut self) -> Result<reqwest::Response, String> {
        self.refresh_credentials(false).await?;
        let mut response = self.send_sse_request().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            && self.bearer_token.is_none()
            && self.oauth_credentials.is_some()
        {
            self.refresh_credentials(true).await?;
            response = self.send_sse_request().await?;
        }
        if !response.status().is_success() {
            return Err(self.authentication_error(response.status(), "open legacy MCP SSE stream"));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.starts_with("text/event-stream") {
            return Err("legacy MCP SSE endpoint returned an unsupported content type".to_string());
        }
        Ok(response)
    }

    async fn send_sse_request(&mut self) -> Result<reqwest::Response, String> {
        self.authorize(
            self.client
                .get(self.sse_endpoint.clone())
                .header(ACCEPT, "text/event-stream"),
        )
        .send()
        .await
        .map_err(|error| format!("failed to open legacy MCP SSE stream: {error}"))
    }

    async fn post(&mut self, message: Value) -> Result<(), String> {
        self.refresh_credentials(false).await?;
        let mut response = self.send_post(message.clone()).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            && self.bearer_token.is_none()
            && self.oauth_credentials.is_some()
        {
            self.refresh_credentials(true).await?;
            response = self.send_post(message).await?;
        }
        if response.status().is_success() {
            return Ok(());
        }
        Err(self.authentication_error(response.status(), "send legacy MCP message"))
    }

    async fn send_post(&mut self, message: Value) -> Result<reqwest::Response, String> {
        self.authorize(
            self.client
                .post(self.post_endpoint.clone())
                .header(CONTENT_TYPE, "application/json")
                .json(&message),
        )
        .send()
        .await
        .map_err(|error| format!("failed to send legacy MCP message: {error}"))
    }

    async fn refresh_credentials(&mut self, force: bool) -> Result<(), String> {
        if let Some(credentials) = self.oauth_credentials.as_mut() {
            oauth::refresh_credentials(&self.server_name, credentials, force).await?;
        }
        Ok(())
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else if let Some(credentials) = &self.oauth_credentials {
            request.bearer_auth(&credentials.access_token)
        } else {
            request
        }
    }

    fn authentication_error(&self, status: reqwest::StatusCode, action: &str) -> String {
        let login_hint = if status == reqwest::StatusCode::UNAUTHORIZED
            && self.bearer_token.is_none()
            && self.oauth_credentials.is_none()
        {
            format!("; run 'cosh-core mcp login {}'", self.server_name)
        } else {
            String::new()
        };
        format!("failed to {action}: server returned {status}{login_hint}")
    }

    async fn read_post_endpoint(&mut self) -> Result<reqwest::Url, String> {
        loop {
            let (event, data) = self.next_event().await?;
            if event.as_deref() != Some("endpoint") {
                continue;
            }
            let endpoint = self
                .sse_endpoint
                .join(&data)
                .map_err(|error| format!("legacy MCP endpoint event is invalid: {error}"))?;
            let endpoint = validate_http_endpoint(endpoint.as_str())?;
            if !same_origin(&self.sse_endpoint, &endpoint) {
                return Err(
                    "legacy MCP endpoint event must use the SSE endpoint origin".to_string()
                );
            }
            return Ok(endpoint);
        }
    }

    async fn next_event(&mut self) -> Result<(Option<String>, String), String> {
        loop {
            let chunk = self
                .sse_stream
                .next()
                .await
                .ok_or_else(|| "legacy MCP SSE stream closed".to_string())??;
            if self.line.len() + chunk.len() > MAX_MCP_MESSAGE_BYTES {
                return Err(format!(
                    "legacy MCP SSE response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
                ));
            }
            self.line.extend_from_slice(&chunk);

            while let Some(newline) = self.line.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.line.drain(..=newline).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    let event = self.event_name.take();
                    let data = self.event_data.join("\n");
                    self.event_data.clear();
                    return Ok((event, data));
                }
                if let Some(name) = line.strip_prefix(b"event:") {
                    let name = std::str::from_utf8(name.strip_prefix(b" ").unwrap_or(name))
                        .map_err(|error| format!("legacy MCP SSE event is not UTF-8: {error}"))?;
                    self.event_name = Some(name.to_string());
                } else if let Some(data) = line.strip_prefix(b"data:") {
                    let data = std::str::from_utf8(data.strip_prefix(b" ").unwrap_or(data))
                        .map_err(|error| format!("legacy MCP SSE data is not UTF-8: {error}"))?;
                    if self.event_data.iter().map(String::len).sum::<usize>() + data.len()
                        > MAX_MCP_MESSAGE_BYTES
                    {
                        return Err(format!(
                            "legacy MCP SSE response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
                        ));
                    }
                    self.event_data.push(data.to_string());
                }
            }
        }
    }

    async fn respond_to_server_request(&mut self, message: Value) -> Result<(), String> {
        let id = message
            .get("id")
            .cloned()
            .ok_or_else(|| "legacy MCP server request has no id".to_string())?;
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| "legacy MCP server request has no method".to_string())?;
        let response = match method {
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "roots/list" => roots_response(&id, &self.workspace_root),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            }),
        };
        self.post(response).await
    }
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

async fn read_limited_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MCP_MESSAGE_BYTES as u64)
    {
        return Err(format!(
            "MCP HTTP response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
        ));
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("failed to read MCP HTTP response: {error}"))?;
        if body.len() + chunk.len() > MAX_MCP_MESSAGE_BYTES {
            return Err(format!(
                "MCP HTTP response exceeds maximum size of {MAX_MCP_MESSAGE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn extract_result(message: Value, id: u64, method: &str) -> Result<Value, String> {
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(format!(
            "MCP HTTP response to '{method}' has unexpected request id"
        ));
    }
    if let Some(error) = message.get("error") {
        let description = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown MCP server error");
        return Err(format!("MCP request '{method}' failed: {description}"));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| format!("MCP response to '{method}' has no result"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::TcpListener;

    use super::super::McpClient;
    use crate::config::McpServerConfig;

    async fn read_request(
        reader: &mut BufReader<OwnedReadHalf>,
    ) -> (String, HashMap<String, String>, Option<Value>) {
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await.unwrap();
        let method = request_line.split_whitespace().next().unwrap().to_string();

        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(':').unwrap();
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
        let length = headers
            .get("content-length")
            .map_or(0, |length| length.parse::<usize>().unwrap());
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.unwrap();
        let body = (!body.is_empty()).then(|| serde_json::from_slice(&body).unwrap());
        (method, headers, body)
    }

    async fn write_response(
        writer: &mut OwnedWriteHalf,
        status: &str,
        content_type: Option<&str>,
        session_id: Option<&str>,
        body: &str,
    ) {
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
        if let Some(content_type) = content_type {
            response.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        if let Some(session_id) = session_id {
            response.push_str(&format!("MCP-Session-Id: {session_id}\r\n"));
        }
        response.push_str("Connection: keep-alive\r\n\r\n");
        writer.write_all(response.as_bytes()).await.unwrap();
        writer.write_all(body.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn write_sse_chunk(writer: &mut OwnedWriteHalf, event: &str) {
        writer
            .write_all(format!("{:X}\r\n{event}\r\n", event.len()).as_bytes())
            .await
            .unwrap();
        writer.flush().await.unwrap();
    }

    #[test]
    fn rejects_non_http_endpoint() {
        let error = match HttpConnection::new(
            "test",
            "file:///tmp/server",
            None,
            None,
            std::path::PathBuf::from("/tmp/workspace"),
        ) {
            Ok(_) => panic!("non-HTTP endpoint should fail"),
            Err(error) => error,
        };
        assert!(error.contains("must use http or https"));
    }

    #[test]
    fn rejects_insecure_remote_endpoint() {
        let error = match HttpConnection::new(
            "test",
            "http://mcp.example.com/mcp",
            None,
            None,
            std::path::PathBuf::from("/tmp/workspace"),
        ) {
            Ok(_) => panic!("insecure remote endpoint should fail"),
            Err(error) => error,
        };
        assert!(error.contains("must use HTTPS unless it is a loopback URL"));
    }

    #[test]
    fn checks_matching_json_rpc_id() {
        let error = extract_result(json!({"id": 2, "result": {}}), 1, "tools/list").unwrap_err();
        assert!(error.contains("unexpected request id"));
    }

    #[test]
    fn returns_json_rpc_errors() {
        let error = extract_result(
            json!({"id": 1, "error": {"message": "access denied"}}),
            1,
            "tools/list",
        )
        .unwrap_err();
        assert!(error.contains("access denied"));
    }

    #[tokio::test]
    async fn does_not_follow_mcp_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let (_, _, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "initialize");
            let response = "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            write_half.write_all(response.as_bytes()).await.unwrap();
            write_half.flush().await.unwrap();
        });

        let config = McpServerConfig {
            command: String::new(),
            url: Some(format!("http://{address}/mcp")),
            args: Vec::new(),
            env: HashMap::new(),
            bearer_token: Some("test-token".to_string()),
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let error = match McpClient::connect("test", &config, &workspace).await {
            Ok(_) => panic!("redirected MCP endpoint should fail"),
            Err(error) => error,
        };
        assert!(error.contains("failed with status 307"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovers_and_calls_tools_over_streamable_http() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "initialize");
            assert_eq!(message["params"]["protocolVersion"], "2025-11-25");
            assert_eq!(headers["authorization"], "Bearer test-token");
            assert!(!headers.contains_key("mcp-protocol-version"));
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                Some("test-session"),
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}"#,
            )
            .await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "notifications/initialized");
            assert_eq!(headers["mcp-session-id"], "test-session");
            assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
            write_response(&mut write_half, "202 Accepted", None, None, "").await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "tools/list");
            assert_eq!(headers["mcp-session-id"], "test-session");
            assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
            let event = r#"data: {"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}

"#;
            write_response(
                &mut write_half,
                "200 OK",
                Some("text/event-stream"),
                None,
                event,
            )
            .await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "tools/call");
            assert_eq!(message["params"]["name"], "echo");
            assert_eq!(headers["mcp-session-id"], "test-session");
            write_response(&mut write_half, "404 Not Found", None, None, "").await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "initialize");
            assert_eq!(message["params"]["protocolVersion"], "2025-06-18");
            assert!(!headers.contains_key("mcp-session-id"));
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                Some("test-session-2"),
                r#"{"jsonrpc":"2.0","id":4,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}"#,
            )
            .await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "notifications/initialized");
            assert_eq!(headers["mcp-session-id"], "test-session-2");
            assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
            write_response(&mut write_half, "202 Accepted", None, None, "").await;

            let (_, headers, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["method"], "tools/call");
            assert_eq!(message["params"]["name"], "echo");
            assert_eq!(headers["mcp-session-id"], "test-session-2");
            assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                None,
                r#"{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"called"}]}}"#,
            )
            .await;
        });

        let config = McpServerConfig {
            command: String::new(),
            url: Some(format!("http://{address}/mcp")),
            args: Vec::new(),
            env: HashMap::new(),
            bearer_token: Some("test-token".to_string()),
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let (client, tools) = McpClient::connect("test", &config, &workspace)
            .await
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            client.call_tool("echo", json!({})).await.unwrap().output,
            "called"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn answers_ping_and_resumes_sse_by_event_id() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "initialize");
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                Some("test-session"),
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}}}}"#,
            )
            .await;

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "notifications/initialized");
            write_response(&mut write_half, "202 Accepted", None, None, "").await;

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "tools/list");
            let event = concat!(
                "id: stream-1\n",
                "retry: 0\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\",\"params\":{}}\n\n"
            );
            write_response(
                &mut write_half,
                "200 OK",
                Some("text/event-stream"),
                None,
                event,
            )
            .await;

            let (_, _, message) = read_request(&mut reader).await;
            let message = message.unwrap();
            assert_eq!(message["id"], "server-ping");
            assert_eq!(message["result"], json!({}));
            write_response(&mut write_half, "202 Accepted", None, None, "").await;

            let (request_method, headers, _) = read_request(&mut reader).await;
            assert_eq!(request_method, "GET");
            assert_eq!(headers["last-event-id"], "stream-1");
            let event = r#"data: {"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}

"#;
            write_response(
                &mut write_half,
                "200 OK",
                Some("text/event-stream"),
                None,
                event,
            )
            .await;
        });

        let config = McpServerConfig {
            command: String::new(),
            url: Some(format!("http://{address}/mcp")),
            args: Vec::new(),
            env: HashMap::new(),
            bearer_token: Some("test-token".to_string()),
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let (_, tools) = McpClient::connect("test", &config, &workspace)
            .await
            .unwrap();
        assert_eq!(tools.len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn closes_management_session_and_tolerates_unsupported_delete() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "initialize");
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                Some("test-session"),
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}}}}"#,
            )
            .await;

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "notifications/initialized");
            write_response(&mut write_half, "202 Accepted", None, None, "").await;

            let (_, _, message) = read_request(&mut reader).await;
            assert_eq!(message.unwrap()["method"], "tools/list");
            write_response(
                &mut write_half,
                "200 OK",
                Some("application/json"),
                None,
                r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#,
            )
            .await;

            let (request_method, headers, body) = read_request(&mut reader).await;
            assert_eq!(request_method, "DELETE");
            assert_eq!(headers["mcp-session-id"], "test-session");
            assert!(body.is_none());
            write_response(&mut write_half, "405 Method Not Allowed", None, None, "").await;
        });

        let config = McpServerConfig {
            command: String::new(),
            url: Some(format!("http://{address}/mcp")),
            args: Vec::new(),
            env: HashMap::new(),
            bearer_token: Some("test-token".to_string()),
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let (client, _) = McpClient::connect("test", &config, &workspace)
            .await
            .unwrap();
        client.close().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn falls_back_to_legacy_http_sse_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let (method, _, message) = read_request(&mut reader).await;
            assert_eq!(method, "POST");
            assert_eq!(message.unwrap()["method"], "initialize");
            write_response(&mut write_half, "405 Method Not Allowed", None, None, "").await;

            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut sse_writer) = stream.into_split();
            let mut sse_reader = BufReader::new(read_half);
            let (method, _, body) = read_request(&mut sse_reader).await;
            assert_eq!(method, "GET");
            assert!(body.is_none());
            sse_writer
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .unwrap();
            write_sse_chunk(&mut sse_writer, "event: endpoint\ndata: /messages\n\n").await;

            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut post_writer) = stream.into_split();
            let mut post_reader = BufReader::new(read_half);

            let (_, _, message) = read_request(&mut post_reader).await;
            assert_eq!(message.unwrap()["method"], "initialize");
            write_response(&mut post_writer, "202 Accepted", None, None, "").await;
            write_sse_chunk(
                &mut sse_writer,
                "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}}}}\n\n",
            )
            .await;

            let (_, _, message) = read_request(&mut post_reader).await;
            assert_eq!(message.unwrap()["method"], "notifications/initialized");
            write_response(&mut post_writer, "202 Accepted", None, None, "").await;

            let (_, _, message) = read_request(&mut post_reader).await;
            assert_eq!(message.unwrap()["method"], "tools/list");
            write_response(&mut post_writer, "202 Accepted", None, None, "").await;
            write_sse_chunk(
                &mut sse_writer,
                "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\",\"inputSchema\":{\"type\":\"object\"}}]}}\n\n",
            )
            .await;
        });

        let config = McpServerConfig {
            command: String::new(),
            url: Some(format!("http://{address}/mcp")),
            args: Vec::new(),
            env: HashMap::new(),
            bearer_token: Some("test-token".to_string()),
            oauth: Default::default(),
            timeout_ms: 1_000,
            startup_timeout_ms: 1_000,
            allowed_tools: None,
        };
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let (_, tools) = McpClient::connect("test", &config, &workspace)
            .await
            .unwrap();
        assert_eq!(tools.len(), 1);
        server.await.unwrap();
    }
}
