//! OAuth authorization and credential storage for Streamable HTTP MCP servers.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::config::{config_dir, McpOAuthConfig, McpServerConfig};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const EXPIRY_SKEW_SECONDS: i64 = 60;
const CREDENTIALS_FILE: &str = "mcp-oauth.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OAuthCredentials {
    pub(super) access_token: String,
    refresh_token: Option<String>,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<String>,
    resource: String,
    #[serde(default)]
    endpoint: String,
    expires_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CredentialFile {
    #[serde(default)]
    servers: HashMap<String, OAuthCredentials>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

struct AuthorizationChallenge {
    resource_metadata_url: Option<Url>,
    scopes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Starts browser-based OAuth login for one configured HTTP MCP server.
pub(super) async fn login(
    server_name: &str,
    server: &McpServerConfig,
    manual: bool,
) -> Result<(), String> {
    if !server.command.trim().is_empty() || server.url.is_none() {
        return Err("OAuth login requires a Streamable HTTP MCP server".to_string());
    }
    if server.bearer_token.is_some() {
        return Err("remove bearer_token before using MCP OAuth login".to_string());
    }
    let endpoint_url = super::expand_env_vars(server.url.as_deref().unwrap_or_default());
    let endpoint = super::validate_http_endpoint(&endpoint_url)?;

    let listener = TcpListener::bind(("127.0.0.1", server.oauth.callback_port.unwrap_or(0)))
        .await
        .map_err(|error| format!("failed to start OAuth callback listener: {error}"))?;
    let callback_url = format!(
        "http://{}/callback",
        listener.local_addr().map_err(|error| {
            format!("failed to read OAuth callback listener address: {error}")
        })?
    );
    let client = oauth_client()?;
    let discovery = discover(&client, &endpoint, &server.oauth).await?;
    if !supports_pkce_s256(discovery.code_challenge_methods_supported.as_deref()) {
        return Err("OAuth server does not support PKCE S256".to_string());
    }

    let registration = register_client(&client, &discovery, &server.oauth, &callback_url).await?;
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let state = Uuid::new_v4().to_string();
    let authorization_url = build_authorization_url(
        &discovery.authorization_endpoint,
        &registration.client_id,
        &callback_url,
        &verifier,
        &state,
        &discovery.scopes,
        &discovery.resource,
    )?;

    eprintln!(
        "Open this URL in a browser to authorize MCP server '{server_name}':\n{authorization_url}"
    );
    let callback = if manual {
        eprintln!("Paste the complete callback URL after authorization:");
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|error| format!("failed to read callback URL: {error}"))?;
        line.trim().to_string()
    } else {
        receive_callback(listener, &state).await?
    };
    let (code, returned_state) = authorization_response(&callback)?;
    if returned_state != state {
        return Err("OAuth callback state did not match the login request".to_string());
    }
    let token = exchange_authorization_code(
        &client,
        &discovery.token_endpoint,
        &registration.client_id,
        registration.client_secret.as_deref(),
        &callback_url,
        &code,
        &verifier,
        &discovery.resource,
    )
    .await?;
    save_credentials(
        server_name,
        OAuthCredentials {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            token_endpoint: discovery.token_endpoint,
            client_id: registration.client_id,
            client_secret: registration.client_secret,
            resource: discovery.resource,
            endpoint: endpoint.to_string(),
            expires_at: token
                .expires_in
                .map(|seconds| Utc::now().timestamp().saturating_add(seconds)),
        },
    )?;
    eprintln!("OAuth login completed for MCP server '{server_name}'.");
    Ok(())
}

fn supports_pkce_s256(methods: Option<&[String]>) -> bool {
    methods.is_some_and(|methods| methods.iter().any(|method| method == "S256"))
}

/// Removes saved OAuth credentials for an MCP server.
pub(super) fn logout(server_name: &str) -> Result<(), String> {
    if !remove_credentials(server_name)? {
        return Err(format!("MCP server '{server_name}' is not logged in"));
    }
    eprintln!("OAuth credentials removed for MCP server '{server_name}'.");
    Ok(())
}

/// Returns whether credentials exist without exposing their contents.
pub(super) fn has_credentials(server_name: &str) -> Result<bool, String> {
    Ok(read_credentials()?.servers.contains_key(server_name))
}

/// Deletes credentials when present and returns whether anything was removed.
pub(super) fn remove_credentials(server_name: &str) -> Result<bool, String> {
    let mut credentials = read_credentials()?;
    let removed = credentials.servers.remove(server_name).is_some();
    if removed {
        write_credentials(&credentials)?;
    }
    Ok(removed)
}

/// Loads credentials only when they were authorized for this endpoint and configured resource.
pub(super) fn load_credentials(
    server_name: &str,
    endpoint: &Url,
    resource: Option<&str>,
) -> Result<Option<OAuthCredentials>, String> {
    let mut credentials = read_credentials()?;
    Ok(credentials
        .servers
        .remove(server_name)
        .filter(|credentials| {
            credentials.endpoint == endpoint.as_str()
                && match resource {
                    Some(resource) => credentials.resource == resource,
                    None => true,
                }
        }))
}

/// Refreshes an expired access token and persists the rotated credential set.
pub(super) async fn refresh_credentials(
    server_name: &str,
    credentials: &mut OAuthCredentials,
    force: bool,
) -> Result<(), String> {
    let expires_soon = credentials
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now().timestamp() + EXPIRY_SKEW_SECONDS);
    if !force && !expires_soon {
        return Ok(());
    }
    let refresh_token = credentials.refresh_token.as_deref().ok_or_else(|| {
        format!("MCP OAuth access token expired; run 'cosh-core mcp login {server_name}'")
    })?;
    let client = oauth_client()?;
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", credentials.client_id.clone()),
        ("resource", credentials.resource.clone()),
    ];
    if let Some(client_secret) = credentials.client_secret.clone() {
        form.push(("client_secret", client_secret));
    }
    let token_endpoint = validated_oauth_url(&credentials.token_endpoint, "OAuth token endpoint")?;
    let response = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("failed to refresh MCP OAuth token: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "MCP OAuth token refresh failed with status {}; run 'cosh-core mcp login {server_name}'",
            response.status()
        ));
    }
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("MCP OAuth refresh response is invalid: {error}"))?;
    credentials.access_token = token.access_token;
    if token.refresh_token.is_some() {
        credentials.refresh_token = token.refresh_token;
    }
    credentials.expires_at = token
        .expires_in
        .map(|seconds| Utc::now().timestamp().saturating_add(seconds));
    save_credentials(server_name, credentials.clone())
}

struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    code_challenge_methods_supported: Option<Vec<String>>,
    scopes: Vec<String>,
    resource: String,
}

async fn discover(
    client: &reqwest::Client,
    endpoint: &Url,
    config: &McpOAuthConfig,
) -> Result<Discovery, String> {
    let (protected_metadata, resource, scopes) = if config.auth_server_metadata_url.is_some() {
        (
            None,
            config
                .resource
                .clone()
                .unwrap_or_else(|| endpoint.to_string()),
            config.scopes.clone(),
        )
    } else {
        let response = client
            .post(endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": super::initialize_params(super::HTTP_MCP_PROTOCOL_VERSION),
            }))
            .send()
            .await
            .map_err(|error| {
                format!("failed to contact MCP server for OAuth discovery: {error}")
            })?;
        if response.status().is_success() {
            return Err(
                "MCP server accepted an unauthenticated request; OAuth login is not required"
                    .to_string(),
            );
        }
        if !matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(format!(
                "MCP server returned {} during OAuth discovery",
                response.status()
            ));
        }
        let challenge = authorization_challenge(response.headers(), endpoint)?;
        let metadata = load_protected_resource_metadata(client, endpoint, &challenge).await?;
        let resource = config
            .resource
            .clone()
            .unwrap_or_else(|| endpoint.to_string());
        validate_protected_resource_metadata(&metadata, &resource)?;
        let scopes = if let Some(scopes) = challenge.scopes {
            scopes
        } else if metadata.scopes_supported.is_empty() {
            config.scopes.clone()
        } else {
            metadata.scopes_supported.clone()
        };
        (Some(metadata), resource, scopes)
    };
    let metadata_url = match &config.auth_server_metadata_url {
        Some(url) => url.clone(),
        None => {
            let server = protected_metadata
                .as_ref()
                .and_then(|metadata| metadata.authorization_servers.first())
                .ok_or_else(|| {
                    "MCP protected-resource metadata has no authorization_servers".to_string()
                })?;
            server.clone()
        }
    };
    let metadata = load_authorization_server_metadata(
        client,
        &metadata_url,
        config.auth_server_metadata_url.is_some(),
    )
    .await?;
    validated_oauth_url(
        &metadata.authorization_endpoint,
        "OAuth authorization endpoint",
    )?;
    validated_oauth_url(&metadata.token_endpoint, "OAuth token endpoint")?;
    if let Some(registration_endpoint) = &metadata.registration_endpoint {
        validated_oauth_url(registration_endpoint, "OAuth registration endpoint")?;
    }
    Ok(Discovery {
        authorization_endpoint: metadata.authorization_endpoint,
        token_endpoint: metadata.token_endpoint,
        registration_endpoint: metadata.registration_endpoint,
        code_challenge_methods_supported: metadata.code_challenge_methods_supported,
        scopes,
        resource,
    })
}

