//! Dashboard authentication middleware and token management.
//!
//! Provides token-based API key authentication for the AgentSight dashboard.
//! On first startup a random 32-byte token is generated and persisted to a
//! local file so it survives restarts.  Requests are authenticated via:
//!
//! 1. `Authorization: Bearer <token>` header
//! 2. `?token=<token>` query parameter
//! 3. `agentsight_session` cookie (set after a successful login)

use std::future::{Future, Ready, ready};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use actix_web::body::EitherBody;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::{Error, HttpResponse};
use sha2::{Digest, Sha256};

use crate::config::ServerAuthConfig;

// ─── Cookie signing ─────────────────────────────────────────────────────────

/// Sign a session cookie value using a SHA-256 keyed hash.
///
/// Format: `<expires_secs>.<signature>` where
/// `signature = SHA256(token || "." || expires_secs || "." || token)`.
///
/// Note: this is a simple keyed-hash sandwich construction, not RFC 2104
/// HMAC.  It is sufficient for short-lived session cookies in a local
/// deployment context.
fn sign_cookie(token: &str, expires_secs: u64) -> String {
    let payload = format!("{token}.{expires_secs}.{token}");
    let sig = hex::encode(Sha256::digest(payload.as_bytes()));
    format!("{expires_secs}.{sig}")
}

/// Verify a signed cookie value.  Returns `true` when the signature matches
/// **and** the expiry timestamp has not passed.
fn verify_cookie(token: &str, cookie_value: &str) -> bool {
    let Some((expires_str, _sig)) = cookie_value.split_once('.') else {
        return false;
    };
    let Ok(expires_secs) = expires_str.parse::<u64>() else {
        return false;
    };

    // Check expiry (Unix seconds since epoch)
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now_secs > expires_secs {
        return false;
    }

    let expected = sign_cookie(token, expires_secs);
    // Constant-time comparison to avoid timing attacks
    constant_time_eq(cookie_value.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ─── Token generation ───────────────────────────────────────────────────────

/// Generate a random 32-byte token (64 hex characters).
///
/// Mixes multiple entropy sources (system time, thread ID, PID, stack
/// address) via `DefaultHasher`, then XORs with `/dev/urandom` when
/// available.  On Linux the `/dev/urandom` path provides the real
/// cryptographic entropy; the hash-based fallback is a safety net for
/// environments where `/dev/urandom` is unavailable.
fn generate_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Use multiple rounds of system entropy
    let mut token_bytes = [0u8; 32];
    for chunk in token_bytes.chunks_mut(8) {
        let mut h = DefaultHasher::new();
        // Time-based entropy
        std::time::SystemTime::now().hash(&mut h);
        // Thread ID entropy
        std::thread::current().id().hash(&mut h);
        // Process ID
        std::process::id().hash(&mut h);
        // Stack address entropy
        let stack_var = 0u64;
        (&stack_var as *const u64 as usize).hash(&mut h);
        let hash_val = h.finish();
        let len = chunk.len().min(8);
        chunk[..len].copy_from_slice(&hash_val.to_le_bytes()[..len]);
        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_nanos(1));
    }
    // Also mix in /dev/urandom if available (read only 32 bytes — it's an infinite stream)
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut random_bytes = [0u8; 32];
        if file.read_exact(&mut random_bytes).is_ok() {
            for (i, byte) in random_bytes.iter().enumerate() {
                token_bytes[i] ^= byte;
            }
        }
    }
    hex::encode(token_bytes)
}

// ─── Token file I/O ─────────────────────────────────────────────────────────

/// Default token file name (stored alongside the SQLite database).
const TOKEN_FILE_NAME: &str = ".dashboard_token";

/// Read the token from a file, or generate and persist a new one.
fn read_or_create_token(token_file: &Path) -> String {
    // Try reading existing token
    if let Ok(content) = std::fs::read_to_string(token_file) {
        let trimmed = content.trim();
        if !trimmed.is_empty() && trimmed.len() >= 32 {
            return trimmed.to_string();
        }
    }

    // Generate new token
    let token = generate_token();

    // Persist to file
    if let Some(parent) = token_file.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("Failed to create token directory {parent:?}: {e}");
        }
    }

    // Write token with 0600 permissions atomically (avoid TOCTOU race)
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(token_file)
        {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(token.as_bytes()) {
                    log::warn!("Failed to write dashboard token to {token_file:?}: {e}");
                } else {
                    log::info!("Dashboard auth token generated and saved to {token_file:?}");
                }
            }
            Err(e) => {
                log::warn!("Failed to create dashboard token file {token_file:?}: {e}");
            }
        }
    }

    #[cfg(not(unix))]
    {
        match std::fs::write(token_file, &token) {
            Ok(()) => {
                log::info!("Dashboard auth token generated and saved to {token_file:?}");
            }
            Err(e) => {
                log::warn!("Failed to persist dashboard token to {token_file:?}: {e}");
            }
        }
    }

    token
}

