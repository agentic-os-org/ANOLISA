//! Provider-specific authentication probes that never emit secrets or response bodies.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::ResolvedProvider;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const ALIYUN_PERMISSION_PATH: &str = "/api/v1/openapi/initial";
const ALIYUN_PERMISSION_ACTION: &str = "InitialSysom";
const ALIYUN_COPILOT_PATH: &str = "/api/v1/copilot/generate_copilot_stream_response";
const ALIYUN_COPILOT_ACTION: &str = "GenerateCopilotStreamResponse";
const ALIYUN_API_VERSION: &str = "2023-12-30";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CODING_AGENT_USER_AGENT: &str =
    concat!("Cosh/", env!("CARGO_PKG_VERSION"), " (coding-agent)");

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelFallback {
    None,
    List,
    Chat,
    ListThenChat,
}

/// Stable authentication-preflight classifications consumed by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthPreflightError {
    InvalidCredentials,
    PermissionDenied,
    ModelUnavailable { model: String },
    EndpointUnreachable,
    Timeout,
    RateLimited,
    ProviderUnavailable,
    ServiceNotReady,
    CredentialSourceUnavailable,
    UnsupportedResponse,
}

impl AuthPreflightError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::PermissionDenied => "permission_denied",
            Self::ModelUnavailable { .. } => "model_unavailable",
            Self::EndpointUnreachable => "endpoint_unreachable",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ServiceNotReady => "service_not_ready",
            Self::CredentialSourceUnavailable => "credential_source_unavailable",
            Self::UnsupportedResponse => "unsupported_response",
        }
    }
}

impl fmt::Display for AuthPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredentials => {
                formatter.write_str("The API key was rejected. Check the API Key and try again.")
            }
            Self::PermissionDenied => formatter.write_str(
                "The credentials do not have permission to use this endpoint. Check the API Key and permissions.",
            ),
            Self::ModelUnavailable { model } => write!(
                formatter,
                "Model {model:?} is unavailable. Check the Model name and access entitlement."
            ),
            Self::EndpointUnreachable => formatter.write_str(
                "The endpoint could not be reached. Check the endpoint configuration and network connection.",
            ),
            Self::Timeout => formatter.write_str(
                "The endpoint did not respond in time. Check the endpoint configuration and network connection.",
            ),
            Self::RateLimited => formatter.write_str(
                "The provider rate-limited the validation request. Check quota or try again later.",
            ),
            Self::ProviderUnavailable => formatter.write_str(
                "The provider is temporarily unavailable. Try again later or check the endpoint configuration.",
            ),
            Self::ServiceNotReady => formatter.write_str(
                "Aliyun SysOM is not authorized for this account. Complete service authorization and try again.",
            ),
            Self::CredentialSourceUnavailable => formatter.write_str(
                "ECS RAM Role credentials are not available yet. Authorize the instance role and try again.",
            ),
            Self::UnsupportedResponse => formatter.write_str(
                "The endpoint returned an unsupported validation response. Check the endpoint configuration and provider compatibility.",
            ),
        }
    }
}

impl std::error::Error for AuthPreflightError {}

/// Validates credentials and model availability without retrying or following redirects.
pub(crate) async fn preflight_auth(provider: &ResolvedProvider) -> Result<(), AuthPreflightError> {
    preflight_auth_with_timeout(provider, REQUEST_TIMEOUT).await
}

async fn preflight_auth_with_timeout(
    provider: &ResolvedProvider,
    total_timeout: Duration,
) -> Result<(), AuthPreflightError> {
    enforce_preflight_deadline(total_timeout, preflight_auth_inner(provider)).await
}

async fn enforce_preflight_deadline<F>(
    total_timeout: Duration,
    preflight: F,
) -> Result<(), AuthPreflightError>
where
    F: Future<Output = Result<(), AuthPreflightError>>,
{
    tokio::time::timeout(total_timeout, preflight)
        .await
        .unwrap_or(Err(AuthPreflightError::Timeout))
}

async fn preflight_auth_inner(provider: &ResolvedProvider) -> Result<(), AuthPreflightError> {
    if provider.provider_type == "mock" {
        return Ok(());
    }
    if provider.provider_type == "aliyun" {
        return preflight_aliyun(provider).await;
    }

    let client = build_client(None)?;
    match provider.provider_type.as_str() {
        "dashscope" => {
            preflight_model_endpoint(&client, provider, ModelFallback::ListThenChat).await
        }
        "coding_plan" | "token_plan" => {
            preflight_model_endpoint(&client, provider, ModelFallback::ListThenChat).await
        }
        "openai_compat" => preflight_model_endpoint(&client, provider, ModelFallback::Chat).await,
        "openai" | "generic" | "deepseek" => {
            preflight_model_endpoint(&client, provider, ModelFallback::List).await
        }
        _ => preflight_model_endpoint(&client, provider, ModelFallback::None).await,
    }
}

fn build_client(
    endpoint: Option<&crate::provider::sysom::endpoint::ResolvedEndpoint>,
) -> Result<Client, AuthPreflightError> {
    let builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    let builder = match endpoint {
        Some(endpoint) => endpoint.configure_client(builder),
        None => builder,
    };
    builder
        .build()
        .map_err(|_| AuthPreflightError::EndpointUnreachable)
}

