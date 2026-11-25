//! Bounded OAuth exchange client bound to release-environment endpoints.

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::contract::{
    challenge_data, credentials_data, envelope_or_status, OAuthCredentials, OAuthError,
    CHALLENGE_PATH, CREDENTIALS_PATH, MAX_JSON, MAX_REQUEST_BODY, REVOKE_URL,
};

/// Per-request budget matching the candidate gateway backend timeout.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Connection setup bound.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
tokio::task_local! {
    pub static TEST_IDENTITY_ENDPOINT: String;
    pub static TEST_REVOKE_ENDPOINT: String;
}

/// Challenge data returned by Identity.
pub struct ChallengeResponse {
    /// Single-use proof identifier embedded in the authorization URL.
    pub nonce: String,
    /// Official authorization URL to hand to the system browser.
    pub authorization_url: String,
}

impl std::fmt::Debug for ChallengeResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ChallengeResponse([REDACTED])")
    }
}

/// Bounded OAuth exchange client bound to one release-environment host.
pub struct OAuthClient {
    identity_base: String,
    #[cfg(test)]
    test_identity_endpoint: Option<String>,
    #[cfg(test)]
    test_revoke_endpoint: Option<String>,
}

impl OAuthClient {
    /// Binds the client to an explicit Identity base URL.
    pub fn new(identity_base: String) -> Self {
        Self {
            identity_base,
            #[cfg(test)]
            test_identity_endpoint: TEST_IDENTITY_ENDPOINT.try_with(Clone::clone).ok(),
            #[cfg(test)]
            test_revoke_endpoint: TEST_REVOKE_ENDPOINT.try_with(Clone::clone).ok(),
        }
    }

    fn base(&self) -> Result<String, OAuthError> {
        #[cfg(test)]
        if let Some(value) = &self.test_identity_endpoint {
            let url = reqwest::Url::parse(value).map_err(|_| OAuthError::UntrustedEndpoint)?;
            let loopback = url
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if url.scheme() != "http" || !loopback || url.port().is_none() {
                return Err(OAuthError::UntrustedEndpoint);
            }
            return Ok(value.trim_end_matches('/').to_owned());
        }
        let url =
            reqwest::Url::parse(&self.identity_base).map_err(|_| OAuthError::UntrustedEndpoint)?;
        let trusted = url.scheme() == "https" && {
            #[cfg(feature = "oauth-pre")]
            {
                url.host_str() == Some(super::contract::PRE_HOST)
            }
            #[cfg(not(feature = "oauth-pre"))]
            {
                matches!(
                    url.host_str(),
                    Some(super::super::endpoint::PUBLIC_HOST)
                        | Some(super::super::endpoint::VPC_PROXY_HOST)
                )
            }
        };
        if !trusted {
            return Err(OAuthError::UntrustedEndpoint);
        }
        Ok(self.identity_base.trim_end_matches('/').to_owned())
    }

    fn http(&self) -> Result<reqwest::Client, OAuthError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| OAuthError::Unavailable)
    }

    /// Creates a single-use challenge for the given PKCE pair and state.
    pub async fn challenge(
        &self,
        code_challenge: &str,
        state: &str,
    ) -> Result<ChallengeResponse, OAuthError> {
        let body = json!({"purpose":"exchange","codeChallenge":code_challenge,"state":state});
        let bytes = self.post(CHALLENGE_PATH, &body).await?;
        let (nonce, authorization_url) = challenge_data(&bytes)?;
        Ok(ChallengeResponse {
            nonce,
            authorization_url,
        })
    }

    /// Exchanges an authorization code for the first STS and refresh token.
    pub async fn exchange_code(
        &self,
        nonce: &str,
        code: &str,
        verifier: &str,
    ) -> Result<OAuthCredentials, OAuthError> {
        let body = json!({
            "grantType":"authorization_code","nonce":nonce,"code":code,"codeVerifier":verifier,
        });
        let started = Instant::now();
        let bytes = self.post(CREDENTIALS_PATH, &body).await?;
        credentials_data(&bytes, started)
    }

    /// Renews the STS with the same refresh token; never rotates it.
    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
        let body = json!({"grantType":"refresh_token","refreshToken":refresh_token});
        let started = Instant::now();
        let bytes = self.post(CREDENTIALS_PATH, &body).await?;
        credentials_data(&bytes, started)
    }

    /// Remote revoke is best-effort and reported separately from local logout.
    pub async fn revoke(&self, client_id: &str, refresh_token: &str) -> Result<(), OAuthError> {
        let url = {
            #[cfg(test)]
            {
                self.test_revoke_endpoint
                    .clone()
                    .unwrap_or_else(|| REVOKE_URL.to_owned())
            }
            #[cfg(not(test))]
            {
                REVOKE_URL.to_owned()
            }
        };
        let form = format!(
            "client_id={}&token={}",
            utf8_percent_encode(client_id),
            utf8_percent_encode(refresh_token)
        );
        let response = self
            .http()?
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        let body = bounded_body(response).await?;
        if status == 200 {
            Ok(())
        } else if status == 429 {
            Err(OAuthError::Throttled)
        } else {
            let _ = envelope_or_status(status, &body);
            Err(OAuthError::UnknownServer)
        }
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Vec<u8>, OAuthError> {
        let base = self.base()?;
        let bytes = serde_json::to_vec(body).map_err(|_| OAuthError::InvalidParameter)?;
        if bytes.len() > MAX_REQUEST_BODY {
            return Err(OAuthError::InvalidParameter);
        }
        let action = if path == CHALLENGE_PATH {
            super::contract::CHALLENGE_ACTION
        } else {
            super::contract::CREDENTIALS_ACTION
        };
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let nonce = uuid::Uuid::new_v4().to_string();
        let hashed_payload = super::super::hex_sha256(&bytes);
        let response = self
            .http()?
            .post(format!("{base}{path}"))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("x-acs-action", action)
            .header("x-acs-version", super::super::API_VERSION)
            .header("x-acs-date", &timestamp)
            .header("x-acs-signature-nonce", &nonce)
            .header("x-acs-content-sha256", &hashed_payload)
            .body(bytes)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        let body = bounded_body(response).await?;
        if let Ok(value) = serde_json::from_slice::<Value>(&body) {
            let request_id = value
                .get("requestId")
                .or_else(|| value.get("RequestId"))
                .and_then(Value::as_str)
                .unwrap_or("");
            tracing::info!(request_id, action, status, "oauth response");
        }
        envelope_or_status(status, &body)?;
        Ok(body)
    }
}