async fn register_client(
    client: &reqwest::Client,
    discovery: &Discovery,
    config: &McpOAuthConfig,
    callback_url: &str,
) -> Result<RegistrationResponse, String> {
    if let Some(client_id) = config.client_id.as_ref().filter(|id| !id.is_empty()) {
        return Ok(RegistrationResponse {
            client_id: client_id.clone(),
            client_secret: None,
        });
    }
    let endpoint = discovery.registration_endpoint.as_deref().ok_or_else(|| {
        "OAuth server does not support dynamic client registration; set mcp.servers.<name>.oauth.client_id"
            .to_string()
    })?;
    let response = client
        .post(endpoint)
        .json(&json!({
            "client_name": "cosh-core",
            "redirect_uris": [callback_url],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .map_err(|error| format!("failed to dynamically register OAuth client: {error}"))?
        .error_for_status()
        .map_err(|error| format!("OAuth dynamic client registration failed: {error}"))?;
    response
        .json()
        .await
        .map_err(|error| format!("OAuth client registration response is invalid: {error}"))
}

fn build_authorization_url(
    endpoint: &str,
    client_id: &str,
    callback_url: &str,
    verifier: &str,
    state: &str,
    scopes: &[String],
    resource: &str,
) -> Result<Url, String> {
    let mut url = Url::parse(endpoint)
        .map_err(|error| format!("OAuth authorization endpoint is invalid: {error}"))?;
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    let mut query = url.query_pairs_mut();
    query.append_pair("response_type", "code");
    query.append_pair("client_id", client_id);
    query.append_pair("redirect_uri", callback_url);
    query.append_pair("code_challenge", &challenge);
    query.append_pair("code_challenge_method", "S256");
    query.append_pair("state", state);
    query.append_pair("resource", resource);
    if !scopes.is_empty() {
        query.append_pair("scope", &scopes.join(" "));
    }
    drop(query);
    Ok(url)
}

async fn receive_callback(listener: TcpListener, expected_state: &str) -> Result<String, String> {
    timeout(CALLBACK_TIMEOUT, async {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("failed to accept OAuth callback: {error}"))?;
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .await
            .map_err(|error| format!("failed to read OAuth callback: {error}"))?;
        let target = request_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| "OAuth callback request is malformed".to_string())?;
        let callback = format!("http://localhost{target}");
        let (_, state) = authorization_response(&callback)?;
        let response = if state == expected_state {
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nOAuth login completed. You may close this tab."
        } else {
            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nOAuth state did not match."
        };
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .map_err(|error| format!("failed to respond to OAuth callback: {error}"))?;
        Ok(callback)
    })
    .await
    .map_err(|_| "OAuth callback timed out after 5 minutes".to_string())?
}

fn authorization_response(callback: &str) -> Result<(String, String), String> {
    let callback =
        Url::parse(callback).map_err(|error| format!("OAuth callback URL is invalid: {error}"))?;
    let values: HashMap<_, _> = callback.query_pairs().into_owned().collect();
    if let Some(error) = values.get("error") {
        return Err(format!("OAuth authorization failed: {error}"));
    }
    let code = values
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| "OAuth callback has no authorization code".to_string())?;
    let state = values
        .get("state")
        .cloned()
        .ok_or_else(|| "OAuth callback has no state".to_string())?;
    Ok((code, state))
}