async fn preflight_model_endpoint(
    client: &Client,
    provider: &ResolvedProvider,
    fallback: ModelFallback,
) -> Result<(), AuthPreflightError> {
    let model_url = endpoint_url(&provider.base_url, &["models", &provider.model])?;
    let response = send(authenticated_request(client.get(model_url), provider)).await?;
    let status = response.status();

    if status.is_success() {
        return validate_model_response(response, &provider.model).await;
    }

    if status == StatusCode::NOT_FOUND {
        let explicitly_missing = response_explicitly_reports_missing_model(response).await;
        if explicitly_missing || fallback == ModelFallback::None {
            return Err(AuthPreflightError::ModelUnavailable {
                model: provider.model.clone(),
            });
        }
        return preflight_model_fallback(client, provider, fallback).await;
    }
    // The token-plan gateway answers GET /models/{model} with HTTP 400
    // "Model not exist." even for models its own model list contains and
    // chat serves, so a 400 must defer to the fallback chain (model list,
    // then minimal chat) instead of ending validation here.
    if fallback != ModelFallback::None
        && matches!(
            status,
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED | StatusCode::BAD_REQUEST
        )
    {
        return preflight_model_fallback(client, provider, fallback).await;
    }

    Err(classify_status(status))
}

async fn preflight_model_fallback(
    client: &Client,
    provider: &ResolvedProvider,
    fallback: ModelFallback,
) -> Result<(), AuthPreflightError> {
    match fallback {
        ModelFallback::List => preflight_model_list(client, provider).await,
        ModelFallback::Chat => preflight_chat(client, provider).await,
        ModelFallback::ListThenChat => preflight_model_list_then_chat(client, provider).await,
        ModelFallback::None => Err(AuthPreflightError::UnsupportedResponse),
    }
}

async fn preflight_model_list(
    client: &Client,
    provider: &ResolvedProvider,
) -> Result<(), AuthPreflightError> {
    let url = endpoint_url(&provider.base_url, &["models"])?;
    let response = send(authenticated_request(client.get(url), provider)).await?;
    let status = response.status();
    if !status.is_success() {
        return Err(classify_status(status));
    }
    validate_model_list(response, &provider.model).await
}

async fn preflight_model_list_then_chat(
    client: &Client,
    provider: &ResolvedProvider,
) -> Result<(), AuthPreflightError> {
    let url = endpoint_url(&provider.base_url, &["models"])?;
    let response = send(authenticated_request(client.get(url), provider)).await?;
    let status = response.status();
    if status.is_success() {
        validate_model_list(response, &provider.model).await?;
    } else if !matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    ) {
        return Err(classify_status(status));
    }

    // A model list may be public. A minimal completion is therefore still required
    // to prove that the submitted credential can use the selected model.
    preflight_chat(client, provider).await
}

async fn validate_model_list(
    response: Response,
    expected_model: &str,
) -> Result<(), AuthPreflightError> {
    let body = response_json(response).await?;
    let models = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or(AuthPreflightError::UnsupportedResponse)?;
    if models.iter().any(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == expected_model)
    }) {
        Ok(())
    } else {
        Err(AuthPreflightError::ModelUnavailable {
            model: expected_model.to_string(),
        })
    }
}

async fn preflight_chat(
    client: &Client,
    provider: &ResolvedProvider,
) -> Result<(), AuthPreflightError> {
    let url = endpoint_url(&provider.base_url, &["chat", "completions"])?;
    let response = send(
        authenticated_request(client.post(url), provider)
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "model": provider.model,
                "messages": [{"role": "user", "content": "ping"}],
                "stream": false,
                "max_tokens": 1
            })),
    )
    .await?;
    let status = response.status();
    if status.is_success() {
        let body = response_json(response).await?;
        return body
            .get("choices")
            .and_then(Value::as_array)
            .filter(|choices| !choices.is_empty())
            .map(|_| ())
            .ok_or(AuthPreflightError::UnsupportedResponse);
    }
    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
    ) && response_explicitly_reports_missing_model(response).await
    {
        return Err(AuthPreflightError::ModelUnavailable {
            model: provider.model.clone(),
        });
    }
    Err(classify_status(status))
}

fn authenticated_request(builder: RequestBuilder, provider: &ResolvedProvider) -> RequestBuilder {
    let builder = builder
        .header(AUTHORIZATION, bearer(&provider.api_key))
        .header(ACCEPT, "application/json");
    if matches!(
        provider.provider_type.as_str(),
        "coding_plan" | "token_plan"
    ) {
        builder
            .header(USER_AGENT, CODING_AGENT_USER_AGENT)
            .header("x-dashscope-useragent", CODING_AGENT_USER_AGENT)
            .header("x-dashscope-authtype", "openai")
    } else {
        builder
    }
}

async fn validate_model_response(
    response: Response,
    expected_model: &str,
) -> Result<(), AuthPreflightError> {
    let body = response_json(response).await?;
    match body.get("id").and_then(Value::as_str) {
        Some(id) if id == expected_model => Ok(()),
        _ => Err(AuthPreflightError::UnsupportedResponse),
    }
}

async fn response_explicitly_reports_missing_model(response: Response) -> bool {
    let Ok(body) = response_json(response).await else {
        return false;
    };
    let error = body.get("error").unwrap_or(&body);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let param = error
        .get("param")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(
        code,
        "model_not_found" | "invalid_model" | "model_not_exist" | "Model.NotFound"
    ) || matches!(error_type, "model_not_found" | "invalid_model")
        || param == "model"
}

