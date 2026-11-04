//! SysOM Provider — Aliyun SysOM `generate_copilot_stream_response` API client.
//!
//! Uses ACS3-HMAC-SHA256 signing and parses the cumulative SSE stream format
//! into incremental `GenerateEvent`s compatible with `ContentGenerator` trait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use uuid::Uuid;

use super::{ContentGenerator, GenerateConfig, GenerateStream, Message, ToolDeclaration};

use self::stream::{sysom_event_stream, sysom_event_stream_structured};
use super::{DispatchState, GenerateEvent, ProviderFailure, ProviderFailureKind};

pub use self::ecs_metadata::{probe_ecs_ram_role, CredentialStatus, NotReadyReason, ProbeError};

pub(super) mod credential_manager;
pub(super) mod ecs_identity;
mod ecs_metadata;
pub mod endpoint;
mod failure;
#[cfg(test)]
mod identity_provider_tests;
#[cfg(test)]
mod identity_test_support;
mod stream;

type HmacSha256 = Hmac<Sha256>;

const API_PATH: &str = "/api/v1/copilot/generate_copilot_stream_response";
const API_VERSION: &str = "2023-12-30";
const API_ACTION: &str = "GenerateCopilotStreamResponse";
const ECS_METADATA_ENDPOINT: &str = "http://100.100.100.200";
const ECS_RAM_ROLE_NAME: &str = "AliyunECSInstanceForSysomRole";
const CONSOLE_URL_TEMPLATE: &str =
    "https://alinux.console.aliyun.com/{regionId}/guide/cosh?instance={instanceId}";

/// Cache TTL for instance_id (3 hours).
const INSTANCE_ID_CACHE_TTL_SECS: u64 = 3 * 3600;
/// Connect timeout for ECS metadata service.
const METADATA_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Connect timeout for the SysOM API.
///
/// Bounds connection setup so an unreachable endpoint surfaces as a prompt
/// error instead of stalling on the protocol stack's own timeout, which was
/// measured at 61 seconds from an ECS with no public egress.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Inactivity timeout for the SysOM API response.
///
/// A *total* request timeout is the wrong instrument: the response is a
/// Server-Sent Events stream whose length belongs to the model, so a deadline
/// would truncate healthy long completions. A read timeout applies per read and
/// resets after each successful one, so it bounds a server that accepts the
/// connection and then goes silent while leaving a steadily streaming response
/// untouched.
///
/// Deliberately generous: the largest legitimate gap is time-to-first-token
/// while the model is queued, and the goal is to turn an unbounded hang into a
/// bounded failure rather than to police latency.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Build the HTTP client for the streaming API.
///
/// Both bounds are parameters so a test can assert the stalled-connection
/// behaviour without waiting the production read timeout.
fn build_streaming_client(
    endpoint: &endpoint::ResolvedEndpoint,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<reqwest::Client, String> {
    build_streaming_client_for_source(endpoint, connect_timeout, read_timeout, false)
}

fn build_streaming_client_for_source(
    endpoint: &endpoint::ResolvedEndpoint,
    connect_timeout: Duration,
    read_timeout: Duration,
    identity: bool,
) -> Result<reqwest::Client, String> {
    let builder = endpoint
        .configure_client(reqwest::Client::builder())
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout);
    let builder = if identity {
        builder
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    } else {
        builder
    };
    builder
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcsAuthChallenge {
    pub instance_id: String,
    pub console_url: String,
}

/// Credentials that can be refreshed at runtime (for STS).
#[derive(Clone)]
pub(super) struct SysomCredentials {
    access_key_id: String,
    access_key_secret: String,
    security_token: Option<String>,
}

impl std::fmt::Debug for SysomCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SysomCredentials([REDACTED])")
    }
}

