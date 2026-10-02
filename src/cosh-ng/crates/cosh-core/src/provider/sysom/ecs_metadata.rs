//! Bounded ECS credential checks expose classifications, never metadata or credentials.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::HeaderValue;
use serde_json::Value;
use tokio::time::{timeout_at, Instant};

use super::{SysomCredentials, ECS_METADATA_ENDPOINT, ECS_RAM_ROLE_NAME, METADATA_CONNECT_TIMEOUT};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 4096;
const TOKEN_HEADER: &str = "x-aliyun-ecs-metadata-token";
const TOKEN_TTL_HEADER: &str = "x-aliyun-ecs-metadata-token-ttl-seconds";
const TOKEN_TTL: &str = "60";

// A task-local loopback override cannot escape into production or another test.
#[cfg(test)]
tokio::task_local! {
    pub(super) static TEST_ENDPOINT: String;
}

fn metadata_endpoint() -> String {
    #[cfg(test)]
    if let Ok(endpoint) = TEST_ENDPOINT.try_with(Clone::clone) {
        return endpoint;
    }
    ECS_METADATA_ENDPOINT.to_string()
}

/// Availability of complete, unexpired metadata credentials, not SysOM permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    /// The specified role supplied structurally valid, unexpired credentials.
    Ready,
    /// The role or refreshed credentials are not yet available.
    NotReady(NotReadyReason),
}

/// Conditions for which waiting for role configuration or refresh is appropriate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReadyReason {
    /// The specified role endpoint returned HTTP 404.
    RoleMissing,
    /// Complete credentials have reached their expiration time.
    CredentialsExpired,
}

impl NotReadyReason {
    /// Stable reason sent over the registry protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoleMissing => "role_missing",
            Self::CredentialsExpired => "credentials_expired",
        }
    }
}

/// Safe metadata failures that do not retain upstream errors or response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    /// Metadata access was rejected by HTTP 401 or 403.
    AccessDenied,
    /// The response was too large or did not contain valid credentials.
    InvalidResponse,
    /// The metadata transport failed.
    Unreachable,
    /// The metadata request exceeded its deadline.
    Timeout,
    /// Metadata returned another non-success HTTP status.
    Http,
}

impl ProbeError {
    /// Stable error code shared by verify and configure preflight.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AccessDenied => "metadata_access_denied",
            Self::InvalidResponse => "invalid_metadata_response",
            Self::Unreachable => "metadata_unreachable",
            Self::Timeout => "metadata_timeout",
            Self::Http => "metadata_http_error",
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AccessDenied => "Unable to access ECS instance metadata.",
            Self::InvalidResponse => "ECS instance metadata returned an invalid response.",
            Self::Unreachable => "Unable to reach ECS instance metadata.",
            Self::Timeout => "ECS instance metadata request timed out.",
            Self::Http => "ECS instance metadata returned an HTTP error.",
        })
    }
}

impl std::error::Error for ProbeError {}

// Credentials never leave this owner and the signing provider; public probes get only status.
pub(super) enum RoleCredentials {
    Ready(SysomCredentials),
    NotReady(NotReadyReason),
}

impl RoleCredentials {
    fn status(&self) -> CredentialStatus {
        match self {
            Self::Ready(_) => CredentialStatus::Ready,
            Self::NotReady(reason) => CredentialStatus::NotReady(*reason),
        }
    }
}

#[cfg(test)]
pub(super) fn classify_credentials(
    status: u16,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<CredentialStatus, ProbeError> {
    validate_credentials(status, body, now).map(|credentials| credentials.status())
}

fn validate_credentials(
    status: u16,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<RoleCredentials, ProbeError> {
    if status == 404 {
        return Ok(RoleCredentials::NotReady(NotReadyReason::RoleMissing));
    }
    check_status(status)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ProbeError::InvalidResponse);
    }
    let body: Value = serde_json::from_slice(body).map_err(|_| ProbeError::InvalidResponse)?;
    if body.get("Code").and_then(Value::as_str) != Some("Success") {
        return Err(ProbeError::InvalidResponse);
    }
    let field = |name| {
        body.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or(ProbeError::InvalidResponse)
    };
    let credentials = SysomCredentials {
        access_key_id: field("AccessKeyId")?.to_string(),
        access_key_secret: field("AccessKeySecret")?.to_string(),
        security_token: Some(field("SecurityToken")?.to_string()),
    };
    let expiration = DateTime::parse_from_rfc3339(field("Expiration")?)
        .map_err(|_| ProbeError::InvalidResponse)?;
    if expiration <= now {
        Ok(RoleCredentials::NotReady(
            NotReadyReason::CredentialsExpired,
        ))
    } else {
        Ok(RoleCredentials::Ready(credentials))
    }
}

/// Probe only the fixed ECS metadata address and SysOM role, without returning secrets.
///
/// The three-second total deadline includes token acquisition and all body reads.
/// No redirects, proxies, retries, IMDSv1 fallback, or role enumeration are used.
///
/// # Errors
/// Returns a safe classification for transport, HTTP, or credential format failures.
pub async fn probe_ecs_ram_role() -> Result<CredentialStatus, ProbeError> {
    load_role_credentials()
        .await
        .map(|credentials| credentials.status())
}

