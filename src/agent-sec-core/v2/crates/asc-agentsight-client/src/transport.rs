//! Minimal synchronous HTTP boundary used by the `AgentSight` Client.

use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use url::Url;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_BASE_URL_BYTES: usize = 2048;

/// HTTP methods required by the current `AgentSight` integration slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSightHttpMethod {
    /// Retrieve health state.
    Get,
    /// Submit a target Binding.
    Post,
    /// Remove a target Binding.
    Delete,
}

/// Sanitized request passed to an injectable `AgentSight` transport.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentSightHttpRequest {
    /// HTTP method.
    pub method: AgentSightHttpMethod,
    /// Absolute path relative to the configured `AgentSight` API root.
    pub path: String,
    /// JSON request bytes, present only for `POST`.
    pub body: Option<Vec<u8>>,
}

impl fmt::Debug for AgentSightHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentSightHttpRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field(
                "body_bytes",
                &self.body.as_ref().map_or(0, std::vec::Vec::len),
            )
            .finish()
    }
}

/// Raw bounded response returned by an injectable `AgentSight` transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSightHttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Bounded response body bytes.
    pub body: Vec<u8>,
}

/// Sanitized transport-layer failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentSightTransportError {
    /// The request shape violates the narrow transport contract.
    #[error("invalid AgentSight HTTP request")]
    InvalidRequest,
    /// The endpoint could not be reached or its response could not be read.
    #[error("AgentSight transport unavailable")]
    Unavailable,
    /// The target response exceeded the Client's memory bound.
    #[error("AgentSight response exceeds the configured limit")]
    ResponseTooLarge,
}

/// Injectable transport port for deterministic Client contract tests.
pub trait AgentSightTransport: Send + Sync {
    /// Sends one request without exposing credentials to the caller.
    ///
    /// # Errors
    /// Returns a sanitized local transport category.
    fn send(
        &self,
        request: &AgentSightHttpRequest,
    ) -> Result<AgentSightHttpResponse, AgentSightTransportError>;
}

/// Stable configuration failure that does not expose a token or file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentSightClientConfigError {
    /// The API root is malformed or uses HTTP outside literal loopback.
    #[error("invalid AgentSight base URL")]
    InvalidBaseUrl,
    /// The configured token file could not be read.
    #[error("AgentSight credential unavailable")]
    CredentialUnavailable,
    /// The token is empty, oversized, or contains unsafe characters.
    #[error("invalid AgentSight credential")]
    InvalidCredential,
}

/// Production synchronous HTTP transport backed by `ureq`.
pub struct UreqAgentSightTransport {
    agent: ureq::Agent,
    base_url: String,
    authorization: String,
}

impl fmt::Debug for UreqAgentSightTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UreqAgentSightTransport")
            .field("base_url", &self.base_url)
            .field("authorization", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl UreqAgentSightTransport {
    /// Creates a transport from an in-memory Bearer token.
    ///
    /// # Errors
    /// Returns a sanitized configuration category for an invalid URL or token.
    pub fn new(base_url: &str, bearer_token: &str) -> Result<Self, AgentSightClientConfigError> {
        let base_url = normalize_base_url(base_url)?;
        let bearer_token = normalize_token(bearer_token)?;
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout(DEFAULT_REQUEST_TIMEOUT)
                .redirects(0)
                .build(),
            base_url,
            authorization: format!("Bearer {bearer_token}"),
        })
    }

    /// Creates a transport by reading the configured Bearer-token file.
    ///
    /// A single trailing line ending is accepted because the standard token
    /// file is commonly produced by a line-oriented command.
    ///
    /// # Errors
    /// Returns a sanitized configuration category; the path and file content
    /// are never included in the error.
    pub fn from_token_file(
        base_url: &str,
        token_file: impl AsRef<Path>,
    ) -> Result<Self, AgentSightClientConfigError> {
        let token =
            fs::read(token_file).map_err(|_| AgentSightClientConfigError::CredentialUnavailable)?;
        if token.len() > MAX_TOKEN_BYTES {
            return Err(AgentSightClientConfigError::InvalidCredential);
        }
        let token = std::str::from_utf8(&token)
            .map_err(|_| AgentSightClientConfigError::InvalidCredential)?;
        Self::new(base_url, token)
    }
}

impl AgentSightTransport for UreqAgentSightTransport {
    fn send(
        &self,
        request: &AgentSightHttpRequest,
    ) -> Result<AgentSightHttpResponse, AgentSightTransportError> {
        validate_request(request)?;
        let url = format!("{}{}", self.base_url, request.path);
        let builder = match request.method {
            AgentSightHttpMethod::Get => self.agent.get(&url),
            AgentSightHttpMethod::Post => self.agent.post(&url),
            AgentSightHttpMethod::Delete => self.agent.delete(&url),
        }
        .set("Authorization", &self.authorization)
        .set("Accept", "application/json");

        let result = match request.method {
            AgentSightHttpMethod::Post => builder
                .set("Content-Type", "application/json")
                .send_bytes(request.body.as_deref().unwrap_or_default()),
            AgentSightHttpMethod::Get | AgentSightHttpMethod::Delete => builder.call(),
        };
        match result {
            Ok(response) | Err(ureq::Error::Status(_, response)) => read_response(response),
            Err(ureq::Error::Transport(_)) => Err(AgentSightTransportError::Unavailable),
        }
    }
}