fn transport_error(error: reqwest::Error) -> OAuthError {
    if error.is_timeout() {
        OAuthError::Timeout
    } else {
        OAuthError::Unavailable
    }
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, OAuthError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| OAuthError::Unavailable)?;
        if body.len() + chunk.len() > MAX_JSON {
            return Err(OAuthError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn utf8_percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        error_envelope, http_response, success_credentials, OAuthFixture,
    };
    use super::*;

    async fn with_fixture<T>(
        fixture: &OAuthFixture,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        TEST_IDENTITY_ENDPOINT
            .scope(fixture.endpoint.clone(), future)
            .await
    }

    fn fixture_client(fixture: &OAuthFixture) -> OAuthClient {
        TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            OAuthClient::new("https://unused.invalid".into())
        })
    }

    #[tokio::test]
    async fn challenge_posts_the_exact_contract_body() {
        let fixture = OAuthFixture::start().await;
        let client = fixture_client(&fixture);
        let response = with_fixture(&fixture, client.challenge("challenge-43", "state-43"))
            .await
            .unwrap();
        assert_eq!(response.nonce.len(), 43);
        assert!(response
            .authorization_url
            .starts_with("https://signin.aliyun.com/"));
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /api/v1/identity/cosh-oauth/challenge "));
        let body: Value =
            serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body,
            json!({"purpose":"exchange","codeChallenge":"challenge-43","state":"state-43"})
        );
        let head = requests[0].to_lowercase();
        assert!(
            head.contains("x-acs-action: createcoshoauthchallenge"),
            "{head}"
        );
        assert!(head.contains("x-acs-version: 2023-12-30"), "{head}");
        assert!(head.contains("x-acs-content-sha256:"), "{head}");
        assert!(head.contains("x-acs-signature-nonce:"), "{head}");
        assert!(
            !head.contains("authorization:"),
            "anonymous action must not sign"
        );
    }

    #[tokio::test]
    async fn exchange_and_refresh_use_strict_grant_bodies() {
        let fixture = OAuthFixture::start().await;
        let client = fixture_client(&fixture);
        let issued = with_fixture(&fixture, client.exchange_code("nonce", "code", "verifier"))
            .await
            .unwrap();
        assert_eq!(issued.credentials.access_key_id, "STS.test");
        assert_eq!(issued.refresh_token.as_deref(), Some("refresh-canary"));
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &success_credentials(900, false));
        let renewed = with_fixture(&fixture, client.refresh("refresh-canary"))
            .await
            .unwrap();
        assert!(renewed.refresh_token.is_none());
        let bodies = fixture.bodies().await;
        assert_eq!(
            bodies[0],
            json!({"grantType":"authorization_code","nonce":"nonce","code":"code","codeVerifier":"verifier"})
        );
        assert_eq!(
            bodies[1],
            json!({"grantType":"refresh_token","refreshToken":"refresh-canary"})
        );
    }

    #[tokio::test]
    async fn envelope_errors_and_http_fallbacks_are_classified() {
        for (response, expected) in [
            (
                http_response("200 OK", &error_envelope("Sysom.Identity.PermissionDenied")),
                OAuthError::PermissionDenied,
            ),
            (
                http_response(
                    "200 OK",
                    &error_envelope("Sysom.Identity.SysomNotActivated"),
                ),
                OAuthError::SysomNotActivated,
            ),
            (
                http_response("429 Too Many Requests", "slow down"),
                OAuthError::Throttled,
            ),
            (
                http_response("500 Internal Server Error", ""),
                OAuthError::UnknownServer,
            ),
            (
                http_response("403 Forbidden", "gateway denied"),
                OAuthError::UnknownServer,
            ),
        ] {
            let fixture = OAuthFixture::start().await;
            *fixture.credentials_response.lock().await = response;
            let client = fixture_client(&fixture);
            let error = with_fixture(&fixture, client.refresh("refresh-canary"))
                .await
                .unwrap_err();
            assert_eq!(error, expected);
        }
    }

    #[tokio::test]
    async fn redirects_are_never_followed() {
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await =
            "HTTP/1.1 302 Found\r\nLocation: http://evil.example/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
        let client = fixture_client(&fixture);
        let error = with_fixture(&fixture, client.refresh("refresh-canary"))
            .await
            .unwrap_err();
        assert_eq!(error, OAuthError::UnknownServer);
        assert_eq!(fixture.requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn oversized_bodies_are_bounded_on_both_sides() {
        let fixture = OAuthFixture::start().await;
        let client = fixture_client(&fixture);
        let huge = "a".repeat(MAX_REQUEST_BODY);
        let error = with_fixture(&fixture, client.exchange_code("nonce", &huge, "verifier"))
            .await
            .unwrap_err();
        assert_eq!(error, OAuthError::InvalidParameter);
        assert!(
            fixture.requests.lock().await.is_empty(),
            "oversized request reached the wire"
        );
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &"x".repeat(MAX_JSON + 1));
        let error = with_fixture(&fixture, client.refresh("refresh-canary"))
            .await
            .unwrap_err();
        assert_eq!(error, OAuthError::InvalidResponse);
    }

    #[tokio::test]
    async fn untrusted_endpoints_are_rejected_without_test_override() {
        for base in [
            "https://evil.example",
            "http://127.0.0.1:18763",
            "not a url",
        ] {
            let client = OAuthClient::new(base.into());
            assert_eq!(
                client.challenge("challenge", "state").await.unwrap_err(),
                OAuthError::UntrustedEndpoint
            );
        }
        let client = OAuthClient::new(format!("https://{}", super::super::contract::bound_host()));
        assert!(client.base().is_ok());
    }

    #[tokio::test]
    async fn bound_route_whitelist_matches_the_release_profile() {
        let bound = format!("https://{}", super::super::contract::bound_host());
        assert!(OAuthClient::new(bound).base().is_ok());
        #[cfg(feature = "oauth-pre")]
        {
            assert!(
                OAuthClient::new("https://sysom.cn-hangzhou.aliyuncs.com".into())
                    .base()
                    .is_err()
            );
            assert!(
                OAuthClient::new("https://sysom.vpc-proxy.aliyuncs.com".into())
                    .base()
                    .is_err()
            );
        }
        #[cfg(not(feature = "oauth-pre"))]
        {
            assert!(
                OAuthClient::new("https://sysom-pre.cn-hangzhou.aliyuncs.com".into())
                    .base()
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn endpoint_env_override_never_rewrites_the_bound_route() {
        let previous = std::env::var("COSH_SYSOM_ENDPOINT").ok();
        std::env::set_var("COSH_SYSOM_ENDPOINT", "https://evil.example");
        let bound = format!("https://{}", super::super::contract::bound_host());
        assert_eq!(OAuthClient::new(bound.clone()).base().unwrap(), bound);
        match previous {
            Some(value) => std::env::set_var("COSH_SYSOM_ENDPOINT", value),
            None => std::env::remove_var("COSH_SYSOM_ENDPOINT"),
        }
    }

    /// Credential-free route calibration against the approved pre host.
    /// Sends only a synthetic PKCE pair to the anonymous challenge Action.
    #[cfg(feature = "oauth-pre")]
    #[tokio::test]
    #[ignore = "provisioned pre-release OAuth acceptance; requires independent login/route/macOS desktop"]
    async fn pre_anonymous_challenge_readback() {
        let client = OAuthClient::new(format!("https://{}", super::super::contract::PRE_HOST));
        let pkce = super::super::contract::Pkce::generate();
        let state = super::super::contract::random_state();
        let challenge = client
            .challenge(&pkce.challenge, &state)
            .await
            .expect("pre anonymous challenge");
        let client_id = super::super::contract::validate_authorization_url(
            &challenge.authorization_url,
            &state,
            &pkce.challenge,
            &challenge.nonce,
        )
        .expect("authorization URL binding");
        eprintln!(
            "PRE_READBACK ok client_id={client_id} nonce_len={}",
            challenge.nonce.len()
        );
        assert_eq!(client_id, super::super::contract::PRE_CLIENT_ID);
    }

    #[tokio::test]
    async fn revoke_posts_form_to_the_fixed_endpoint() {
        let fixture = OAuthFixture::start().await;
        let client = TEST_REVOKE_ENDPOINT
            .sync_scope(format!("{}/v1/revoke", fixture.endpoint), || {
                fixture_client(&fixture)
            });
        client.revoke("client-1", "refresh+token/=").await.unwrap();
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /v1/revoke "));
        let body = requests[0].split_once("\r\n\r\n").unwrap().1;
        assert_eq!(body, "client_id=client-1&token=refresh%2Btoken%2F%3D");
        assert!(requests[0]
            .to_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"));
    }
}