/// SysOM Provider that connects to Aliyun SysOM API with ACS3-HMAC-SHA256 signing.
pub struct SysomProvider {
    /// Explicit endpoint override from configuration; empty when unset.
    ///
    /// The effective host is resolved per request rather than here because
    /// these constructors are synchronous yet run inside the tokio runtime: a
    /// blocking reachability probe would occupy a worker thread. Resolution is
    /// cached process-wide, so the probe still runs at most once.
    configured_endpoint: String,
    credentials: RwLock<SysomCredentials>,
    source: CredentialSource,
    cancelled: Arc<AtomicBool>,
    /// Resolved once per provider instance, including a negative result; only the
    /// on-disk cache honours [`INSTANCE_ID_CACHE_TTL_SECS`].
    instance_id: OnceCell<Option<String>>,
}

enum CredentialSource {
    Static,
    EcsRamRole,
    EcsIdentity(Arc<credential_manager::CredentialManager>),
}

struct IdentityRequest {
    manager: Arc<credential_manager::CredentialManager>,
    generation: u64,
    expires_at: tokio::time::Instant,
    deadline: tokio::time::Instant,
}

impl IdentityRequest {
    fn new(
        manager: Arc<credential_manager::CredentialManager>,
        lease: &credential_manager::CredentialLease,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            manager,
            generation: lease.generation,
            expires_at: lease.expires_at,
            deadline,
        }
    }
}

#[derive(Debug)]
enum SysomRequestError {
    Legacy(String),
    LeaseExpired,
}

impl SysomProvider {
    pub fn new(
        access_key_id: &str,
        access_key_secret: &str,
        security_token: Option<&str>,
        configured_endpoint: &str,
    ) -> Self {
        Self {
            configured_endpoint: configured_endpoint.to_string(),
            credentials: RwLock::new(SysomCredentials {
                access_key_id: access_key_id.to_string(),
                access_key_secret: access_key_secret.to_string(),
                security_token: security_token.map(|s| s.to_string()),
            }),
            source: CredentialSource::Static,
            cancelled: Arc::new(AtomicBool::new(false)),
            instance_id: OnceCell::new(),
        }
    }

    pub fn from_ecs_ram_role(configured_endpoint: &str) -> Self {
        Self {
            configured_endpoint: configured_endpoint.to_string(),
            credentials: RwLock::new(SysomCredentials {
                access_key_id: String::new(),
                access_key_secret: String::new(),
                security_token: None,
            }),
            source: CredentialSource::EcsRamRole,
            cancelled: Arc::new(AtomicBool::new(false)),
            instance_id: OnceCell::new(),
        }
    }

    pub fn from_ecs_identity(configured_endpoint: &str) -> Self {
        let mut provider = Self::new("", "", None, configured_endpoint);
        provider.source = CredentialSource::EcsIdentity(Arc::new(
            credential_manager::CredentialManager::new(configured_endpoint.into()),
        ));
        provider
    }

    pub fn from_ecs_identity_with_admission(
        configured_endpoint: &str,
        check: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let mut provider = Self::new("", "", None, configured_endpoint);
        provider.source = CredentialSource::EcsIdentity(Arc::new(
            credential_manager::CredentialManager::with_admission(
                configured_endpoint.into(),
                check,
            ),
        ));
        provider
    }