fn normalize_base_url(base_url: &str) -> Result<String, AgentSightClientConfigError> {
    if base_url.is_empty()
        || base_url.len() > MAX_BASE_URL_BYTES
        || base_url
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || base_url.contains(['?', '#'])
    {
        return Err(AgentSightClientConfigError::InvalidBaseUrl);
    }
    let authority_and_path = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .ok_or(AgentSightClientConfigError::InvalidBaseUrl)?;
    let authority = authority_and_path
        .split('/')
        .next()
        .ok_or(AgentSightClientConfigError::InvalidBaseUrl)?;
    if authority.is_empty() || authority.contains('@') || authority_and_path.starts_with('/') {
        return Err(AgentSightClientConfigError::InvalidBaseUrl);
    }
    let parsed = Url::parse(base_url).map_err(|_| AgentSightClientConfigError::InvalidBaseUrl)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port() == Some(0)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(AgentSightClientConfigError::InvalidBaseUrl);
    }
    let normalized = parsed.as_str().trim_end_matches('/');
    if parsed.scheme() == "http"
        && !matches!(parsed.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        && !matches!(parsed.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
    {
        return Err(AgentSightClientConfigError::InvalidBaseUrl);
    }
    if normalized.len() > MAX_BASE_URL_BYTES {
        return Err(AgentSightClientConfigError::InvalidBaseUrl);
    }
    Ok(normalized.to_owned())
}

fn normalize_token(token: &str) -> Result<&str, AgentSightClientConfigError> {
    let token = token.trim_end_matches(['\r', '\n']);
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(AgentSightClientConfigError::InvalidCredential);
    }
    Ok(token)
}

fn validate_request(request: &AgentSightHttpRequest) -> Result<(), AgentSightTransportError> {
    if !request.path.starts_with('/')
        || request.path.contains(['?', '#'])
        || request
            .path
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(AgentSightTransportError::InvalidRequest);
    }
    match (request.method, request.body.is_some()) {
        (AgentSightHttpMethod::Post, true)
        | (AgentSightHttpMethod::Get | AgentSightHttpMethod::Delete, false) => Ok(()),
        _ => Err(AgentSightTransportError::InvalidRequest),
    }
}

fn read_response(
    response: ureq::Response,
) -> Result<AgentSightHttpResponse, AgentSightTransportError> {
    let status = response.status();
    let mut body = Vec::new();
    response
        .into_reader()
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| AgentSightTransportError::Unavailable)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(AgentSightTransportError::ResponseTooLarge);
    }
    Ok(AgentSightHttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_credentials_require_tls_outside_literal_loopback() {
        for endpoint in [
            "http://127.0.0.1:7396/api",
            "http://127.23.4.5/api",
            "http://[::1]/api",
            "https://example.com/api",
            "https://192.0.2.1/api",
        ] {
            assert!(
                UreqAgentSightTransport::new(endpoint, "token").is_ok(),
                "{endpoint}"
            );
        }
        for endpoint in [
            "http://192.0.2.1/api",
            "http://10.0.0.1/api",
            "http://example.com/api",
            "http://localhost/api",
            "http://localhost./api",
            "http://127.0.0.1.example.com/api",
            "http://[2001:db8::1]/api",
            "http://0.0.0.0/api",
        ] {
            assert_eq!(
                UreqAgentSightTransport::new(endpoint, "token").unwrap_err(),
                AgentSightClientConfigError::InvalidBaseUrl,
                "{endpoint}"
            );
        }
    }

    #[test]
    fn debug_output_redacts_credentials_and_request_bodies() {
        let transport =
            UreqAgentSightTransport::new("http://127.0.0.1:7396/api/", "top-secret-token\n")
                .unwrap();
        let transport_debug = format!("{transport:?}");
        assert!(transport_debug.contains("<redacted>"));
        assert!(!transport_debug.contains("top-secret-token"));

        let request = AgentSightHttpRequest {
            method: AgentSightHttpMethod::Post,
            path: "/enforcement/bindings".to_owned(),
            body: Some(b"sensitive policy body".to_vec()),
        };
        let request_debug = format!("{request:?}");
        assert!(request_debug.contains("body_bytes"));
        assert!(!request_debug.contains("sensitive policy body"));
    }

    #[test]
    fn configuration_rejects_unsafe_urls_and_tokens() {
        assert_eq!(
            UreqAgentSightTransport::new("file:///tmp/socket", "token").unwrap_err(),
            AgentSightClientConfigError::InvalidBaseUrl
        );
        assert_eq!(
            UreqAgentSightTransport::new("http://localhost/api?token=x", "token").unwrap_err(),
            AgentSightClientConfigError::InvalidBaseUrl
        );
        assert_eq!(
            UreqAgentSightTransport::new("http://user:secret@localhost/api", "token").unwrap_err(),
            AgentSightClientConfigError::InvalidBaseUrl
        );
        assert_eq!(
            UreqAgentSightTransport::new("http://127.0.0.1/api", "bad token").unwrap_err(),
            AgentSightClientConfigError::InvalidCredential
        );
    }

    #[test]
    fn configuration_rejects_malformed_authorities_and_ports() {
        for base_url in [
            "http://[::1",
            "http://localhost:99999",
            "http://:80",
            "http://localhost:0",
        ] {
            assert_eq!(
                UreqAgentSightTransport::new(base_url, "token").unwrap_err(),
                AgentSightClientConfigError::InvalidBaseUrl,
                "base URL should be rejected: {base_url}"
            );
        }

        let valid_ipv6 = UreqAgentSightTransport::new("http://[::1]:7396/api/", "token").unwrap();
        assert_eq!(valid_ipv6.base_url, "http://[::1]:7396/api");
    }
}