async fn exchange_authorization_code(
    client: &reqwest::Client,
    endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    callback_url: &str,
    code: &str,
    verifier: &str,
    resource: &str,
) -> Result<TokenResponse, String> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("client_id", client_id.to_string()),
        ("redirect_uri", callback_url.to_string()),
        ("code_verifier", verifier.to_string()),
        ("resource", resource.to_string()),
    ];
    if let Some(client_secret) = client_secret {
        form.push(("client_secret", client_secret.to_string()));
    }
    let response = client
        .post(endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("failed to exchange MCP OAuth authorization code: {error}"))?
        .error_for_status()
        .map_err(|error| format!("MCP OAuth authorization-code exchange failed: {error}"))?;
    response
        .json()
        .await
        .map_err(|error| format!("MCP OAuth token response is invalid: {error}"))
}

fn authorization_challenge(
    headers: &reqwest::header::HeaderMap,
    endpoint: &Url,
) -> Result<AuthorizationChallenge, String> {
    let mut resource_metadata_url = None;
    let scopes = challenged_scopes(headers);
    for value in headers.get_all(WWW_AUTHENTICATE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        if resource_metadata_url.is_none() {
            resource_metadata_url = quoted_parameter(value, "resource_metadata")
                .map(|url| validated_oauth_url(url, "OAuth resource_metadata URL"))
                .transpose()?;
        }
    }
    if resource_metadata_url.is_none() {
        for url in protected_resource_metadata_urls(endpoint)? {
            validated_oauth_url(url.as_str(), "OAuth protected-resource metadata URL")?;
        }
    }
    Ok(AuthorizationChallenge {
        resource_metadata_url,
        scopes,
    })
}

/// Returns the scope challenge supplied by an MCP server, if present.
pub(super) fn challenged_scopes(headers: &reqwest::header::HeaderMap) -> Option<Vec<String>> {
    headers.get_all(WWW_AUTHENTICATE).iter().find_map(|value| {
        value.to_str().ok().and_then(|value| {
            quoted_parameter(value, "scope").map(|scope| {
                scope
                    .split_ascii_whitespace()
                    .map(ToString::to_string)
                    .collect()
            })
        })
    })
}

fn quoted_parameter<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let bytes = header.as_bytes();
    let name = name.as_bytes();
    let mut index = 0;
    let mut quoted = false;

    while index + name.len() <= bytes.len() {
        if bytes[index] == b'"' && (index == 0 || bytes[index - 1] != b'\\') {
            quoted = !quoted;
            index += 1;
            continue;
        }
        let before_is_boundary =
            index == 0 || bytes[index - 1] == b',' || bytes[index - 1].is_ascii_whitespace();
        if !quoted
            && before_is_boundary
            && bytes[index..index + name.len()].eq_ignore_ascii_case(name)
        {
            let mut value_start = index + name.len();
            while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
                value_start += 1;
            }
            if bytes.get(value_start) != Some(&b'=') {
                index += name.len();
                continue;
            }
            value_start += 1;
            while value_start < bytes.len() && bytes[value_start].is_ascii_whitespace() {
                value_start += 1;
            }
            if bytes.get(value_start) != Some(&b'"') {
                index += name.len();
                continue;
            }
            let value_start = value_start + 1;
            let value_end = bytes[value_start..]
                .iter()
                .position(|byte| *byte == b'"')
                .map(|offset| value_start + offset)?;
            return Some(&header[value_start..value_end]);
        }
        index += 1;
    }
    None
}

fn protected_resource_metadata_urls(endpoint: &Url) -> Result<Vec<Url>, String> {
    let mut urls = Vec::new();
    let path = endpoint.path().trim_matches('/');
    if !path.is_empty() {
        let mut with_path = endpoint.clone();
        with_path.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
        with_path.set_query(None);
        urls.push(with_path);
    }
    let mut root = endpoint.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    urls.push(root);
    Ok(urls)
}