    /// Build the JSON request body for the SysOM API.
    fn build_request_body(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<Value, String> {
        // Fields this builder constructs below. `tool_choice` and
        // `instance_id` are owned here but not by the generic OpenAI-compatible
        // builder, so the set is enumerated per provider.
        const OWNED_REQUEST_FIELDS: [&str; 8] = [
            "messages",
            "model",
            "stream",
            "use_dashscope",
            "version",
            "tools",
            "tool_choice",
            "instance_id",
        ];
        super::reject_owned_field_overrides(&OWNED_REQUEST_FIELDS, config.extra_params.as_ref())?;

        let mut inner = serde_json::json!({
            "messages": messages,
            "model": config.model,
            "stream": true,
            "use_dashscope": true,
            "version": 2,
            "max_tokens": config.max_tokens,
        });

        if let Some(temp) = config.temperature {
            inner["temperature"] = serde_json::json!(temp);
        }

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            inner["tools"] = serde_json::json!(tool_defs);
            inner["tool_choice"] = serde_json::json!("auto");
        }

        if let Some(extra) = &config.extra_params {
            if let (Some(inner_obj), Some(extra_obj)) = (inner.as_object_mut(), extra.as_object()) {
                for (k, v) in extra_obj {
                    inner_obj.insert(k.clone(), v.clone());
                }
            }
        }

        if let Some(Some(id)) = self.instance_id.get() {
            inner["instance_id"] = serde_json::json!(id);
        }

        // Last word on the output cap: the `extra_params` merge above may have
        // replaced the resolved `max_tokens`, which would let the wire request
        // outspend the reserve the compaction budget priced it against (#2240).
        super::clamp_output_cap_fields(&mut inner, config.max_tokens);

        // Wrap in llmParamString
        Ok(serde_json::json!({
            "llmParamString": inner.to_string()
        }))
    }

    /// Compute ACS3-HMAC-SHA256 authorization header.
    fn sign_request(
        &self,
        method: &str,
        pathname: &str,
        headers: &[(String, String)],
        hashed_payload: &str,
        creds: &SysomCredentials,
    ) -> String {
        let signature_algorithm = "ACS3-HMAC-SHA256";

        // Build canonical headers and signed headers list
        // Sort headers by lowercase key
        let mut sorted_headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
            .collect();
        sorted_headers.sort_by(|a, b| a.0.cmp(&b.0));

        let canonical_headers: String = sorted_headers
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v))
            .collect();

        let signed_headers: Vec<&str> = sorted_headers.iter().map(|(k, _)| k.as_str()).collect();
        let signed_headers_str = signed_headers.join(";");

        // Canonical request
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            pathname,
            "", // query string (empty)
            canonical_headers,
            signed_headers_str,
            hashed_payload,
        );

        // String to sign
        let hashed_canonical = hex_sha256(canonical_request.as_bytes());
        let string_to_sign = format!("{}\n{}", signature_algorithm, hashed_canonical);

        // Signature
        let signature = hex_hmac_sha256(
            creds.access_key_secret.as_bytes(),
            string_to_sign.as_bytes(),
        );

        format!(
            "{} Credential={},SignedHeaders={},Signature={}",
            signature_algorithm, creds.access_key_id, signed_headers_str, signature
        )
    }
}