async fn preflight_aliyun(provider: &ResolvedProvider) -> Result<(), AuthPreflightError> {
    if provider.auth_source.as_deref() == Some("ecs_ram_role") {
        return tokio::task::spawn_blocking(
            crate::provider::sysom::ecs_ram_role_credentials_available,
        )
        .await
        .map_err(|_| AuthPreflightError::EndpointUnreachable)?
        .then_some(())
        .ok_or(AuthPreflightError::CredentialSourceUnavailable);
    }

    let resolved = crate::provider::sysom::endpoint::resolve(&provider.sysom_endpoint).await;
    let client = build_client(Some(&resolved))?;
    tracing::debug!(
        host = %resolved.host,
        origin = resolved.origin.as_str(),
        "sysom preflight endpoint"
    );
    let base_url = resolved.base_url();
    let base_url = base_url.as_str();
    let payload = br#"{"check_only":true,"source":"cosh"}"#;
    let request = signed_aliyun_request(
        &client,
        base_url,
        ALIYUN_PERMISSION_PATH,
        ALIYUN_PERMISSION_ACTION,
        payload,
        provider,
    )?;
    let response = send(request).await?;
    let status = response.status();
    let body = response_json(response).await;
    if status.is_success() {
        let body = body?;
        match aliyun_response_code(&body) {
            Some("Success") if aliyun_role_exists(&body) == Some(true) => {}
            Some("Success") if aliyun_role_exists(&body) == Some(false) => {
                return Err(AuthPreflightError::ServiceNotReady);
            }
            Some("Success") | None => return Err(AuthPreflightError::UnsupportedResponse),
            Some(code) => return Err(classify_aliyun_code(code, &provider.model)),
        }
    } else {
        if let Ok(body) = body {
            if let Some(code) = aliyun_response_code(&body) {
                return Err(classify_aliyun_code(code, &provider.model));
            }
        }
        return Err(classify_status(status));
    }

    preflight_aliyun_copilot(&client, base_url, provider).await
}

fn signed_aliyun_request(
    client: &Client,
    base_url: &str,
    path: &str,
    action: &str,
    payload: &[u8],
    provider: &ResolvedProvider,
) -> Result<RequestBuilder, AuthPreflightError> {
    let mut url = Url::parse(base_url).map_err(|_| AuthPreflightError::EndpointUnreachable)?;
    url.set_path(path);
    url.set_query(None);
    let host_name = url
        .host_str()
        .ok_or(AuthPreflightError::EndpointUnreachable)?
        .to_string();
    let host = url
        .port()
        .map(|port| format!("{host_name}:{port}"))
        .unwrap_or(host_name);
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let nonce = Uuid::new_v4().to_string();
    let payload_hash = hex_sha256(payload);
    let mut headers = vec![
        ("host".to_string(), host.clone()),
        (
            "content-type".to_string(),
            "application/json; charset=utf-8".to_string(),
        ),
        ("x-acs-action".to_string(), action.to_string()),
        ("x-acs-content-sha256".to_string(), payload_hash.clone()),
        ("x-acs-date".to_string(), timestamp.clone()),
        ("x-acs-signature-nonce".to_string(), nonce.clone()),
        ("x-acs-version".to_string(), ALIYUN_API_VERSION.to_string()),
    ];
    if let Some(token) = provider.security_token.as_ref() {
        headers.push((
            "x-acs-accesskey-id".to_string(),
            provider.access_key_id.clone(),
        ));
        headers.push(("x-acs-security-token".to_string(), token.clone()));
    }
    let authorization = sign_acs3(
        "POST",
        path,
        "",
        &headers,
        &payload_hash,
        &provider.access_key_id,
        &provider.access_key_secret,
    );
    let mut request = client
        .post(url)
        .header("host", host)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header("x-acs-action", action)
        .header("x-acs-content-sha256", payload_hash)
        .header("x-acs-date", timestamp)
        .header("x-acs-signature-nonce", nonce)
        .header("x-acs-version", ALIYUN_API_VERSION)
        .header(AUTHORIZATION, authorization)
        .body(payload.to_vec());
    if let Some(token) = provider.security_token.as_ref() {
        request = request
            .header("x-acs-accesskey-id", &provider.access_key_id)
            .header("x-acs-security-token", token);
    }
    Ok(request)
}

async fn preflight_aliyun_copilot(
    client: &Client,
    base_url: &str,
    provider: &ResolvedProvider,
) -> Result<(), AuthPreflightError> {
    let llm_params = serde_json::json!({
        "messages": [{"role": "user", "content": "ping"}],
        "model": provider.model,
        "stream": true,
        "use_dashscope": true,
        "version": 2,
        "max_tokens": 1
    });
    let payload = serde_json::to_vec(&serde_json::json!({
        "llmParamString": llm_params.to_string()
    }))
    .map_err(|_| AuthPreflightError::UnsupportedResponse)?;
    let request = signed_aliyun_request(
        client,
        base_url,
        ALIYUN_COPILOT_PATH,
        ALIYUN_COPILOT_ACTION,
        &payload,
        provider,
    )?
    .header(ACCEPT, "text/event-stream")
    .header("x-sysom-invoke-source", "cosh");
    let response = send(request).await?;
    let status = response.status();
    if !status.is_success() {
        let body = response_json(response).await;
        if let Ok(body) = body {
            if let Some(code) = aliyun_response_code(&body) {
                return Err(classify_aliyun_code(code, &provider.model));
            }
        }
        return Err(classify_status(status));
    }
    let bytes = response_bytes(response).await?;
    validate_aliyun_copilot_stream(&bytes, &provider.model)
}