pub(super) async fn load_role_credentials() -> Result<RoleCredentials, ProbeError> {
    load_role_credentials_at(&metadata_endpoint()).await
}

async fn load_role_credentials_at(base_url: &str) -> Result<RoleCredentials, ProbeError> {
    let session = MetadataSession::start(base_url).await?;
    let (status, body) = session
        .get(&format!(
            "/latest/meta-data/ram/security-credentials/{ECS_RAM_ROLE_NAME}"
        ))
        .await?;
    validate_credentials(status, &body, Utc::now())
}

// Only tests may inject a URL; the role path is always fixed by the owner.
#[cfg(test)]
pub(super) async fn probe_url(url: &str) -> Result<CredentialStatus, ProbeError> {
    let url = reqwest::Url::parse(url).map_err(|_| ProbeError::InvalidResponse)?;
    let base_url = url.origin().ascii_serialization();
    load_role_credentials_at(&base_url)
        .await
        .map(|credentials| credentials.status())
}

pub(super) async fn fetch_instance_id() -> Result<String, ProbeError> {
    MetadataSession::start(&metadata_endpoint())
        .await?
        .instance_id()
        .await
}

pub(super) async fn fetch_identity() -> Result<Option<(String, String)>, ProbeError> {
    let session = match MetadataSession::start(&metadata_endpoint()).await {
        Ok(session) => session,
        Err(ProbeError::Unreachable | ProbeError::Timeout) => return Ok(None),
        Err(error) => return Err(error),
    };
    // A valid token establishes ECS; later metadata failures must not select manual auth.
    let instance_id = session.instance_id().await?;
    let zone_id = session.text("/latest/meta-data/zone-id").await?;
    let region_id = super::region_id_from_zone_id(&zone_id).ok_or(ProbeError::InvalidResponse)?;
    Ok(Some((instance_id, region_id)))
}

/// A short-lived IMDSv2 session shares one deadline across token and GET requests.
/// The token is neither cached on disk nor passed outside the metadata owner.
struct MetadataSession {
    client: reqwest::Client,
    base_url: String,
    token: HeaderValue,
    deadline: Instant,
}

impl MetadataSession {
    async fn start(base_url: &str) -> Result<Self, ProbeError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(METADATA_CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ProbeError::Unreachable)?;
        let (status, body) = read_response(
            client
                .put(format!("{base_url}/latest/api/token"))
                .header(TOKEN_TTL_HEADER, TOKEN_TTL),
            deadline,
            MAX_TOKEN_BYTES,
        )
        .await?;
        // A missing token endpoint is not a missing role and never permits IMDSv1.
        check_status(status)?;
        if body.is_empty() || !body.iter().all(|byte| byte.is_ascii_graphic()) {
            return Err(ProbeError::InvalidResponse);
        }
        let mut token = HeaderValue::from_bytes(&body).map_err(|_| ProbeError::InvalidResponse)?;
        token.set_sensitive(true);
        Ok(Self {
            client,
            base_url: base_url.to_string(),
            token,
            deadline,
        })
    }

    async fn instance_id(&self) -> Result<String, ProbeError> {
        let id = self.text("/latest/meta-data/instance-id").await?;
        if id.starts_with("i-") && id.len() > 2 {
            Ok(id)
        } else {
            Err(ProbeError::InvalidResponse)
        }
    }

    async fn text(&self, path: &str) -> Result<String, ProbeError> {
        let (status, body) = self.get(path).await?;
        check_status(status)?;
        let text = std::str::from_utf8(&body)
            .map_err(|_| ProbeError::InvalidResponse)?
            .trim();
        // Identity fields become URL components, so reject delimiters and error pages.
        if text.is_empty()
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ProbeError::InvalidResponse);
        }
        Ok(text.to_string())
    }

    async fn get(&self, path: &str) -> Result<(u16, Vec<u8>), ProbeError> {
        read_response(
            self.client
                .get(format!("{}{path}", self.base_url))
                .header(TOKEN_HEADER, self.token.clone()),
            self.deadline,
            MAX_RESPONSE_BYTES,
        )
        .await
    }
}

async fn read_response(
    request: reqwest::RequestBuilder,
    deadline: Instant,
    limit: usize,
) -> Result<(u16, Vec<u8>), ProbeError> {
    timeout_at(deadline, async {
        let mut response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        // Never read or retain an untrusted error body (which can echo secrets).
        if !status.is_success() {
            return Ok((status.as_u16(), Vec::new()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(ProbeError::InvalidResponse);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > limit - body.len() {
                return Err(ProbeError::InvalidResponse);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status.as_u16(), body))
    })
    .await
    .unwrap_or(Err(ProbeError::Timeout))
}

fn check_status(status: u16) -> Result<(), ProbeError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(ProbeError::AccessDenied),
        _ => Err(ProbeError::Http),
    }
}

fn transport_error(error: reqwest::Error) -> ProbeError {
    if error.is_timeout() {
        ProbeError::Timeout
    } else if error.is_connect() {
        ProbeError::Unreachable
    } else {
        // A connected peer sending malformed HTTP is not evidence of non-ECS.
        ProbeError::InvalidResponse
    }
}