async fn load_protected_resource_metadata(
    client: &reqwest::Client,
    endpoint: &Url,
    challenge: &AuthorizationChallenge,
) -> Result<ProtectedResourceMetadata, String> {
    let urls = match &challenge.resource_metadata_url {
        Some(url) => vec![url.clone()],
        None => protected_resource_metadata_urls(endpoint)?,
    };
    for (index, url) in urls.iter().enumerate() {
        let response =
            client.get(url.clone()).send().await.map_err(|error| {
                format!("failed to load MCP protected-resource metadata: {error}")
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND
            && challenge.resource_metadata_url.is_none()
            && index + 1 < urls.len()
        {
            continue;
        }
        return response
            .error_for_status()
            .map_err(|error| format!("MCP protected-resource metadata request failed: {error}"))?
            .json()
            .await
            .map_err(|error| format!("MCP protected-resource metadata is invalid: {error}"));
    }
    Err("MCP protected-resource metadata was not found".to_string())
}

fn validate_protected_resource_metadata(
    metadata: &ProtectedResourceMetadata,
    expected_resource: &str,
) -> Result<(), String> {
    if metadata.resource != expected_resource {
        return Err(
            "MCP protected-resource metadata resource does not match the configured resource"
                .to_string(),
        );
    }
    Ok(())
}

fn authorization_server_metadata_urls(server: &str) -> Result<Vec<Url>, String> {
    let server = validated_oauth_url(server, "OAuth authorization server URL")?;
    let path = server.path().trim_matches('/').to_string();
    let mut urls = Vec::new();
    for well_known in [
        ".well-known/oauth-authorization-server",
        ".well-known/openid-configuration",
    ] {
        let mut url = server.clone();
        let discovered_path = if path.is_empty() {
            format!("/{well_known}")
        } else {
            format!("/{well_known}/{path}")
        };
        url.set_path(&discovered_path);
        url.set_query(None);
        urls.push(url);
    }
    if !path.is_empty() {
        let mut appended = server;
        appended.set_path(&format!("/{path}/.well-known/openid-configuration"));
        appended.set_query(None);
        urls.push(appended);
    }
    Ok(urls)
}

async fn load_authorization_server_metadata(
    client: &reqwest::Client,
    server_or_metadata_url: &str,
    explicit_metadata_url: bool,
) -> Result<AuthorizationServerMetadata, String> {
    let (urls, expected_issuer) = if explicit_metadata_url {
        let url = validated_oauth_url(
            server_or_metadata_url,
            "OAuth authorization-server metadata URL",
        )?;
        let issuer = issuer_from_metadata_url(&url)?;
        (vec![url], issuer)
    } else {
        (
            authorization_server_metadata_urls(server_or_metadata_url)?,
            server_or_metadata_url.to_string(),
        )
    };
    for (index, url) in urls.iter().enumerate() {
        let response = client.get(url.clone()).send().await.map_err(|error| {
            format!("failed to load OAuth authorization-server metadata: {error}")
        })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && index + 1 < urls.len() {
            continue;
        }
        let metadata: AuthorizationServerMetadata = response
            .error_for_status()
            .map_err(|error| {
                format!("OAuth authorization-server metadata request failed: {error}")
            })?
            .json()
            .await
            .map_err(|error| format!("OAuth authorization-server metadata is invalid: {error}"))?;
        validate_authorization_server_issuer(&metadata, &expected_issuer)?;
        return Ok(metadata);
    }
    Err("OAuth authorization-server metadata was not found".to_string())
}

fn issuer_from_metadata_url(metadata_url: &Url) -> Result<String, String> {
    let path = metadata_url.path();
    let issuer_path =
        if let Some(path) = path.strip_prefix("/.well-known/oauth-authorization-server") {
            path
        } else if let Some(path) = path.strip_prefix("/.well-known/openid-configuration") {
            path
        } else if let Some(path) = path.strip_suffix("/.well-known/openid-configuration") {
            path
        } else {
            return Err(
                "OAuth authorization-server metadata URL must use a standard well-known path"
                    .to_string(),
            );
        };
    if !issuer_path.is_empty() && !issuer_path.starts_with('/') {
        return Err(
            "OAuth authorization-server metadata URL has an invalid well-known path".to_string(),
        );
    }
    Ok(format!(
        "{}{}",
        metadata_url.origin().ascii_serialization(),
        issuer_path
    ))
}

fn validate_authorization_server_issuer(
    metadata: &AuthorizationServerMetadata,
    expected_issuer: &str,
) -> Result<(), String> {
    validated_oauth_url(&metadata.issuer, "OAuth authorization-server issuer")?;
    if metadata.issuer != expected_issuer {
        return Err(
            "OAuth authorization-server metadata issuer does not match the discovery issuer"
                .to_string(),
        );
    }
    Ok(())
}

fn validated_oauth_url(value: &str, label: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("{label} is invalid: {error}"))?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if url.scheme() == "https" || (url.scheme() == "http" && loopback) {
        return Ok(url);
    }
    Err(format!(
        "{label} must use HTTPS unless it is a loopback URL"
    ))
}

fn credentials_path() -> PathBuf {
    config_dir().join(CREDENTIALS_FILE)
}

fn oauth_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("failed to build OAuth client: {error}"))
}