fn validate_aliyun_copilot_stream(bytes: &[u8], model: &str) -> Result<(), AuthPreflightError> {
    if let Ok(body) = serde_json::from_slice::<Value>(bytes) {
        return Err(aliyun_response_code(&body)
            .map(|code| classify_aliyun_code(code, model))
            .unwrap_or(AuthPreflightError::UnsupportedResponse));
    }
    let stream = std::str::from_utf8(bytes).map_err(|_| AuthPreflightError::UnsupportedResponse)?;
    let normalized = stream.replace("\r\n", "\n");
    let mut saw_success = false;
    for block in normalized.split("\n\n") {
        let mut event = "";
        let mut data = String::new();
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event = value.trim();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let body: Value =
            serde_json::from_str(&data).map_err(|_| AuthPreflightError::UnsupportedResponse)?;
        if event.eq_ignore_ascii_case("failed") {
            return Err(aliyun_response_code(&body)
                .map(|code| classify_aliyun_code(code, model))
                .unwrap_or(AuthPreflightError::UnsupportedResponse));
        }
        if event.eq_ignore_ascii_case("ok")
            && body.get("choices").and_then(Value::as_array).is_some()
        {
            saw_success = true;
        }
    }
    saw_success
        .then_some(())
        .ok_or(AuthPreflightError::UnsupportedResponse)
}

fn aliyun_response_code(body: &Value) -> Option<&str> {
    body.get("code")
        .or_else(|| body.get("Code"))
        .or_else(|| body.get("error").and_then(|error| error.get("code")))
        .and_then(Value::as_str)
}

fn aliyun_role_exists(body: &Value) -> Option<bool> {
    body.get("data")
        .and_then(|data| data.get("role_exist"))
        .and_then(Value::as_bool)
}

fn classify_aliyun_code(code: &str, model: &str) -> AuthPreflightError {
    let code = code.to_ascii_lowercase();
    if code.contains("invalidaccesskey")
        || code.contains("signaturedoesnotmatch")
        || code.contains("invalidsecuritytoken")
    {
        AuthPreflightError::InvalidCredentials
    } else if code.contains("model")
        && (code.contains("notfound")
            || code.contains("not_found")
            || code.contains("notexist")
            || code.contains("invalid")
            || code.contains("unavailable"))
    {
        AuthPreflightError::ModelUnavailable {
            model: model.to_string(),
        }
    } else if code.contains("nopermission")
        || code.contains("forbidden")
        || code.contains("notauthorized")
    {
        AuthPreflightError::PermissionDenied
    } else if code.contains("throttl") || code.contains("ratelimit") {
        AuthPreflightError::RateLimited
    } else if code.contains("serviceunavailable") || code.contains("internalerror") {
        AuthPreflightError::ProviderUnavailable
    } else {
        AuthPreflightError::UnsupportedResponse
    }
}

async fn send(builder: reqwest::RequestBuilder) -> Result<Response, AuthPreflightError> {
    builder.send().await.map_err(|error| {
        if error.is_timeout() {
            AuthPreflightError::Timeout
        } else {
            AuthPreflightError::EndpointUnreachable
        }
    })
}

async fn response_json(response: Response) -> Result<Value, AuthPreflightError> {
    let bytes = response_bytes(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| AuthPreflightError::UnsupportedResponse)
}

async fn response_bytes(response: Response) -> Result<Vec<u8>, AuthPreflightError> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                AuthPreflightError::Timeout
            } else {
                AuthPreflightError::UnsupportedResponse
            }
        })?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(AuthPreflightError::UnsupportedResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn classify_status(status: StatusCode) -> AuthPreflightError {
    match status {
        StatusCode::UNAUTHORIZED => AuthPreflightError::InvalidCredentials,
        StatusCode::FORBIDDEN => AuthPreflightError::PermissionDenied,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => AuthPreflightError::Timeout,
        StatusCode::TOO_MANY_REQUESTS => AuthPreflightError::RateLimited,
        status if status.is_server_error() => AuthPreflightError::ProviderUnavailable,
        _ => AuthPreflightError::UnsupportedResponse,
    }
}

fn endpoint_url(base_url: &str, suffix: &[&str]) -> Result<Url, AuthPreflightError> {
    let mut url = Url::parse(base_url).map_err(|_| AuthPreflightError::EndpointUnreachable)?;
    url.set_query(None);
    url.set_fragment(None);
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| AuthPreflightError::EndpointUnreachable)?;
    segments.pop_if_empty();
    for segment in suffix {
        segments.push(segment);
    }
    drop(segments);
    Ok(url)
}

fn bearer(api_key: &str) -> String {
    format!("Bearer {api_key}")
}