#[async_trait]
impl ContentGenerator for SysomProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.cancelled.store(false, Ordering::SeqCst);
        if let CredentialSource::EcsIdentity(manager) = &self.source {
            return self
                .generate_identity(messages, tools, config, manager.clone())
                .await;
        }
        self.instance_id.get_or_init(resolve_instance_id).await;

        let body = self.build_request_body(messages, tools, config)?;
        let body_bytes = serde_json::to_vec(&body).map_err(|e| format!("JSON serialize: {e}"))?;

        if matches!(self.source, CredentialSource::EcsRamRole)
            && self.credentials.read().unwrap().security_token.is_none()
            && !self.refresh_sts_credentials().await
        {
            return Err("failed to refresh ECS RAM Role credentials".to_string());
        }

        // First attempt
        match self.do_streaming_request(&body_bytes).await {
            Ok(stream) => Ok(stream),
            Err(e) if matches!(self.source, CredentialSource::EcsRamRole) && is_sts_error(&e) => {
                // STS credential expired — try to refresh from ECS metadata and retry once
                tracing::debug!("STS credential error, attempting refresh...");
                if self.refresh_sts_credentials().await {
                    tracing::debug!("STS credentials refreshed, retrying...");
                    self.do_streaming_request(&body_bytes).await
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl SysomProvider {
    async fn generate_identity(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
        manager: Arc<credential_manager::CredentialManager>,
    ) -> Result<GenerateStream, String> {
        let deadline = tokio::time::Instant::now() + ecs_identity::PREPARATION_TIMEOUT;
        for _ in 0..3 {
            let lease = match tokio::time::timeout_at(deadline, manager.ensure()).await {
                Ok(Ok(lease)) => lease,
                Ok(Err(ecs_identity::IdentityError::LeaseExpired)) => continue,
                result => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => ecs_identity::IdentityError::Timeout,
                    };
                    let kind = if error == ecs_identity::IdentityError::PermissionDenied {
                        ProviderFailureKind::PermissionDenied
                    } else {
                        ProviderFailureKind::Identity
                    };
                    return Ok(failure_stream(failure::fixed(
                        error.code(),
                        kind,
                        DispatchState::NotDispatched,
                    )));
                }
            };
            self.instance_id
                .get_or_init(|| async { lease.instance_id.clone() })
                .await;
            let body = self.build_request_body(messages, tools, config)?;
            let body_bytes =
                serde_json::to_vec(&body).map_err(|error| format!("JSON serialize: {error}"))?;
            let identity = IdentityRequest::new(manager.clone(), &lease, deadline);
            match self
                .send_streaming_request(&body_bytes, lease.credentials, Some(identity))
                .await
            {
                Err(SysomRequestError::LeaseExpired) => manager.invalidate(lease.generation).await,
                Err(SysomRequestError::Legacy(error)) => return Err(error),
                Ok(stream) => return Ok(stream),
            }
        }
        Ok(failure_stream(failure::fixed(
            ecs_identity::IdentityError::LeaseExpired.code(),
            ProviderFailureKind::Identity,
            DispatchState::NotDispatched,
        )))
    }

    /// Send a streaming request using current credentials.
    async fn do_streaming_request(&self, body_bytes: &[u8]) -> Result<GenerateStream, String> {
        let creds = self.credentials.read().unwrap().clone();
        self.send_streaming_request(body_bytes, creds, None)
            .await
            .map_err(|error| match error {
                SysomRequestError::Legacy(error) => error,
                SysomRequestError::LeaseExpired => "Sysom.Identity.LeaseExpired".into(),
            })
    }

    async fn send_streaming_request(
        &self,
        body_bytes: &[u8],
        creds: SysomCredentials,
        identity: Option<IdentityRequest>,
    ) -> Result<GenerateStream, SysomRequestError> {
        if identity.as_ref().is_some_and(|context| {
            context
                .expires_at
                .saturating_duration_since(tokio::time::Instant::now())
                < ecs_identity::MINIMUM_SEND_TTL
        }) {
            return Err(SysomRequestError::LeaseExpired);
        }
        let resolved = if let Some(context) = &identity {
            match tokio::time::timeout_at(
                context.deadline,
                endpoint::resolve(&self.configured_endpoint),
            )
            .await
            {
                Ok(endpoint) => endpoint,
                Err(_) => {
                    return Ok(failure_stream(failure::fixed(
                        ecs_identity::IdentityError::Timeout.code(),
                        ProviderFailureKind::Identity,
                        DispatchState::NotDispatched,
                    )))
                }
            }
        } else {
            endpoint::resolve(&self.configured_endpoint).await
        };
        if identity.is_some() {
            let trusted = ecs_identity::trusted_endpoint(&resolved);
            #[cfg(test)]
            let trusted = trusted
                || reqwest::Url::parse(&resolved.base_url())
                    .ok()
                    .and_then(|url| {
                        url.host_str()
                            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                    })
                    .is_some_and(|ip| ip.is_loopback());
            if !trusted {
                return Ok(failure_stream(failure::fixed(
                    "Sysom.Identity.UntrustedEndpoint",
                    ProviderFailureKind::Identity,
                    DispatchState::NotDispatched,
                )));
            }
        }
        tracing::debug!(
            host = %resolved.host,
            origin = resolved.origin.as_str(),
            "sysom endpoint"
        );
        let url = format!("{}{}", resolved.base_url(), API_PATH);
        let built_client = if identity.is_some() {
            build_streaming_client_for_source(&resolved, CONNECT_TIMEOUT, READ_TIMEOUT, true)
        } else {
            build_streaming_client(&resolved, CONNECT_TIMEOUT, READ_TIMEOUT)
        };
        let client = match built_client {
            Ok(client) => client,
            Err(error) if identity.is_none() => return Err(SysomRequestError::Legacy(error)),
            Err(_) => {
                return Ok(failure_stream(failure::fixed(
                    "Sysom.TransportError",
                    ProviderFailureKind::Transport,
                    DispatchState::Unknown,
                )))
            }
        };
        let host = resolved.host;
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let nonce = Uuid::new_v4().to_string();
        let hashed_payload = hex_sha256(body_bytes);

        // `host` feeds the signature, the URL and the wire header alike: the
        // gateway recomputes the signature from the Host it receives, so the
        // three must not diverge.
        let mut sign_headers: Vec<(String, String)> = vec![
            ("host".to_string(), host.clone()),
            ("x-acs-version".to_string(), API_VERSION.to_string()),
            ("x-acs-action".to_string(), API_ACTION.to_string()),
            ("x-acs-date".to_string(), timestamp.clone()),
            ("x-acs-signature-nonce".to_string(), nonce.clone()),
            ("x-acs-content-sha256".to_string(), hashed_payload.clone()),
            (
                "content-type".to_string(),
                "application/json; charset=utf-8".to_string(),
            ),
        ];

        if let Some(ref token) = creds.security_token {
            sign_headers.push((
                "x-acs-accesskey-id".to_string(),
                creds.access_key_id.clone(),
            ));
            sign_headers.push(("x-acs-security-token".to_string(), token.clone()));
        }

        let authorization =
            self.sign_request("POST", API_PATH, &sign_headers, &hashed_payload, &creds);

        let mut req = client
            .post(&url)
            .header("host", &host)
            .header("x-acs-version", API_VERSION)
            .header("x-acs-action", API_ACTION)
            .header("x-acs-date", &timestamp)
            .header("x-acs-signature-nonce", &nonce)
            .header("x-acs-content-sha256", &hashed_payload)
            .header("content-type", "application/json; charset=utf-8")
            .header("accept", "text/event-stream")
            .header("x-sysom-invoke-source", "cosh")
            .header("Authorization", &authorization);

        if let Some(ref token) = creds.security_token {
            req = req
                .header("x-acs-accesskey-id", &creds.access_key_id)
                .header("x-acs-security-token", token);
        }

        let req = req.body(body_bytes.to_vec());
        if let Some(identity) = &identity {
            let admission =
                tokio::time::timeout_at(identity.deadline, identity.manager.check_admission())
                    .await
                    .unwrap_or(Err(ecs_identity::IdentityError::Timeout));
            if let Err(error) = admission {
                return Ok(failure_stream(failure::fixed(
                    error.code(),
                    ProviderFailureKind::Identity,
                    DispatchState::NotDispatched,
                )));
            }
        }

        if let Some(context) = &identity {
            if context
                .expires_at
                .saturating_duration_since(tokio::time::Instant::now())
                < ecs_identity::MINIMUM_SEND_TTL
            {
                return Err(SysomRequestError::LeaseExpired);
            }
            if tokio::time::Instant::now() >= context.deadline {
                return Ok(failure_stream(failure::fixed(
                    ecs_identity::IdentityError::Timeout.code(),
                    ProviderFailureKind::Identity,
                    DispatchState::NotDispatched,
                )));
            }
        }

        // Report the request before it is encrypted; see `provider::observe` for
        // why an out-of-process observer cannot read it off the wire.
        super::observe::tap_request("POST", &url, body_bytes);

        let response = match req.send().await {
            Ok(response) => response,
            Err(error) if identity.is_none() => {
                return Err(SysomRequestError::Legacy(format!(
                    "HTTP request failed: {error}"
                )))
            }
            Err(_) => {
                return Ok(failure_stream(failure::fixed(
                    "Sysom.TransportError",
                    ProviderFailureKind::Transport,
                    DispatchState::Unknown,
                )))
            }
        };

        // Announce the response before the status check, mirroring
        // `openai_compat`: an error response must still be reported or the
        // observer is left with a pending request that never completes.
        let status = response.status();
        let response_is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_owned();
        super::observe::tap_response_head(status.as_u16(), &content_type);
        if !status.is_success()
            || content_type
                .to_ascii_lowercase()
                .contains("application/json")
            || (identity.is_some() && !response_is_sse)
        {
            let body = match bounded_error_body(response).await {
                Ok(body) => body,
                Err(kind) => {
                    return Ok(failure_stream(failure::fixed(
                        if kind == ProviderFailureKind::Transport {
                            "Sysom.TransportError"
                        } else {
                            "Sysom.InvalidResponse"
                        },
                        kind,
                        DispatchState::Unknown,
                    )))
                }
            };
            if identity.is_none() && !status.is_success() {
                let text = String::from_utf8_lossy(&body);
                super::observe::tap_response_chunk(&body);
                return Err(SysomRequestError::Legacy(format!(
                    "SysOM API error {status}: {text}"
                )));
            }
            if let Some(failure) = failure::from_payload(&body, DispatchState::NotDispatched) {
                if failure.kind == ProviderFailureKind::CredentialsExpired {
                    if let Some(identity) = &identity {
                        identity.manager.invalidate(identity.generation).await;
                    }
                }
                return Ok(failure_stream(failure));
            }
            return Ok(failure_stream(failure::fixed(
                "Sysom.ProviderFailure",
                ProviderFailureKind::Other,
                DispatchState::Unknown,
            )));
        }

        let cancelled = Arc::clone(&self.cancelled);
        // Normalizing to owned bytes keeps the SSE state machine testable with a
        // plain in-memory stream instead of a live HTTP response.
        //
        // The tap sits here rather than inside `sysom_event_stream` because that
        // function is also driven by in-memory streams in its tests, which would
        // report fixture bytes as if they came off the network.
        let byte_stream = response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| {
                    super::observe::tap_response_chunk(&bytes);
                    bytes.to_vec()
                })
                .map_err(|e| e.to_string())
        });

        if let Some(identity) = identity {
            let manager = identity.manager;
            let generation = identity.generation;
            let stream = sysom_event_stream_structured(Box::pin(byte_stream), cancelled);
            let stream =
                futures::stream::once(async { GenerateEvent::RequestAccepted }).chain(stream);
            Ok(Box::pin(stream.then(move |event|{
                let manager=manager.clone();
                async move {
                    if matches!(&event,GenerateEvent::ProviderFailure(failure) if failure.kind==ProviderFailureKind::CredentialsExpired) { manager.invalidate(generation).await; }
                    event
                }
            })))
        } else {
            Ok(sysom_event_stream(Box::pin(byte_stream), cancelled))
        }
    }

    /// Refresh only with credentials accepted by the same validator as auth preflight.
    async fn refresh_sts_credentials(&self) -> bool {
        match ecs_metadata::load_role_credentials().await {
            Ok(ecs_metadata::RoleCredentials::Ready(credentials)) => {
                *self.credentials.write().unwrap() = credentials;
                true
            }
            Ok(ecs_metadata::RoleCredentials::NotReady(reason)) => {
                tracing::debug!(reason = reason.as_str(), "STS credentials not ready");
                false
            }
            Err(error) => {
                tracing::warn!(error_code = error.code(), "STS metadata refresh failed");
                false
            }
        }
    }
}