fn read_credentials() -> Result<CredentialFile, String> {
    let path = credentials_path();
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
            format!(
                "MCP OAuth credentials file {} is invalid: {error}",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CredentialFile::default()),
        Err(error) => Err(format!("failed to read MCP OAuth credentials: {error}")),
    }
}

fn save_credentials(server_name: &str, credentials: OAuthCredentials) -> Result<(), String> {
    let mut file = read_credentials()?;
    file.servers.insert(server_name.to_string(), credentials);
    write_credentials(&file)
}

fn write_credentials(credentials: &CredentialFile) -> Result<(), String> {
    let path = credentials_path();
    let dir = path
        .parent()
        .ok_or_else(|| "MCP OAuth credentials path has no parent".to_string())?;
    fs::create_dir_all(dir)
        .map_err(|error| format!("failed to create MCP OAuth directory: {error}"))?;
    let temporary = dir.join(format!("{CREDENTIALS_FILE}.tmp.{}", Uuid::new_v4()));
    let contents = serde_json::to_vec(credentials)
        .map_err(|error| format!("failed to encode MCP OAuth credentials: {error}"))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(&temporary)
        .map_err(|error| format!("failed to create MCP OAuth credentials file: {error}"))?;
    output
        .write_all(&contents)
        .map_err(|error| format!("failed to write MCP OAuth credentials: {error}"))?;
    output
        .sync_all()
        .map_err(|error| format!("failed to sync MCP OAuth credentials: {error}"))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("failed to save MCP OAuth credentials: {error}"))
}

fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[((first & 0x03) << 4 | second >> 4) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[((second & 0x0f) << 2 | third >> 6) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_uses_url_safe_base64_without_padding() {
        assert_eq!(base64_url(&[0xfb, 0xff, 0xff]), "-___");
    }

    #[test]
    fn parses_authorization_callback() {
        let callback = "http://127.0.0.1/callback?code=test-code&state=test-state";
        assert_eq!(
            authorization_response(callback).unwrap(),
            ("test-code".to_string(), "test-state".to_string())
        );
    }

    #[test]
    fn parses_resource_metadata_challenge() {
        let endpoint = Url::parse("https://example.com/mcp").unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            "Bearer resource_metadata = \"https://example.com/meta\", scope = \"files:read\""
                .parse()
                .unwrap(),
        );
        let challenge = authorization_challenge(&headers, &endpoint).unwrap();
        assert_eq!(
            challenge.resource_metadata_url.unwrap().as_str(),
            "https://example.com/meta"
        );
        assert_eq!(challenge.scopes, Some(vec!["files:read".to_string()]));
    }

    #[test]
    fn ignores_parameter_names_inside_quoted_values() {
        let endpoint = Url::parse("https://example.com/mcp").unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            "Bearer resource_metadata=\"https://example.com/meta?scope=ignored\", scope=\"tools:read\""
                .parse()
                .unwrap(),
        );
        let challenge = authorization_challenge(&headers, &endpoint).unwrap();
        assert_eq!(challenge.scopes, Some(vec!["tools:read".to_string()]));
    }

    #[test]
    fn protected_resource_metadata_falls_back_from_path_to_root() {
        let endpoint = Url::parse("https://example.com/mcp/server").unwrap();
        assert_eq!(
            protected_resource_metadata_urls(&endpoint)
                .unwrap()
                .iter()
                .map(Url::as_str)
                .collect::<Vec<_>>(),
            vec![
                "https://example.com/.well-known/oauth-protected-resource/mcp/server",
                "https://example.com/.well-known/oauth-protected-resource"
            ]
        );
    }

    #[test]
    fn authorization_server_metadata_preserves_issuer_path_and_tries_oidc() {
        assert_eq!(
            authorization_server_metadata_urls("https://auth.example.com/tenant")
                .unwrap()
                .iter()
                .map(Url::as_str)
                .collect::<Vec<_>>(),
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant",
                "https://auth.example.com/.well-known/openid-configuration/tenant",
                "https://auth.example.com/tenant/.well-known/openid-configuration"
            ]
        );
    }

    #[test]
    fn rejects_missing_or_mismatched_protected_resource_metadata() {
        assert!(serde_json::from_str::<ProtectedResourceMetadata>(
            r#"{"authorization_servers":[],"scopes_supported":[]}"#,
        )
        .is_err());

        let mut metadata = ProtectedResourceMetadata {
            resource: "https://other.example.com/mcp".to_string(),
            authorization_servers: Vec::new(),
            scopes_supported: Vec::new(),
        };
        assert!(
            validate_protected_resource_metadata(&metadata, "https://example.com/mcp").is_err()
        );

        metadata.resource = "https://example.com/mcp".to_string();
        assert!(validate_protected_resource_metadata(&metadata, "https://example.com/mcp").is_ok());
    }

    #[test]
    fn rejects_missing_or_mismatched_authorization_server_issuer() {
        assert!(serde_json::from_str::<AuthorizationServerMetadata>(
            r#"{"authorization_endpoint":"https://auth.example.com/authorize","token_endpoint":"https://auth.example.com/token"}"#,
        )
        .is_err());

        let metadata = AuthorizationServerMetadata {
            issuer: "https://other.example.com".to_string(),
            authorization_endpoint: "https://auth.example.com/authorize".to_string(),
            token_endpoint: "https://auth.example.com/token".to_string(),
            registration_endpoint: None,
            code_challenge_methods_supported: Some(vec!["S256".to_string()]),
        };
        assert!(
            validate_authorization_server_issuer(&metadata, "https://auth.example.com").is_err()
        );

        let metadata = AuthorizationServerMetadata {
            issuer: "https://auth.example.com".to_string(),
            ..metadata
        };
        assert!(
            validate_authorization_server_issuer(&metadata, "https://auth.example.com").is_ok()
        );
    }

    #[test]
    fn derives_issuer_from_rfc_and_oidc_metadata_urls() {
        for metadata_url in [
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant",
            "https://auth.example.com/.well-known/openid-configuration/tenant",
            "https://auth.example.com/tenant/.well-known/openid-configuration",
        ] {
            let url = Url::parse(metadata_url).unwrap();
            assert_eq!(
                issuer_from_metadata_url(&url).unwrap(),
                "https://auth.example.com/tenant"
            );
        }
    }

    #[test]
    fn oauth_endpoints_require_https_except_loopback() {
        assert!(validated_oauth_url("https://example.com/token", "token").is_ok());
        assert!(validated_oauth_url("http://127.0.0.1/token", "token").is_ok());
        assert!(validated_oauth_url("http://[::1]/token", "token").is_ok());
        assert!(validated_oauth_url("http://example.com/token", "token").is_err());
    }

    #[test]
    fn rejects_missing_or_empty_pkce_metadata() {
        assert!(!supports_pkce_s256(None));
        assert!(!supports_pkce_s256(Some(&[])));
        assert!(!supports_pkce_s256(Some(&["plain".to_string()])));
        assert!(supports_pkce_s256(Some(&["S256".to_string()])));
    }

    #[tokio::test]
    async fn expired_credential_names_server_in_relogin_hint() {
        let mut credentials = OAuthCredentials {
            access_token: "expired".to_string(),
            refresh_token: None,
            token_endpoint: "https://example.com/token".to_string(),
            client_id: "client".to_string(),
            client_secret: None,
            resource: "https://example.com/mcp".to_string(),
            endpoint: "https://example.com/mcp".to_string(),
            expires_at: Some(0),
        };
        let error = refresh_credentials("remote", &mut credentials, false)
            .await
            .unwrap_err();
        assert!(error.contains("mcp login remote"));
        assert!(!error.contains("<server>"));
    }
}