fn sign_acs3(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
    access_key_id: &str,
    access_key_secret: &str,
) -> String {
    let mut sorted = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_headers = sorted
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let signed_headers = sorted
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical =
        format!("{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let string_to_sign = format!("ACS3-HMAC-SHA256\n{}", hex_sha256(canonical.as_bytes()));
    // HMAC-SHA256 accepts keys of any length, so `InvalidLength` is unreachable here.
    let mut mac = match Hmac::<Sha256>::new_from_slice(access_key_secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => unreachable!("HMAC-SHA256 accepts keys of any length"),
    };
    mac.update(string_to_sign.as_bytes());
    format!(
        "ACS3-HMAC-SHA256 Credential={access_key_id},SignedHeaders={signed_headers},Signature={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

fn hex_sha256(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    use super::*;

    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
        delay: Duration,
    }

    impl Reply {
        fn json(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                body: body.into(),
                headers: Vec::new(),
                delay: Duration::ZERO,
            }
        }
    }

    struct MockServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        /// How long the mock waits for a connection that may never come.
        ///
        /// A client that gives up before it dials — `request_timeout_is_classified`
        /// uses a 10ms timeout, which under a saturated CPU can fire before the
        /// request future is ever polled — leaves nothing to accept. A blocking
        /// accept would then leave the thread alive forever and `finish()`'s
        /// `join()` would stall the whole test binary rather than failing one
        /// test.
        const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);

        fn spawn(replies: Vec<Reply>) -> Self {
            Self::spawn_with_accept_timeout(replies, Self::ACCEPT_TIMEOUT)
        }

        fn spawn_with_accept_timeout(replies: Vec<Reply>, accept_timeout: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let address = listener.local_addr().expect("mock address");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener nonblocking");
                for reply in replies {
                    let deadline = Instant::now() + accept_timeout;
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                                if Instant::now() >= deadline {
                                    // No client arrived. Leave the recorded
                                    // requests short so an assertion fails
                                    // instead of the suite hanging.
                                    return;
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(err) => panic!("accept mock request: {err}"),
                        }
                    };
                    // `read_request` relies on a read timeout, which needs a
                    // blocking socket; accepted sockets can inherit the
                    // listener's non-blocking flag.
                    stream.set_nonblocking(false).expect("mock stream blocking");
                    let request = read_request(&mut stream);
                    captured.lock().unwrap().push(request);
                    if !reply.delay.is_zero() {
                        thread::sleep(reply.delay);
                    }
                    let reason = match reply.status {
                        200 => "OK",
                        302 => "Found",
                        401 => "Unauthorized",
                        403 => "Forbidden",
                        404 => "Not Found",
                        405 => "Method Not Allowed",
                        429 => "Too Many Requests",
                        500 => "Internal Server Error",
                        501 => "Not Implemented",
                        _ => "Test",
                    };
                    let extra_headers = reply
                        .headers
                        .iter()
                        .map(|(name, value)| format!("{name}: {value}\r\n"))
                        .collect::<String>();
                    let response = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                        reply.status,
                        reason,
                        reply.body.len(),
                        extra_headers,
                        reply.body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                thread: Some(thread),
            }
        }

        fn finish(mut self) -> Vec<String> {
            self.thread.take().unwrap().join().expect("mock thread");
            Arc::try_unwrap(self.requests)
                .expect("request records")
                .into_inner()
                .unwrap()
        }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).unwrap_or(0);
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn provider(base_url: &str, provider_type: &str) -> ResolvedProvider {
        ResolvedProvider {
            base_url: if provider_type == "aliyun" {
                "http://127.0.0.1:1".to_string()
            } else {
                base_url.to_string()
            },
            sysom_endpoint: base_url.to_string(),
            api_key: "sk-private-value".to_string(),
            model: "test-model".to_string(),
            provider_type: provider_type.to_string(),
            auth_source: None,
            extra_params: None,
            access_key_id: String::new(),
            access_key_secret: String::new(),
            security_token: None,
            explicit_cache: false,
        }
    }

    #[test]
    fn endpoint_error_messages_are_provider_neutral() {
        for (error, code) in [
            (
                AuthPreflightError::EndpointUnreachable,
                "endpoint_unreachable",
            ),
            (AuthPreflightError::Timeout, "timeout"),
            (
                AuthPreflightError::ProviderUnavailable,
                "provider_unavailable",
            ),
            (
                AuthPreflightError::UnsupportedResponse,
                "unsupported_response",
            ),
        ] {
            let message = error.to_string();
            assert_eq!(error.code(), code);
            assert!(
                message.contains("endpoint configuration"),
                "{code}: {message}"
            );
            assert!(!message.contains("Base URL"), "{code}: {message}");
            assert!(!message.contains("sysom_endpoint"), "{code}: {message}");
        }
    }

    #[tokio::test]
    async fn aliyun_connection_failure_uses_neutral_endpoint_hint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut aliyun = provider(&format!("http://{address}"), "aliyun");
        aliyun.access_key_id = "test-access-key".to_string();
        aliyun.access_key_secret = "test-secret".to_string();

        let error = preflight_auth(&aliyun)
            .await
            .expect_err("unreachable SysOM endpoint");
        assert_eq!(error, AuthPreflightError::EndpointUnreachable);
        let message = error.to_string();
        assert!(message.contains("endpoint configuration"), "{message}");
        assert!(!message.contains("Base URL"), "{message}");
    }

    #[tokio::test]
    async fn model_endpoint_success_validates_credentials_and_model() {
        let server = MockServer::spawn(vec![Reply::json(
            200,
            r#"{"id":"test-model","object":"model"}"#,
        )]);
        preflight_auth(&provider(&server.base_url, "dashscope"))
            .await
            .expect("valid preflight");
        let requests = server.finish();
        assert!(requests[0].starts_with("GET /v1/models/test-model HTTP/1.1"));
        assert!(requests[0].contains("authorization: Bearer sk-private-value"));
    }

    #[tokio::test]
    async fn classifies_auth_rate_limit_and_provider_failures() {
        for (status, expected) in [
            (401, AuthPreflightError::InvalidCredentials),
            (403, AuthPreflightError::PermissionDenied),
            (429, AuthPreflightError::RateLimited),
            (500, AuthPreflightError::ProviderUnavailable),
        ] {
            let server = MockServer::spawn(vec![Reply::json(status, r#"{"error":{}}"#)]);
            let result = preflight_auth(&provider(&server.base_url, "dashscope")).await;
            assert_eq!(result, Err(expected), "status {status}");
            server.finish();
        }
    }

    #[tokio::test]
    async fn dashscope_list_fallback_still_validates_credentials_with_chat() {
        let server = MockServer::spawn(vec![
            Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
            Reply::json(
                200,
                r#"{"object":"list","data":[{"id":"vendor/test-model","object":"model"}]}"#,
            ),
            Reply::json(200, r#"{"choices":[{"message":{"content":""}}]}"#),
        ]);
        let mut dashscope = provider(&server.base_url, "dashscope");
        dashscope.model = "vendor/test-model".to_string();
        preflight_auth(&dashscope)
            .await
            .expect("model list fallback succeeds");
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v1/models/vendor%2Ftest-model HTTP/1.1"));
        assert!(requests[1].starts_with("GET /v1/models HTTP/1.1"));
        assert!(!requests[1].contains("messages"));
        assert!(requests[2].starts_with("POST /v1/chat/completions HTTP/1.1"));
    }

    #[tokio::test]
    async fn openai_compat_falls_back_for_ambiguous_model_endpoint_failures() {
        for status in [400, 404, 405, 501] {
            let server = MockServer::spawn(vec![
                Reply::json(status, r#"{"error":{"code":"route_not_found"}}"#),
                Reply::json(200, r#"{"choices":[{"message":{"content":""}}]}"#),
            ]);
            preflight_auth(&provider(&server.base_url, "openai_compat"))
                .await
                .expect("chat fallback succeeds");
            let requests = server.finish();
            assert_eq!(requests.len(), 2);
            assert!(requests[1].starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(requests[1].contains(r#""stream":false"#));
            assert!(requests[1].contains(r#""max_tokens":1"#));
        }
    }

    #[tokio::test]
    async fn known_read_only_providers_fall_back_to_model_list_without_chat() {
        for provider_type in ["openai", "generic", "deepseek"] {
            let server = MockServer::spawn(vec![
                Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
                Reply::json(200, r#"{"object":"list","data":[{"id":"test-model"}]}"#),
            ]);
            preflight_auth(&provider(&server.base_url, provider_type))
                .await
                .expect("model list fallback succeeds");
            let requests = server.finish();
            assert_eq!(requests.len(), 2, "provider type {provider_type}");
            assert!(requests[1].starts_with("GET /v1/models HTTP/1.1"));
            assert!(!requests[1].contains("messages"));
        }
    }

    #[tokio::test]
    async fn unknown_provider_type_does_not_fall_back() {
        let server = MockServer::spawn(vec![Reply::json(
            404,
            r#"{"error":{"code":"route_not_found"}}"#,
        )]);
        assert_eq!(
            preflight_auth(&provider(&server.base_url, "custom-provider")).await,
            Err(AuthPreflightError::ModelUnavailable {
                model: "test-model".to_string()
            })
        );
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn chat_fallback_classifies_explicit_model_errors_from_400_and_422() {
        for status in [400, 422] {
            let server = MockServer::spawn(vec![
                Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
                Reply::json(
                    status,
                    r#"{"error":{"code":"model_not_found","param":"model"}}"#,
                ),
            ]);
            assert_eq!(
                preflight_auth(&provider(&server.base_url, "openai_compat")).await,
                Err(AuthPreflightError::ModelUnavailable {
                    model: "test-model".to_string()
                }),
                "status {status}"
            );
            assert_eq!(server.finish().len(), 2);
        }
    }

    #[tokio::test]
    async fn plan_provider_list_and_chat_validate_model_and_credentials() {
        for status in [404, 405, 501] {
            let server = MockServer::spawn(vec![
                Reply::json(status, r#"{"error":{"code":"route_not_found"}}"#),
                Reply::json(
                    200,
                    r#"{"object":"list","data":[{"id":"test-model","object":"model"}]}"#,
                ),
                Reply::json(200, r#"{"choices":[{"message":{"content":""}}]}"#),
            ]);
            preflight_auth(&provider(&server.base_url, "coding_plan"))
                .await
                .expect("plan fallback succeeds");
            let requests = server.finish();
            assert_eq!(requests.len(), 3);
            assert!(requests[0].starts_with("GET /v1/models/test-model HTTP/1.1"));
            assert!(requests[1].starts_with("GET /v1/models HTTP/1.1"));
            assert!(requests[2].starts_with("POST /v1/chat/completions HTTP/1.1"));
            for request in &requests {
                assert!(request.contains("user-agent: Cosh/"));
                assert!(request.contains("x-dashscope-useragent: Cosh/"));
                assert!(request.contains("x-dashscope-authtype: openai"));
            }
        }
    }

    #[tokio::test]
    async fn public_model_list_does_not_accept_invalid_credentials() {
        for provider_type in ["dashscope", "coding_plan"] {
            let server = MockServer::spawn(vec![
                Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
                Reply::json(
                    200,
                    r#"{"object":"list","data":[{"id":"test-model","object":"model"}]}"#,
                ),
                Reply::json(401, r#"{"error":{"code":"invalid_api_key"}}"#),
            ]);
            assert_eq!(
                preflight_auth(&provider(&server.base_url, provider_type)).await,
                Err(AuthPreflightError::InvalidCredentials),
                "provider type {provider_type}"
            );
            assert_eq!(server.finish().len(), 3);
        }
    }

    #[tokio::test]
    async fn plan_provider_uses_chat_when_model_list_route_is_unavailable() {
        let server = MockServer::spawn(vec![
            Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
            Reply::json(405, r#"{"error":"coding agents only"}"#),
            Reply::json(200, r#"{"choices":[{"message":{"content":""}}]}"#),
        ]);
        preflight_auth(&provider(&server.base_url, "token_plan"))
            .await
            .expect("chat fallback succeeds");
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].starts_with("POST /v1/chat/completions HTTP/1.1"));
    }

    /// The real token-plan gateway answers GET /models/{model} with HTTP 400
    /// "Model not exist." even for models its own model list contains and
    /// chat serves, so the retrieve-route 400 must defer to the list+chat
    /// chain instead of ending validation.
    #[tokio::test]
    async fn token_plan_retrieve_bad_request_defers_to_list_and_chat() {
        let server = MockServer::spawn(vec![
            Reply::json(
                400,
                r#"{"code":"InvalidParameter","message":"Model not exist.","request_id":"tp"}"#,
            ),
            Reply::json(
                200,
                r#"{"object":"list","data":[{"id":"qwen3.8-max","object":"model"}]}"#,
            ),
            Reply::json(200, r#"{"choices":[{"message":{"content":""}}]}"#),
        ]);
        let mut token_plan = provider(&server.base_url, "token_plan");
        token_plan.model = "qwen3.8-max".to_string();

        preflight_auth(&token_plan)
            .await
            .expect("list and chat are the authority");
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v1/models/qwen3.8-max HTTP/1.1"));
        assert!(requests[1].starts_with("GET /v1/models HTTP/1.1"));
        assert!(requests[2].starts_with("POST /v1/chat/completions HTTP/1.1"));
    }

    #[tokio::test]
    async fn plan_model_list_rejects_an_unavailable_model() {
        let server = MockServer::spawn(vec![
            Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
            Reply::json(
                200,
                r#"{"object":"list","data":[{"id":"another-model","object":"model"}]}"#,
            ),
        ]);
        assert_eq!(
            preflight_auth(&provider(&server.base_url, "token_plan")).await,
            Err(AuthPreflightError::ModelUnavailable {
                model: "test-model".to_string()
            })
        );
        assert_eq!(server.finish().len(), 2);
    }

    #[tokio::test]
    async fn model_list_can_exceed_the_old_64_kib_limit() {
        let padding = "x".repeat(70 * 1024);
        let body = format!(
            r#"{{"metadata":"{padding}","data":[{{"id":"test-model","object":"model"}}]}}"#
        );
        let server = MockServer::spawn(vec![
            Reply::json(404, r#"{"error":{"code":"route_not_found"}}"#),
            Reply::json(200, body),
        ]);
        preflight_auth(&provider(&server.base_url, "deepseek"))
            .await
            .expect("bounded large model list succeeds");
        assert_eq!(server.finish().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn preflight_phases_share_one_total_deadline() {
        let two_phases = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(())
        };

        assert_eq!(
            enforce_preflight_deadline(Duration::from_millis(250), two_phases).await,
            Err(AuthPreflightError::Timeout)
        );
    }

    #[tokio::test]
    async fn explicit_model_not_found_does_not_fall_back() {
        let server = MockServer::spawn(vec![Reply::json(
            404,
            r#"{"error":{"code":"model_not_found","param":"model"}}"#,
        )]);
        assert!(matches!(
            preflight_auth(&provider(&server.base_url, "openai_compat")).await,
            Err(AuthPreflightError::ModelUnavailable { .. })
        ));
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn connection_failure_is_endpoint_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert_eq!(
            preflight_auth(&provider(&format!("http://{address}/v1"), "dashscope")).await,
            Err(AuthPreflightError::EndpointUnreachable)
        );
    }

    /// `finish()` must return even when no client ever connects.
    ///
    /// This is the hang `request_timeout_is_classified` triggered: a 10ms client
    /// timeout can fire before the request future is first polled, so nothing is
    /// ever dialed. With a blocking accept the mock thread never exits and
    /// `join()` stalls the entire test binary — one flaky test becomes a hung
    /// suite with no failing assertion to point at.
    #[test]
    fn mock_server_finish_returns_when_no_client_connects() {
        let accept_timeout = Duration::from_millis(200);
        let server = MockServer::spawn_with_accept_timeout(
            vec![Reply::json(200, r#"{"id":"unused"}"#)],
            accept_timeout,
        );

        let started = Instant::now();
        let requests = server.finish();
        let elapsed = started.elapsed();

        assert!(requests.is_empty(), "no request should have been recorded");
        assert!(
            elapsed < accept_timeout * 10,
            "finish() blocked for {elapsed:?}, accept timeout was {accept_timeout:?}"
        );
    }

    #[tokio::test]
    async fn request_timeout_is_classified() {
        let server = MockServer::spawn(vec![Reply {
            delay: Duration::from_millis(50),
            ..Reply::json(200, r#"{"id":"test-model"}"#)
        }]);
        assert_eq!(
            preflight_auth_with_timeout(
                &provider(&server.base_url, "dashscope"),
                Duration::from_millis(10),
            )
            .await,
            Err(AuthPreflightError::Timeout)
        );
        server.finish();
    }

    #[tokio::test]
    async fn redirect_is_not_followed_and_does_not_forward_credentials() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_address = target.local_addr().unwrap();
        let server = MockServer::spawn(vec![Reply {
            headers: vec![(
                "Location".to_string(),
                format!("http://{target_address}/steal"),
            )],
            ..Reply::json(302, "")
        }]);
        assert_eq!(
            preflight_auth(&provider(&server.base_url, "dashscope")).await,
            Err(AuthPreflightError::UnsupportedResponse)
        );
        let source_requests = server.finish();
        assert!(source_requests[0].contains("authorization: Bearer sk-private-value"));
        thread::sleep(Duration::from_millis(100));
        assert!(
            target.accept().is_err(),
            "redirect target received credentials"
        );
    }

    #[tokio::test]
    async fn aliyun_manual_credentials_validate_runtime_permission_and_model() {
        let server = MockServer::spawn(vec![
            Reply::json(200, r#"{"code":"Success","data":{"role_exist":true}}"#),
            Reply::json(
                200,
                "event: OK\ndata: {\"choices\":[{\"message\":{\"content\":\"\"}}]}\n\n",
            ),
        ]);
        let mut aliyun = provider(&server.base_url, "aliyun");
        aliyun.api_key.clear();
        aliyun.access_key_id = "test-access-key".to_string();
        aliyun.access_key_secret = "test-secret".to_string();

        preflight_auth(&aliyun)
            .await
            .expect("signed permission probe");
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /api/v1/openapi/initial HTTP/1.1"));
        assert!(requests[0].contains("x-acs-action: InitialSysom"));
        assert!(requests[0].contains("x-acs-version: 2023-12-30"));
        assert!(requests[0].contains(r#"{"check_only":true,"source":"cosh"}"#));
        assert!(requests[0].contains("authorization: ACS3-HMAC-SHA256 Credential=test-access-key"));
        assert!(!requests[0].contains("test-secret"));
        assert!(!requests[0].contains("messages"));
        assert!(requests[1]
            .starts_with("POST /api/v1/copilot/generate_copilot_stream_response HTTP/1.1"));
        assert!(requests[1].contains("x-acs-action: GenerateCopilotStreamResponse"));
        assert!(requests[1].contains("authorization: ACS3-HMAC-SHA256 Credential=test-access-key"));
        assert!(requests[1].contains(r#"\"model\":\"test-model\""#));
        assert!(requests[1].contains(r#"\"max_tokens\":1"#));
        assert!(!requests[1].contains("test-secret"));
    }

    #[tokio::test]
    async fn aliyun_permission_probe_preserves_permission_classification() {
        let server = MockServer::spawn(vec![Reply::json(
            200,
            r#"{"code":"NoPermission","message":"redacted by preflight"}"#,
        )]);
        let mut aliyun = provider(&server.base_url, "aliyun");
        aliyun.api_key.clear();
        aliyun.access_key_id = "test-access-key".to_string();
        aliyun.access_key_secret = "test-secret".to_string();

        assert_eq!(
            preflight_auth(&aliyun).await,
            Err(AuthPreflightError::PermissionDenied)
        );
        server.finish();
    }

    #[tokio::test]
    async fn aliyun_permission_probe_rejects_a_missing_service_role() {
        let server = MockServer::spawn(vec![Reply::json(
            200,
            r#"{"code":"Success","data":{"role_exist":false}}"#,
        )]);
        let mut aliyun = provider(&server.base_url, "aliyun");
        aliyun.api_key.clear();
        aliyun.access_key_id = "test-access-key".to_string();
        aliyun.access_key_secret = "test-secret".to_string();

        assert_eq!(
            preflight_auth(&aliyun).await,
            Err(AuthPreflightError::ServiceNotReady)
        );
        server.finish();
    }

    #[tokio::test]
    async fn aliyun_non_success_status_preserves_body_error_classification() {
        let server = MockServer::spawn(vec![Reply::json(
            404,
            r#"{"Code":"InvalidAccessKeyId.NotFound"}"#,
        )]);
        let mut aliyun = provider(&server.base_url, "aliyun");
        aliyun.api_key.clear();
        aliyun.access_key_id = "test-access-key".to_string();
        aliyun.access_key_secret = "test-secret".to_string();

        assert_eq!(
            preflight_auth(&aliyun).await,
            Err(AuthPreflightError::InvalidCredentials)
        );
        server.finish();
    }

    #[tokio::test]
    async fn aliyun_copilot_permission_failure_rejects_candidate() {
        for reply in [
            Reply::json(403, r#"{"Code":"NoPermission"}"#),
            Reply::json(200, r#"{"Code":"NoPermission"}"#),
        ] {
            let server = MockServer::spawn(vec![
                Reply::json(200, r#"{"code":"Success","data":{"role_exist":true}}"#),
                reply,
            ]);
            let mut aliyun = provider(&server.base_url, "aliyun");
            aliyun.api_key.clear();
            aliyun.access_key_id = "test-access-key".to_string();
            aliyun.access_key_secret = "test-secret".to_string();

            assert_eq!(
                preflight_auth(&aliyun).await,
                Err(AuthPreflightError::PermissionDenied)
            );
            assert_eq!(server.finish().len(), 2);
        }
    }

    #[tokio::test]
    async fn aliyun_copilot_model_failure_rejects_candidate() {
        for reply in [
            Reply::json(400, r#"{"Code":"ModelNotFound"}"#),
            Reply::json(
                200,
                "event: Failed\ndata: {\"error\":{\"code\":\"InvalidModel\"}}\n\n",
            ),
        ] {
            let server = MockServer::spawn(vec![
                Reply::json(200, r#"{"code":"Success","data":{"role_exist":true}}"#),
                reply,
            ]);
            let mut aliyun = provider(&server.base_url, "aliyun");
            aliyun.api_key.clear();
            aliyun.access_key_id = "test-access-key".to_string();
            aliyun.access_key_secret = "test-secret".to_string();

            assert_eq!(
                preflight_auth(&aliyun).await,
                Err(AuthPreflightError::ModelUnavailable {
                    model: "test-model".to_string()
                })
            );
            assert_eq!(server.finish().len(), 2);
        }
    }

    #[test]
    fn errors_never_render_credentials_or_response_bodies() {
        for error in [
            AuthPreflightError::InvalidCredentials,
            AuthPreflightError::PermissionDenied,
            AuthPreflightError::ModelUnavailable {
                model: "test-model".to_string(),
            },
            AuthPreflightError::ServiceNotReady,
            AuthPreflightError::CredentialSourceUnavailable,
            AuthPreflightError::UnsupportedResponse,
        ] {
            let message = error.to_string();
            assert!(!message.contains("sk-private-value"));
            assert!(!message.contains("Authorization"));
            assert!(!message.contains("response body"));
        }
    }
}