// ─── DashboardAuth ──────────────────────────────────────────────────────────

/// Holds the authentication state for the dashboard server.
#[derive(Clone, Debug)]
pub struct DashboardAuth {
    /// Whether authentication is enabled.
    pub enabled: bool,
    /// The secret token used for authentication.
    token: Option<String>,
    /// Path to the token file (for CLI queries).
    token_file: PathBuf,
}

impl DashboardAuth {
    /// Initialize authentication from configuration.
    ///
    /// The token is always read from the default token file
    /// (`<storage_base>/.dashboard_token`), or auto-generated on first run.
    pub fn init(config: &ServerAuthConfig, storage_base: &Path) -> Self {
        let token_file = storage_base.join(TOKEN_FILE_NAME);

        if !config.enabled {
            log::info!("Dashboard authentication is disabled");
            return Self {
                enabled: false,
                token: None,
                token_file,
            };
        }

        let token = read_or_create_token(&token_file);

        Self {
            enabled: true,
            token: Some(token),
            token_file,
        }
    }

    /// Return the token value (for cookie signing/verification).
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Return the path to the token file (for CLI queries).
    pub fn token_file(&self) -> &Path {
        &self.token_file
    }

    /// Verify a candidate token against the stored secret.
    pub fn verify_token(&self, candidate: &str) -> bool {
        match &self.token {
            Some(secret) => constant_time_eq(candidate.as_bytes(), secret.as_bytes()),
            None => false,
        }
    }

