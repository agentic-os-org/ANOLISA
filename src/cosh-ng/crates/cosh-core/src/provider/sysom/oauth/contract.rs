//! Fixed SysOM Cosh OAuth contract: envelopes, grants, PKCE and URL checks.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::Instant;

use super::super::SysomCredentials;

/// Registered loopback callback port.
pub const CALLBACK_PORT: u16 = 18763;
/// Only callback path accepted on the loopback listener.
pub const CALLBACK_PATH: &str = "/callback";
/// Redirect URI registered with the Native application.
pub const REDIRECT_URI: &str = "http://127.0.0.1:18763/callback";
/// Official Alibaba Cloud authorization endpoint host.
pub const AUTHORIZATION_HOST: &str = "signin.aliyun.com";
/// Official authorization endpoint path.
pub const AUTHORIZATION_PATH: &str = "/oauth2/v1/auth";
/// Fixed token revocation endpoint.
pub const REVOKE_URL: &str = "https://oauth.aliyun.com/v1/revoke";
/// Identity challenge Action path.
pub const CHALLENGE_PATH: &str = "/api/v1/identity/cosh-oauth/challenge";
/// Identity credential exchange Action path.
pub const CREDENTIALS_PATH: &str = "/api/v1/identity/cosh-oauth/credentials";
/// Gateway Action name for the challenge path (anonymous ACS3 header set).
pub const CHALLENGE_ACTION: &str = "CreateCoshOAuthChallenge";
/// Gateway Action name for the credential exchange path.
pub const CREDENTIALS_ACTION: &str = "ExchangeCoshModelCredentials";
/// Request body cap shared with the Identity service.
pub const MAX_REQUEST_BODY: usize = 8 * 1024;
/// Response body cap for envelope parsing.
pub const MAX_JSON: usize = 64 * 1024;
/// Upper bound on accepted STS lifetime (900s plus clock skew).
pub const STS_MAX_TTL_SECS: i64 = 930;
/// Refresh this far before expiry, on demand only.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// Pre-release SysOM OpenAPI host, prototype-verified; pre profile only.
pub const PRE_HOST: &str = "sysom-pre.cn-hangzhou.aliyuncs.com";
/// Native application registered for pre (Identity config.yml @ ab6c02656).
pub const PRE_CLIENT_ID: &str = "4037115895158241462";

/// Release-environment SysOM host this build is bound to.
pub fn bound_host() -> &'static str {
    #[cfg(feature = "oauth-pre")]
    {
        PRE_HOST
    }
    #[cfg(not(feature = "oauth-pre"))]
    {
        super::super::endpoint::PUBLIC_HOST
    }
}

/// Application id this build must see in the authorization URL, once frozen.
pub fn expected_client_id() -> Option<&'static str> {
    #[cfg(feature = "oauth-pre")]
    {
        Some(PRE_CLIENT_ID)
    }
    #[cfg(not(feature = "oauth-pre"))]
    {
        None
    }
}

/// OS keyring namespace isolated per release environment.
pub fn keyring_service() -> &'static str {
    #[cfg(feature = "oauth-pre")]
    {
        "cosh-ng.oauth.pre"
    }
    #[cfg(not(feature = "oauth-pre"))]
    {
        "cosh-ng.oauth"
    }
}

/// Local login-state file isolated per release environment.
pub fn state_file_name() -> &'static str {
    #[cfg(feature = "oauth-pre")]
    {
        "oauth-state.pre.json"
    }
    #[cfg(not(feature = "oauth-pre"))]
    {
        "oauth-state.json"
    }
}
/// Identity caps one exchange at 15s and the candidate gateway at 20s; the
/// login task deadline leaves network margin instead of reusing Registry timeouts.
pub const EXCHANGE_DEADLINE: Duration = Duration::from_secs(25);