fn failure_stream(failure: ProviderFailure) -> GenerateStream {
    Box::pin(futures::stream::once(async {
        GenerateEvent::ProviderFailure(failure)
    }))
}

async fn bounded_error_body(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, ProviderFailureKind> {
    const LIMIT: usize = 64 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > LIMIT as u64)
    {
        return Err(ProviderFailureKind::Other);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProviderFailureKind::Transport)?
    {
        if chunk.len() > LIMIT - body.len() {
            return Err(ProviderFailureKind::Other);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Check if an error string contains STS-related error codes.
fn is_sts_error(error: &str) -> bool {
    error.contains("InvalidSecurityToken")
        || error.contains("SecurityTokenExpired")
        || error.contains("InvalidAccessKeyId")
}

// ---------------------------------------------------------------------------
// Instance ID resolution with local cache
// ---------------------------------------------------------------------------

/// Resolve instance_id: read from local cache if valid, otherwise fetch from
/// ECS metadata service and update the cache.
async fn resolve_instance_id() -> Option<String> {
    let config_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".copilot-shell");
    let cache_path = config_dir.join("instance_id");
    #[cfg(test)]
    let cache_path = metadata_tests::TEST_CACHE_PATH
        .try_with(Clone::clone)
        .unwrap_or(cache_path);
    resolve_instance_id_cached(&cache_path).await
}

async fn resolve_instance_id_cached(cache_path: &std::path::Path) -> Option<String> {
    // Try reading from cache
    if let Ok(metadata) = std::fs::metadata(cache_path) {
        if let Ok(modified) = metadata.modified() {
            let age = modified.elapsed().unwrap_or(Duration::from_secs(u64::MAX));
            if age < Duration::from_secs(INSTANCE_ID_CACHE_TTL_SECS) {
                // Cache is still valid
                let content = std::fs::read_to_string(cache_path).unwrap_or_default();
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    // Empty file = previously failed to fetch
                    return None;
                }
                return Some(trimmed.to_string());
            }
        }
    }

    // Cache miss or expired — fetch from metadata service
    let instance_id = ecs_metadata::fetch_instance_id().await.ok();

    // Write cache (create parent dir if needed)
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = instance_id.as_deref().unwrap_or("");
    if let Err(e) = std::fs::write(cache_path, content) {
        tracing::debug!("failed to write instance_id cache: {e}");
    }

    instance_id
}

/// Detect ECS identity without treating denied or malformed metadata as non-ECS.
///
/// # Errors
/// Returns a safe error for HTTP rejection, invalid metadata, or any identity fetch
/// failure after acquiring a valid token. Only an unreachable or timed-out session
/// start is treated as non-ECS so manual authentication remains available.
pub async fn detect_ecs_auth_challenge() -> Result<Option<EcsAuthChallenge>, ProbeError> {
    let identity = ecs_metadata::fetch_identity().await?;
    Ok(identity.map(|(instance_id, region_id)| EcsAuthChallenge {
        console_url: generate_console_url(&instance_id, &region_id),
        instance_id,
    }))
}

fn region_id_from_zone_id(zone_id: &str) -> Option<String> {
    if zone_id.is_empty() {
        return None;
    }
    if let Some(pos) = zone_id.rfind('-') {
        let suffix = &zone_id[pos + 1..];
        if suffix.len() == 1 && suffix.chars().all(|c| c.is_ascii_lowercase()) {
            return Some(zone_id[..pos].to_string());
        }
    }
    Some(zone_id.to_string())
}

fn generate_console_url(instance_id: &str, region_id: &str) -> String {
    CONSOLE_URL_TEMPLATE
        .replace("{regionId}", region_id)
        .replace("{instanceId}", instance_id)
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn hex_hmac_sha256(key: &[u8], data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
#[path = "sysom/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sysom/metadata_tests.rs"]
mod metadata_tests;