    /// Create a signed session cookie value.
    ///
    /// The cookie expires after `ttl_secs` seconds (default 24 h).
    pub fn create_session_cookie(&self, ttl_secs: u64) -> String {
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() + ttl_secs)
            .unwrap_or(0);
        match &self.token {
            Some(secret) => sign_cookie(secret, expires),
            None => String::new(),
        }
    }

    /// Verify a session cookie value.
    pub fn verify_session_cookie(&self, cookie_value: &str) -> bool {
        match &self.token {
            Some(secret) => verify_cookie(secret, cookie_value),
            None => false,
        }
    }

    /// Read the token from the file (used by CLI `dashboard` command).
    pub fn read_token_from_file(&self) -> Option<String> {
        if let Some(ref t) = self.token {
            return Some(t.clone());
        }
        std::fs::read_to_string(&self.token_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

// ─── Exempt paths ───────────────────────────────────────────────────────────

/// Paths that do not require authentication (but still subject to localhost-only restriction).
const EXEMPT_PREFIXES: &[&str] = &[
    "/health",
    "/api/auth/login",
    "/api/auth/status",
    "/api/auth/verify",
];

/// Paths that are only accessible from localhost (loopback interface).
/// Non-loopback requests to these paths receive 403 Forbidden.
const LOCALHOST_ONLY_PREFIXES: &[&str] = &["/health", "/metrics"];

fn is_exempt(path: &str) -> bool {
    EXEMPT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn is_localhost_only(path: &str) -> bool {
    LOCALHOST_ONLY_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

struct CanonicalRequestPath {
    decoded: String,
    matched: Option<String>,
}

impl CanonicalRequestPath {
    fn from_request(request: &ServiceRequest) -> Result<Self, ()> {
        let decoded = normalize_path(strict_percent_decode(request.path())?);
        let matched = request.match_pattern().map(normalize_path);
        Ok(Self { decoded, matched })
    }

    fn matches(&self, predicate: impl Fn(&str) -> bool) -> bool {
        predicate(&self.decoded) || self.matched.as_deref().is_some_and(predicate)
    }

    fn is_api(&self) -> bool {
        self.matches(|path| path == "/api" || path.starts_with("/api/"))
    }
}

fn strict_percent_decode(path: &str) -> Result<String, ()> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = bytes.get(index + 1).and_then(|value| hex_value(*value));
        let low = bytes.get(index + 2).and_then(|value| hex_value(*value));
        let (Some(high), Some(low)) = (high, low) else {
            return Err(());
        };
        decoded.push((high << 4) | low);
        index += 3;
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded.as_bytes().windows(3).any(|window| {
        window[0] == b'%' && hex_value(window[1]).is_some() && hex_value(window[2]).is_some()
    }) {
        return Err(());
    }
    Ok(decoded)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn normalize_path(mut path: String) -> String {
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

fn is_enforcement_mutation_path(method: &actix_web::http::Method, path: &str) -> bool {
    match *method {
        actix_web::http::Method::POST => {
            matches!(
                path,
                "/api/enforcement/bindings"
                    | "/api/enforcement/file-bindings"
                    | "/api/enforcement/credential-bindings"
            ) || ["/contain", "/review"].iter().any(|suffix| {
                path.strip_prefix("/api/audit/cases/")
                    .and_then(|case_id| case_id.strip_suffix(suffix))
                    .is_some_and(|case_id| !case_id.is_empty() && !case_id.contains('/'))
            })
        }
        actix_web::http::Method::DELETE => path
            .strip_prefix("/api/enforcement/bindings/")
            .is_some_and(|binding_id| !binding_id.is_empty() && !binding_id.contains('/')),
        _ => false,
    }
}

// ─── actix-web middleware ────────────────────────────────────────────────────

/// actix-web `Transform` that wraps every request with token authentication.
pub struct AuthMiddleware {
    auth: Arc<DashboardAuth>,
}

impl AuthMiddleware {
    pub fn new(auth: Arc<DashboardAuth>) -> Self {
        Self { auth }
    }
}

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Transform = AuthMiddlewareService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddlewareService {
            service,
            auth: self.auth.clone(),
        }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: S,
    auth: Arc<DashboardAuth>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let path = match CanonicalRequestPath::from_request(&req) {
            Ok(path) => path,
            Err(()) => {
                let response = req.into_response(
                    HttpResponse::BadRequest()
                        .json(serde_json::json!({"error": "invalid_path", "message": "Request path encoding is invalid"}))
                        .map_into_right_body(),
                );
                return Box::pin(async move { Ok(response) });
            }
        };
        let is_loopback = req
            .peer_addr()
            .map(|addr| addr.ip().is_loopback())
            .unwrap_or(false);

        // Localhost-only endpoints: reject non-loopback requests with 403.
        if path.matches(is_localhost_only) && !is_loopback {
            let response = req.into_response(
                HttpResponse::Forbidden()
                    .json(serde_json::json!({"error": "forbidden", "message": "This endpoint is only accessible from localhost"}))
                    .map_into_right_body(),
            );
            return Box::pin(async move { Ok(response) });
        }

        // OPTIONS preflight requests: pass through to CORS middleware.
        // Without this, cross-origin preflight to non-exempt /api/* routes
        // would be 401'd before Cors can answer.
        if *req.method() == actix_web::http::Method::OPTIONS {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
        }

        // Kernel-policy mutations always require an explicit credential. Loopback is a
        // network location, not an authorization boundary: unprivileged local processes
        // must not be able to use the root server as a confused deputy.
        if path.matches(|value| is_enforcement_mutation_path(req.method(), value)) {
            let authenticated = self.auth.enabled
                && extract_token(&req)
                    .map(|candidate| {
                        self.auth.verify_token(&candidate)
                            || self.auth.verify_session_cookie(&candidate)
                    })
                    .unwrap_or(false);
            if authenticated {
                let fut = self.service.call(req);
                return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
            }
            let response = req.into_response(
                HttpResponse::Unauthorized()
                    .json(serde_json::json!({"error": "unauthorized", "message": "Authentication required"}))
                    .map_into_right_body(),
            );
            return Box::pin(async move { Ok(response) });
        }

        // If auth is disabled, pass through immediately.
        if !self.auth.enabled {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
        }

        // Localhost (loopback) requests bypass authentication.
        if is_loopback {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
        }

        // Exempt paths pass through.
        if path.matches(is_exempt) {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
        }

        // Try to extract and verify the token or session cookie.
        let authenticated = extract_token(&req)
            .map(|candidate| {
                // Try raw token match first, then session cookie verification.
                self.auth.verify_token(&candidate) || self.auth.verify_session_cookie(&candidate)
            })
            .unwrap_or(false);

        if authenticated {
            let fut = self.service.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) });
        }

        // Not authenticated.
        // For API paths: return 401 JSON.
        // For pages/static assets: pass through — the SPA frontend handles auth via
        // GET /api/auth/status and renders LoginPage client-side.
        if path.is_api() {
            let response = req.into_response(
                HttpResponse::Unauthorized()
                    .json(serde_json::json!({"error": "unauthorized", "message": "Authentication required"}))
                    .map_into_right_body(),
            );
            return Box::pin(async move { Ok(response) });
        }

        let fut = self.service.call(req);
        Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) })
    }
}