const B64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Non-sensitive OAuth failure categories; server codes keep the Identity
/// family, client-side categories use `Cosh.OAuth.*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// User or parent task cancelled the flow.
    Cancelled,
    /// No usable login for the selected source.
    NotLoggedIn,
    /// Login generation changed mid-flight; the result was discarded.
    GenerationChanged,
    /// Cached or returned lease is past its safe send window.
    LeaseExpired,
    /// Envelope or payload failed strict validation.
    InvalidResponse,
    /// Authorization URL failed official-endpoint or binding checks.
    InvalidAuthorizationUrl,
    /// Request parameters rejected by Identity.
    InvalidParameter,
    /// Definitive proof/identity/authorization rejection.
    PermissionDenied,
    /// Account lacks the issuable service-linked role.
    SysomNotActivated,
    /// Challenge invalid, expired or already consumed.
    ChallengeNotRedeemable,
    /// Throttled by the gateway or upstream (HTTP 429).
    Throttled,
    /// Configuration or dependency not ready; session preserved.
    ProviderNotReady,
    /// STS temporarily unavailable; session preserved.
    StsUnavailable,
    /// STS returned an incomplete or invalid response.
    StsInvalidResponse,
    /// Identity internal error.
    InternalError,
    /// Unknown server code or unclassifiable failure; session preserved.
    UnknownServer,
    /// Transport failure.
    Unavailable,
    /// Client deadline elapsed.
    Timeout,
    /// Endpoint outside the release-environment whitelist.
    UntrustedEndpoint,
    /// OS secret store unavailable; existing configuration preserved.
    SecretStoreUnavailable,
    /// Local login-state file unreadable or unwritable.
    StateUnavailable,
    /// Callback port already occupied.
    PortInUse,
    /// No state-matched callback before the login deadline.
    CallbackTimeout,
    /// Callback matched state but carried no usable result.
    CallbackInvalid,
    /// System browser handoff failed.
    BrowserUnavailable,
    /// User or administrator denied the authorization.
    AuthorizationDenied,
}

impl OAuthError {
    /// Stable non-sensitive code for logs and registry responses.
    pub fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "Cosh.OAuth.Cancelled",
            Self::NotLoggedIn => "Cosh.OAuth.NotLoggedIn",
            Self::GenerationChanged => "Cosh.OAuth.GenerationChanged",
            Self::LeaseExpired => "Cosh.OAuth.LeaseExpired",
            Self::InvalidResponse => "Sysom.Identity.InvalidResponse",
            Self::InvalidAuthorizationUrl => "Cosh.OAuth.InvalidAuthorizationUrl",
            Self::InvalidParameter => "Sysom.Identity.InvalidParameter",
            Self::PermissionDenied => "Sysom.Identity.PermissionDenied",
            Self::SysomNotActivated => "Sysom.Identity.SysomNotActivated",
            Self::ChallengeNotRedeemable => "Sysom.Identity.ChallengeNotRedeemable",
            Self::Throttled => "Sysom.Identity.Throttled",
            Self::ProviderNotReady => "Sysom.Identity.ProviderNotReady",
            Self::StsUnavailable => "Sysom.Identity.StsUnavailable",
            Self::StsInvalidResponse => "Sysom.Identity.StsInvalidResponse",
            Self::InternalError => "Sysom.Identity.InternalError",
            Self::UnknownServer => "Sysom.Identity.UnknownServer",
            Self::Unavailable => "Cosh.OAuth.Unavailable",
            Self::Timeout => "Cosh.OAuth.Timeout",
            Self::UntrustedEndpoint => "Cosh.OAuth.UntrustedEndpoint",
            Self::SecretStoreUnavailable => "Cosh.OAuth.SecretStoreUnavailable",
            Self::StateUnavailable => "Cosh.OAuth.StateUnavailable",
            Self::PortInUse => "Cosh.OAuth.PortInUse",
            Self::CallbackTimeout => "Cosh.OAuth.CallbackTimeout",
            Self::CallbackInvalid => "Cosh.OAuth.CallbackInvalid",
            Self::BrowserUnavailable => "Cosh.OAuth.BrowserUnavailable",
            Self::AuthorizationDenied => "Cosh.OAuth.AuthorizationDenied",
        }
    }

    /// Whether bounded backoff may keep the local login and retry later.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Throttled
                | Self::ProviderNotReady
                | Self::StsUnavailable
                | Self::StsInvalidResponse
                | Self::InternalError
                | Self::UnknownServer
                | Self::Unavailable
                | Self::Timeout
        )
    }
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for OAuthError {}

/// Issued STS plus the refresh token returned on first exchange only.
pub struct OAuthCredentials {
    /// Short-lived model credentials.
    pub credentials: SysomCredentials,
    /// Monotonic expiry derived from `expiration - serverTime`.
    pub expires_at: Instant,
    /// Present on first exchange; refresh responses must not rotate it.
    pub refresh_token: Option<String>,
}

impl std::fmt::Debug for OAuthCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OAuthCredentials([REDACTED])")
    }
}

/// PKCE S256 pair for one login attempt.
pub struct Pkce {
    /// Secret verifier kept in process memory only.
    pub verifier: String,
    /// S256 challenge sent to Identity and embedded in the browser URL.
    pub challenge: String,
}

impl Pkce {
    /// Generates a fresh random verifier and its S256 challenge.
    pub fn generate() -> Self {
        let verifier = b64url_encode(&random_32());
        let challenge = b64url_encode(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// Random URL-safe state binding one browser callback to one attempt.
pub fn random_state() -> String {
    b64url_encode(&random_32())
}

fn random_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

pub(super) fn b64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(B64URL[(triple >> 18) as usize & 63] as char);
        out.push(B64URL[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[triple as usize & 63] as char);
        }
    }
    out
}

fn is_url_token(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn map_code(code: &str) -> OAuthError {
    match code {
        "Sysom.Identity.InvalidParameter" => OAuthError::InvalidParameter,
        "Sysom.Identity.PermissionDenied" => OAuthError::PermissionDenied,
        "Sysom.Identity.SysomNotActivated" => OAuthError::SysomNotActivated,
        "Sysom.Identity.ChallengeNotRedeemable" => OAuthError::ChallengeNotRedeemable,
        "Sysom.Identity.Throttled" => OAuthError::Throttled,
        "Sysom.Identity.ProviderNotReady" => OAuthError::ProviderNotReady,
        "Sysom.Identity.StsUnavailable" => OAuthError::StsUnavailable,
        "Sysom.Identity.StsInvalidResponse" => OAuthError::StsInvalidResponse,
        "Sysom.Identity.InternalError" => OAuthError::InternalError,
        _ => OAuthError::UnknownServer,
    }
}

/// Accept only `code == "Success"` envelopes with complete data; classify
/// everything else without ever echoing upstream messages.
pub(super) fn envelope_or_status(status: u16, body: &[u8]) -> Result<Value, OAuthError> {
    if body.len() <= MAX_JSON {
        if let Ok(value) = serde_json::from_slice::<Value>(body) {
            if let Some(code) = value
                .get("code")
                .or_else(|| value.get("Code"))
                .and_then(Value::as_str)
            {
                if code != "Success" {
                    return Err(map_code(code));
                }
                if status == 200
                    && value.get("success").and_then(Value::as_bool) == Some(true)
                    && value.get("data").is_some_and(Value::is_object)
                {
                    return Ok(value["data"].clone());
                }
                return Err(OAuthError::InvalidResponse);
            }
        }
    }
    if status == 429 {
        Err(OAuthError::Throttled)
    } else if status == 200 {
        Err(OAuthError::InvalidResponse)
    } else {
        Err(OAuthError::UnknownServer)
    }
}

fn field<'a>(data: &'a Value, name: &str) -> Result<&'a str, OAuthError> {
    data.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(OAuthError::InvalidResponse)
}

fn lifetime(data: &Value) -> Result<Duration, OAuthError> {
    let server = chrono::DateTime::parse_from_rfc3339(field(data, "serverTime")?)
        .map_err(|_| OAuthError::InvalidResponse)?;
    let expiry = chrono::DateTime::parse_from_rfc3339(field(data, "expiration")?)
        .map_err(|_| OAuthError::InvalidResponse)?;
    let ttl = expiry.signed_duration_since(server);
    if ttl <= chrono::Duration::zero() || ttl > chrono::Duration::seconds(STS_MAX_TTL_SECS) {
        return Err(OAuthError::InvalidResponse);
    }
    ttl.to_std().map_err(|_| OAuthError::InvalidResponse)
}

pub(super) fn credentials_data(
    body: &[u8],
    started: Instant,
) -> Result<OAuthCredentials, OAuthError> {
    let data = envelope_or_status(200, body)?;
    let expires_at = started + lifetime(&data)?;
    if expires_at <= Instant::now() {
        return Err(OAuthError::InvalidResponse);
    }
    let refresh_token = data
        .get("refreshToken")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(OAuthCredentials {
        credentials: SysomCredentials {
            access_key_id: field(&data, "accessKeyId")?.into(),
            access_key_secret: field(&data, "accessKeySecret")?.into(),
            security_token: Some(field(&data, "securityToken")?.into()),
        },
        expires_at,
        refresh_token,
    })
}