/// Extract a candidate token from the request.
///
/// Checks in order:
/// 1. `Authorization: Bearer <token>` header
/// 2. `token` query parameter
/// 3. `agentsight_session` cookie
fn extract_token(req: &ServiceRequest) -> Option<String> {
    // 1. Authorization header
    if let Some(auth_header) = req.headers().get("Authorization") {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                let trimmed = token.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // 2. Query parameter
    let query_string = req.query_string();
    if let Some(token_param) = extract_query_param(query_string, "token") {
        if !token_param.is_empty() {
            return Some(token_param);
        }
    }

    // 3. Session cookie
    if let Some(cookie) = req.cookie("agentsight_session") {
        let value = cookie.value();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    None
}

/// Extract a query parameter value from a query string without full parsing.
fn extract_query_param(query: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for part in query.split('&') {
        if let Some(value) = part.strip_prefix(&prefix) {
            return Some(percent_decode(value));
        }
    }
    None
}

/// Minimal percent-decoding for query values (handles `%XX` sequences).
fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ─── hex encoding (avoid extra dependency) ──────────────────────────────────

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_cookie_roundtrip() {
        let token = "test-token-abc123";
        let ttl = 3600u64;
        let cookie = sign_cookie(
            token,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + ttl,
        );
        assert!(verify_cookie(token, &cookie));
    }

    #[test]
    fn expired_cookie_fails_verification() {
        let token = "test-token-abc123";
        // Already expired (1 second in the past)
        let cookie = sign_cookie(token, 1);
        assert!(!verify_cookie(token, &cookie));
    }

    #[test]
    fn wrong_token_fails_cookie_verification() {
        let token = "correct-token";
        let wrong = "wrong-token";
        let cookie = sign_cookie(
            token,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600,
        );
        assert!(!verify_cookie(wrong, &cookie));
    }

    #[test]
    fn malformed_cookie_fails_verification() {
        let token = "test-token";
        assert!(!verify_cookie(token, ""));
        assert!(!verify_cookie(token, "not-a-number.signature"));
        assert!(!verify_cookie(token, "abc"));
    }

    #[test]
    fn generate_token_produces_64_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 64, "token should be 64 hex characters");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn exempt_paths_are_recognized() {
        assert!(is_exempt("/health"));
        assert!(is_exempt("/health/"));
        assert!(!is_exempt("/metrics")); // requires auth — contains token stats
        assert!(is_exempt("/api/auth/login"));
        assert!(is_exempt("/api/auth/status"));
        assert!(is_exempt("/api/auth/verify"));
        assert!(!is_exempt("/api/sessions"));
        assert!(!is_exempt("/api/interruptions"));
        assert!(!is_exempt("/"));
    }

    #[test]
    fn localhost_only_paths_are_recognized() {
        assert!(is_localhost_only("/health"));
        assert!(is_localhost_only("/health/"));
        assert!(is_localhost_only("/metrics"));
        assert!(is_localhost_only("/metrics?foo=bar"));
        assert!(!is_localhost_only("/api/sessions"));
        assert!(!is_localhost_only("/api/auth/login"));
        assert!(!is_localhost_only("/"));
    }

    #[test]
    fn extract_query_param_parses_correctly() {
        assert_eq!(
            extract_query_param("foo=bar&token=abc123&baz=qux", "token"),
            Some("abc123".to_string())
        );
        assert_eq!(
            extract_query_param("token=hello", "token"),
            Some("hello".to_string())
        );
        assert_eq!(extract_query_param("foo=bar", "token"), None);
    }

    #[test]
    fn percent_decode_handles_simple_cases() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("abc"), "abc");
        assert_eq!(percent_decode("%41%42%43"), "ABC");
    }

    #[test]
    fn dashboard_auth_disabled_passes_through() {
        let config = ServerAuthConfig { enabled: false };
        let auth = DashboardAuth::init(&config, Path::new("/tmp"));
        assert!(!auth.enabled);
        assert!(auth.token().is_none());
    }

    #[test]
    fn dashboard_auth_token_file_path() {
        let config = ServerAuthConfig { enabled: true };
        let auth = DashboardAuth::init(&config, Path::new("/var/log/sysak/.agentsight"));
        assert_eq!(
            auth.token_file(),
            Path::new("/var/log/sysak/.agentsight/.dashboard_token")
        );
    }

    #[test]
    fn dashboard_auth_create_and_verify_session_cookie() {
        let config = ServerAuthConfig { enabled: true };
        let dir = std::env::temp_dir().join("auth_test_cookie");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = DashboardAuth::init(&config, &dir);
        let cookie = auth.create_session_cookie(3600);
        assert!(!cookie.is_empty());
        assert!(auth.verify_session_cookie(&cookie));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dashboard_auth_session_cookie_different_storage_fails() {
        let dir1 = std::env::temp_dir().join("auth_test_cookie_a");
        let dir2 = std::env::temp_dir().join("auth_test_cookie_b");
        let _ = std::fs::remove_dir_all(&dir1);
        let _ = std::fs::remove_dir_all(&dir2);
        std::fs::create_dir_all(&dir1).ok();
        std::fs::create_dir_all(&dir2).ok();
        let config = ServerAuthConfig { enabled: true };
        let auth1 = DashboardAuth::init(&config, &dir1);
        let auth2 = DashboardAuth::init(&config, &dir2);
        let cookie = auth1.create_session_cookie(3600);
        // Different storage dirs → different tokens → cookie should fail
        assert!(!auth2.verify_session_cookie(&cookie));
        let _ = std::fs::remove_dir_all(&dir1);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn dashboard_auth_disabled_returns_empty_cookie() {
        let config = ServerAuthConfig { enabled: false };
        let auth = DashboardAuth::init(&config, Path::new("/tmp"));
        let cookie = auth.create_session_cookie(3600);
        assert!(cookie.is_empty());
        assert!(!auth.verify_session_cookie("anything"));
    }

    #[test]
    fn dashboard_auth_verify_token_when_disabled_returns_false() {
        let config = ServerAuthConfig { enabled: false };
        let auth = DashboardAuth::init(&config, Path::new("/tmp"));
        assert!(!auth.verify_token("any-token"));
    }

    #[test]
    fn read_or_create_token_creates_file_when_missing() {
        let dir = std::env::temp_dir().join("auth_test_create_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let token_file = dir.join(".dashboard_token");
        assert!(!token_file.exists());
        let token = read_or_create_token(&token_file);
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(token_file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_or_create_token_reads_existing_file() {
        let dir = std::env::temp_dir().join("auth_test_read_existing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let token_file = dir.join(".dashboard_token");
        std::fs::write(&token_file, "existing-token-value-1234567890abcdef").ok();
        let token = read_or_create_token(&token_file);
        assert_eq!(token, "existing-token-value-1234567890abcdef");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_or_create_token_regenerates_when_too_short() {
        let dir = std::env::temp_dir().join("auth_test_short");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let token_file = dir.join(".dashboard_token");
        std::fs::write(&token_file, "short").ok();
        let token = read_or_create_token(&token_file);
        assert_eq!(token.len(), 64);
        assert_ne!(token, "short");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dashboard_auth_read_token_from_file_reads_persisted() {
        let dir = std::env::temp_dir().join("auth_test_read_persisted");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let config = ServerAuthConfig { enabled: true };
        let auth = DashboardAuth::init(&config, &dir);
        let token = auth.read_token_from_file();
        assert!(token.is_some());
        assert_eq!(token.as_ref().map(|t| t.len()), Some(64));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dashboard_auth_init_auto_generates_token_file() {
        let dir = std::env::temp_dir().join("auth_test_auto_gen");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let config = ServerAuthConfig { enabled: true };
        let auth = DashboardAuth::init(&config, &dir);
        assert!(auth.enabled);
        assert!(auth.token().is_some());
        assert!(dir.join(".dashboard_token").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_encode_produces_correct_output() {
        assert_eq!(hex::encode([0u8, 1, 2, 255]), "000102ff");
        assert_eq!(hex::encode(b"\x00"), "00");
        assert_eq!(hex::encode([] as [u8; 0]), "");
    }

    // ─── Middleware integration tests ──────────────────────────────────────────

    #[actix_web::test]
    async fn middleware_passes_through_when_disabled() {
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: false },
            Path::new("/tmp"),
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/test",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    #[actix_web::test]
    async fn middleware_requires_auth_for_loopback_enforcement_mutations() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let dir = std::env::temp_dir().join("auth_mw_enforcement_loopback");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let token = auth.token().unwrap_or("").to_string();
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .wrap(AuthMiddleware::new(auth))
                .route(
                    "/api/enforcement/file-bindings",
                    actix_web::web::post().to(|| async { HttpResponse::Ok().finish() }),
                )
                .route(
                    "/api/enforcement/bindings/{binding_id}",
                    actix_web::web::delete().to(|| async { HttpResponse::Ok().finish() }),
                ),
        )
        .await;
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);

        let unauthenticated_apply = actix_web::test::TestRequest::post()
            .uri("/api/enforcement/file-bindings")
            .peer_addr(loopback)
            .to_request();
        let response = actix_web::test::call_service(&app, unauthenticated_apply).await;
        assert_eq!(response.status(), 401);

        let unauthenticated_detach = actix_web::test::TestRequest::delete()
            .uri("/api/enforcement/bindings/00000000-0000-0000-0000-000000000001")
            .peer_addr(loopback)
            .to_request();
        let response = actix_web::test::call_service(&app, unauthenticated_detach).await;
        assert_eq!(response.status(), 401);

        let authenticated_apply = actix_web::test::TestRequest::post()
            .uri("/api/enforcement/file-bindings")
            .peer_addr(loopback)
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = actix_web::test::call_service(&app, authenticated_apply).await;
        assert_eq!(response.status(), 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[actix_web::test]
    async fn middleware_rejects_enforcement_mutations_when_auth_is_disabled() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: false },
            Path::new("/tmp"),
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/enforcement/bindings",
                actix_web::web::post().to(|| async { HttpResponse::Ok().finish() }),
            ),
        )
        .await;
        let request = actix_web::test::TestRequest::post()
            .uri("/api/enforcement/bindings")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345))
            .to_request();

        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), 401);
    }

    #[actix_web::test]
    async fn middleware_rejects_cross_origin_loopback_enforcement_mutation() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let dir = std::env::temp_dir().join("auth_mw_enforcement_origin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/enforcement/file-bindings",
                actix_web::web::post().to(|| async { HttpResponse::Ok().finish() }),
            ),
        )
        .await;
        let request = actix_web::test::TestRequest::post()
            .uri("/api/enforcement/file-bindings")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345))
            .insert_header(("Origin", "https://attacker.example"))
            .to_request();

        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), 401);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[actix_web::test]
    async fn middleware_passes_exempt_paths_without_token() {
        let dir = std::env::temp_dir().join("auth_mw_exempt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .wrap(AuthMiddleware::new(auth))
                .route(
                    "/api/auth/login",
                    actix_web::web::post().to(|| async { HttpResponse::Ok().body("ok") }),
                )
                .route(
                    "/api/auth/status",
                    actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                ),
        )
        .await;
        // GET exempt paths (NOT localhost-only)
        for uri in &["/api/auth/status"] {
            let req = actix_web::test::TestRequest::get().uri(uri).to_request();
            let resp = actix_web::test::call_service(&app, req).await;
            assert_eq!(resp.status(), 200, "exempt path {uri} should pass");
        }
        // POST exempt path
        let req = actix_web::test::TestRequest::post()
            .uri("/api/auth/login")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            200,
            "exempt path /api/auth/login should pass"
        );
    }

    #[actix_web::test]
    async fn middleware_blocks_localhost_only_from_remote() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let dir = std::env::temp_dir().join("auth_mw_localhost_only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .wrap(AuthMiddleware::new(auth))
                .route(
                    "/health",
                    actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                )
                .route(
                    "/metrics",
                    actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                ),
        )
        .await;

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 12345);
        let loopback_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);

        // Remote request to /health should be blocked (403)
        let req = actix_web::test::TestRequest::get()
            .uri("/health")
            .peer_addr(remote_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            403,
            "/health should be forbidden from remote"
        );

        // Remote request to /metrics should be blocked (403)
        let req = actix_web::test::TestRequest::get()
            .uri("/metrics")
            .peer_addr(remote_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            403,
            "/metrics should be forbidden from remote"
        );

        // Loopback request to /health should pass
        let req = actix_web::test::TestRequest::get()
            .uri("/health")
            .peer_addr(loopback_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "/health should pass from loopback");

        // Loopback request to /metrics should pass
        let req = actix_web::test::TestRequest::get()
            .uri("/metrics")
            .peer_addr(loopback_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "/metrics should pass from loopback");
    }

    #[actix_web::test]
    async fn middleware_passes_options_preflight_without_token() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let dir = std::env::temp_dir().join("auth_mw_options");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 12345);

        // GET to protected API from remote should be 401
        let req = actix_web::test::TestRequest::get()
            .uri("/api/sessions")
            .peer_addr(remote_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);

        // OPTIONS preflight to the same API should NOT be 401'd by auth middleware.
        // (404 is expected here — no OPTIONS route registered; in production the
        // CORS middleware handles OPTIONS. The key assertion is: not 401.)
        let req = actix_web::test::TestRequest::default()
            .method(actix_web::http::Method::OPTIONS)
            .uri("/api/sessions")
            .peer_addr(remote_addr)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_ne!(
            resp.status(),
            401,
            "OPTIONS preflight should not be blocked by auth middleware"
        );
    }

    #[actix_web::test]
    async fn middleware_returns_401_for_protected_api_without_token() {
        let dir = std::env::temp_dir().join("auth_mw_401");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/sessions")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn middleware_allows_valid_bearer_token() {
        let dir = std::env::temp_dir().join("auth_mw_bearer");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let token = auth.token().unwrap_or("").to_string();
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/sessions")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    #[actix_web::test]
    async fn middleware_allows_valid_query_token() {
        let dir = std::env::temp_dir().join("auth_mw_query");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let token = auth.token().unwrap_or("").to_string();
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri(&format!("/api/sessions?token={token}"))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    #[actix_web::test]
    async fn middleware_allows_valid_session_cookie() {
        let dir = std::env::temp_dir().join("auth_mw_cookie");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let cookie_value = auth.create_session_cookie(3600);
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/sessions")
            .cookie(actix_web::cookie::Cookie::new(
                "agentsight_session",
                cookie_value,
            ))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    #[actix_web::test]
    async fn middleware_rejects_invalid_bearer_token() {
        let dir = std::env::temp_dir().join("auth_mw_reject");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/api/sessions",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/sessions")
            .insert_header(("Authorization", "Bearer wrong-token"))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn middleware_passes_through_non_api_paths_without_token() {
        let dir = std::env::temp_dir().join("auth_mw_nonapi");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            &dir,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new().wrap(AuthMiddleware::new(auth)).route(
                "/",
                actix_web::web::get().to(|| async { HttpResponse::Ok().body("index") }),
            ),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri("/").to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        // Non-API paths should pass through (SPA handles auth client-side)
        assert_eq!(resp.status(), 200);
    }

    // ─── extract_token unit tests ────────────────────────────────────────────

    #[actix_web::test]
    async fn extract_token_from_bearer_header() {
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test")
            .insert_header(("Authorization", "Bearer abc123"))
            .to_srv_request();
        let token = extract_token(&req);
        assert_eq!(token, Some("abc123".to_string()));
    }

    #[actix_web::test]
    async fn extract_token_from_query_param() {
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test?token=query-tok")
            .to_srv_request();
        let token = extract_token(&req);
        assert_eq!(token, Some("query-tok".to_string()));
    }

    #[actix_web::test]
    async fn extract_token_from_cookie() {
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test")
            .cookie(actix_web::cookie::Cookie::new(
                "agentsight_session",
                "cookie-val",
            ))
            .to_srv_request();
        let token = extract_token(&req);
        assert_eq!(token, Some("cookie-val".to_string()));
    }

    #[actix_web::test]
    async fn extract_token_prefers_bearer_over_query_and_cookie() {
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test?token=query-tok")
            .insert_header(("Authorization", "Bearer bearer-tok"))
            .cookie(actix_web::cookie::Cookie::new(
                "agentsight_session",
                "cookie-val",
            ))
            .to_srv_request();
        let token = extract_token(&req);
        assert_eq!(token, Some("bearer-tok".to_string()));
    }

    #[actix_web::test]
    async fn extract_token_returns_none_when_no_credentials() {
        let req = actix_web::test::TestRequest::get()
            .uri("/api/test")
            .to_srv_request();
        let token = extract_token(&req);
        assert_eq!(token, None);
    }
}

#[cfg(test)]
#[path = "auth_path_tests.rs"]
mod path_tests;