pub(super) fn challenge_data(body: &[u8]) -> Result<(String, String), OAuthError> {
    let data = envelope_or_status(200, body)?;
    let nonce = field(&data, "nonce")?;
    if !is_url_token(nonce, 43) {
        return Err(OAuthError::InvalidResponse);
    }
    let url = field(&data, "authorizationUrl")?;
    if url.len() > 2048 {
        return Err(OAuthError::InvalidResponse);
    }
    Ok((nonce.to_owned(), url.to_owned()))
}

/// Verify the browser URL against the fixed official endpoint and the exact
/// parameters this login attempt generated. Returns the application client id.
/// Verifies the official endpoint and every attempt binding; returns the
/// application client id carried by the URL.
pub fn validate_authorization_url(
    url: &str,
    state: &str,
    challenge: &str,
    nonce: &str,
) -> Result<String, OAuthError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| OAuthError::InvalidAuthorizationUrl)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(AUTHORIZATION_HOST)
        || parsed.path() != AUTHORIZATION_PATH
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(OAuthError::InvalidAuthorizationUrl);
    }
    let mut params: Vec<(String, String)> = Vec::new();
    for (key, value) in parsed.query_pairs() {
        if params.iter().any(|(existing, _)| existing == key.as_ref()) {
            return Err(OAuthError::InvalidAuthorizationUrl);
        }
        params.push((key.into_owned(), value.into_owned()));
    }
    let get = |name: &str| -> Result<&str, OAuthError> {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or(OAuthError::InvalidAuthorizationUrl)
    };
    if get("response_type")? != "code"
        || get("redirect_uri")? != REDIRECT_URI
        || get("state")? != state
        || get("code_challenge")? != challenge
        || get("code_challenge_method")? != "S256"
        || get("nonce")? != nonce
    {
        return Err(OAuthError::InvalidAuthorizationUrl);
    }
    let client_id = get("client_id")?;
    if client_id.len() > 128 {
        return Err(OAuthError::InvalidAuthorizationUrl);
    }
    let scope = get("scope")?;
    let mut scopes: Vec<&str> = scope.split_whitespace().collect();
    scopes.sort_unstable();
    scopes.dedup();
    for required in ["aliuid", "openid", "profile"] {
        if !scopes.contains(&required) {
            return Err(OAuthError::InvalidAuthorizationUrl);
        }
    }
    Ok(client_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials_body(extra: Value) -> Vec<u8> {
        let mut data = serde_json::json!({
            "accessKeyId": "STS.test",
            "accessKeySecret": "secret-canary",
            "securityToken": "token-canary",
            "serverTime": "2026-01-01T00:00:00Z",
            "expiration": "2026-01-01T00:15:00Z",
            "refreshToken": "refresh-canary",
        });
        if let (Some(base), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        serde_json::json!({"code":"Success","success":true,"data":data})
            .to_string()
            .into_bytes()
    }

    fn authorization_url(overrides: &[(&str, &str)]) -> String {
        let mut params: Vec<(String, String)> = vec![
            ("response_type".into(), "code".into()),
            ("client_id".into(), "4038181954510891234".into()),
            ("redirect_uri".into(), REDIRECT_URI.into()),
            ("state".into(), "state-value".into()),
            ("code_challenge".into(), "challenge-value".into()),
            ("code_challenge_method".into(), "S256".into()),
            ("scope".into(), "openid aliuid profile".into()),
            ("nonce".into(), "nonce-value".into()),
        ];
        for (key, value) in overrides {
            if let Some(entry) = params.iter_mut().find(|(existing, _)| existing == key) {
                entry.1 = value.to_string();
            } else {
                params.push((key.to_string(), value.to_string()));
            }
        }
        let query = params
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        format!("https://{AUTHORIZATION_HOST}{AUTHORIZATION_PATH}?{query}")
    }

    #[test]
    fn pkce_matches_rfc7636_vector_and_is_random() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            b64url_encode(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let first = Pkce::generate();
        let second = Pkce::generate();
        assert!(is_url_token(&first.verifier, 43));
        assert!(is_url_token(&first.challenge, 43));
        assert_eq!(
            first.challenge,
            b64url_encode(&Sha256::digest(first.verifier.as_bytes()))
        );
        assert_ne!(first.verifier, second.verifier);
        assert!(is_url_token(&random_state(), 43));
    }

    #[test]
    fn envelope_requires_success_flag_and_object_data() {
        for body in [
            r#"{"code":"Success","data":{}}"#,
            r#"{"code":"Success","success":false,"data":{}}"#,
            r#"{"code":"Success","success":true,"data":[]}"#,
            r#"{"code":"Success","success":true}"#,
            r#"{"success":true,"data":{}}"#,
            "not json",
        ] {
            assert!(
                matches!(
                    envelope_or_status(200, body.as_bytes()),
                    Err(OAuthError::InvalidResponse)
                ),
                "{body}"
            );
        }
        let ok = envelope_or_status(
            200,
            br#"{"code":"Success","success":true,"data":{"nonce":"n"}}"#,
        )
        .unwrap();
        assert_eq!(ok["nonce"], "n");
    }

    #[test]
    fn envelope_maps_the_frozen_error_family() {
        for (code, expected) in [
            (
                "Sysom.Identity.InvalidParameter",
                OAuthError::InvalidParameter,
            ),
            (
                "Sysom.Identity.PermissionDenied",
                OAuthError::PermissionDenied,
            ),
            (
                "Sysom.Identity.SysomNotActivated",
                OAuthError::SysomNotActivated,
            ),
            (
                "Sysom.Identity.ChallengeNotRedeemable",
                OAuthError::ChallengeNotRedeemable,
            ),
            ("Sysom.Identity.Throttled", OAuthError::Throttled),
            (
                "Sysom.Identity.ProviderNotReady",
                OAuthError::ProviderNotReady,
            ),
            ("Sysom.Identity.StsUnavailable", OAuthError::StsUnavailable),
            (
                "Sysom.Identity.StsInvalidResponse",
                OAuthError::StsInvalidResponse,
            ),
            ("Sysom.Identity.InternalError", OAuthError::InternalError),
        ] {
            let body = serde_json::json!({"code":code,"success":false,"message":"secret-canary"})
                .to_string();
            assert_eq!(envelope_or_status(200, body.as_bytes()), Err(expected));
        }
    }

    #[test]
    fn unknown_or_unparseable_responses_preserve_the_session() {
        let unknown =
            serde_json::json!({"code":"Sysom.Identity.BrandNew","success":false}).to_string();
        assert_eq!(
            envelope_or_status(200, unknown.as_bytes()),
            Err(OAuthError::UnknownServer)
        );
        assert_eq!(
            envelope_or_status(403, b"gateway denied"),
            Err(OAuthError::UnknownServer)
        );
        assert_eq!(envelope_or_status(500, b""), Err(OAuthError::UnknownServer));
        assert_eq!(
            envelope_or_status(429, b"slow down"),
            Err(OAuthError::Throttled)
        );
        assert!(OAuthError::UnknownServer.is_transient());
        assert!(OAuthError::Throttled.is_transient());
        assert!(!OAuthError::PermissionDenied.is_transient());
        assert!(!OAuthError::SysomNotActivated.is_transient());
        assert!(!OAuthError::InvalidParameter.is_transient());
    }

    #[test]
    fn oversized_response_is_rejected() {
        let body = vec![b'x'; MAX_JSON + 1];
        assert_eq!(
            envelope_or_status(200, &body),
            Err(OAuthError::InvalidResponse)
        );
    }

    #[test]
    fn credentials_use_server_relative_lifetime_and_capture_refresh() {
        let started = Instant::now();
        let issued = credentials_data(&credentials_body(Value::Null), started).unwrap();
        let ttl = issued.expires_at.saturating_duration_since(started);
        assert_eq!(ttl, Duration::from_secs(900));
        assert_eq!(issued.refresh_token.as_deref(), Some("refresh-canary"));
        assert_eq!(issued.credentials.access_key_id, "STS.test");
        assert_eq!(
            issued.credentials.security_token.as_deref(),
            Some("token-canary")
        );
        let without = credentials_body(serde_json::json!({"refreshToken": null}));
        assert!(credentials_data(&without, started)
            .unwrap()
            .refresh_token
            .is_none());
    }

    #[test]
    fn credentials_require_every_sts_field() {
        for missing in [
            "accessKeyId",
            "accessKeySecret",
            "securityToken",
            "serverTime",
            "expiration",
        ] {
            let mut body: Value = serde_json::from_slice(&credentials_body(Value::Null)).unwrap();
            body["data"].as_object_mut().unwrap().remove(missing);
            assert!(
                matches!(
                    credentials_data(body.to_string().as_bytes(), Instant::now()),
                    Err(OAuthError::InvalidResponse)
                ),
                "{missing}"
            );
        }
    }

    #[test]
    fn credentials_reject_invalid_lifetimes() {
        for (server, expiration) in [
            ("2026-01-01T00:15:00Z", "2026-01-01T00:00:00Z"),
            ("2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
            ("2026-01-01T00:00:00Z", "2026-01-01T00:16:00Z"),
            ("not-a-time", "2026-01-01T00:15:00Z"),
        ] {
            let body = credentials_body(serde_json::json!({
                "serverTime": server, "expiration": expiration,
            }));
            assert!(
                matches!(
                    credentials_data(&body, Instant::now()),
                    Err(OAuthError::InvalidResponse)
                ),
                "{server} {expiration}"
            );
        }
    }

    #[test]
    fn challenge_requires_url_safe_nonce_and_bounded_url() {
        let body = serde_json::json!({"code":"Success","success":true,"data":{
            "nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "authorizationUrl":"https://signin.aliyun.com/oauth2/v1/auth?x=1"}})
        .to_string();
        let (nonce, url) = challenge_data(body.as_bytes()).unwrap();
        assert_eq!(nonce.len(), 43);
        assert!(url.starts_with("https://signin.aliyun.com/"));
        for nonce in ["short", &"a".repeat(44), &format!("{}!", "a".repeat(42))] {
            let body = serde_json::json!({"code":"Success","success":true,"data":{
                "nonce":nonce,"authorizationUrl":"https://signin.aliyun.com/oauth2/v1/auth"}})
            .to_string();
            assert!(matches!(
                challenge_data(body.as_bytes()),
                Err(OAuthError::InvalidResponse)
            ));
        }
    }

    #[test]
    fn authorization_url_validates_every_binding() {
        let url = authorization_url(&[]);
        assert_eq!(
            validate_authorization_url(&url, "state-value", "challenge-value", "nonce-value")
                .unwrap(),
            "4038181954510891234"
        );
        for (key, value) in [
            ("response_type", "token"),
            ("redirect_uri", "http://127.0.0.1:18764/callback"),
            ("redirect_uri", "http://evil.example/callback"),
            ("state", "other-state"),
            ("code_challenge", "other-challenge"),
            ("code_challenge_method", "plain"),
            ("nonce", "other-nonce"),
            ("scope", "openid aliuid"),
            ("scope", "openid profile"),
            ("scope", "aliuid profile"),
            ("client_id", ""),
        ] {
            let url = authorization_url(&[(key, value)]);
            assert!(
                matches!(
                    validate_authorization_url(
                        &url,
                        "state-value",
                        "challenge-value",
                        "nonce-value"
                    ),
                    Err(OAuthError::InvalidAuthorizationUrl)
                ),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn authorization_url_rejects_unofficial_origins_and_duplicates() {
        for url in [
            "http://signin.aliyun.com/oauth2/v1/auth?x=1".to_string(),
            "https://evil.example/oauth2/v1/auth?x=1".to_string(),
            "https://signin.aliyun.com/oauth2/v2/auth?x=1".to_string(),
            "https://signin.aliyun.com/oauth2/v1/auth#frag".to_string(),
            "https://user@signin.aliyun.com/oauth2/v1/auth".to_string(),
            format!("{}&state=duplicate", authorization_url(&[])),
            "not a url".to_string(),
        ] {
            assert!(
                matches!(
                    validate_authorization_url(
                        &url,
                        "state-value",
                        "challenge-value",
                        "nonce-value"
                    ),
                    Err(OAuthError::InvalidAuthorizationUrl)
                ),
                "{url}"
            );
        }
    }
}
