//! Bounded anonymous ECS identity exchange with safe error classifications.

use super::{ecs_metadata, endpoint, SysomCredentials};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Instant};

const MAX_JSON: usize = 64 * 1024;
const MAX_DOCUMENT: usize = 8 * 1024;
const MAX_DER: usize = 16 * 1024;
pub(super) const PREPARATION_TIMEOUT: Duration = Duration::from_secs(45);
pub(super) const MINIMUM_SEND_TTL: Duration = Duration::from_secs(5);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdentityError {
    Cancelled,
    TrialDisabled,
    LeaseExpired,
    InvalidResponse,
    PermissionDenied,
    InvalidProof,
    ChallengeNotRedeemable,
    Throttled,
    ProviderNotReady,
    StsUnavailable,
    StsInvalidResponse,
    InternalError,
    InvalidParameter,
    Unavailable,
    Timeout,
    Metadata,
    MetadataTransient,
    UntrustedEndpoint,
}

impl IdentityError {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "Sysom.Identity.Cancelled",
            Self::TrialDisabled => "EcsTrialDisabled",
            Self::LeaseExpired => "Sysom.Identity.LeaseExpired",
            Self::InvalidResponse => "Sysom.Identity.InvalidResponse",
            Self::PermissionDenied => "Sysom.Identity.PermissionDenied",
            Self::InvalidProof => "Sysom.Identity.InvalidInstanceProof",
            Self::ChallengeNotRedeemable => "Sysom.Identity.ChallengeNotRedeemable",
            Self::Throttled => "Sysom.Identity.Throttled",
            Self::ProviderNotReady => "Sysom.Identity.ProviderNotReady",
            Self::StsUnavailable => "Sysom.Identity.StsUnavailable",
            Self::StsInvalidResponse => "Sysom.Identity.StsInvalidResponse",
            Self::InternalError => "Sysom.Identity.InternalError",
            Self::InvalidParameter => "Sysom.Identity.InvalidParameter",
            Self::Unavailable => "Sysom.Identity.Unavailable",
            Self::Timeout => "Sysom.Identity.Timeout",
            Self::Metadata | Self::MetadataTransient => "Sysom.Identity.MetadataUnavailable",
            Self::UntrustedEndpoint => "Sysom.Identity.UntrustedEndpoint",
        }
    }

    pub(super) fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Throttled
                | Self::ProviderNotReady
                | Self::StsUnavailable
                | Self::InternalError
                | Self::Unavailable
                | Self::Timeout
                | Self::MetadataTransient
        )
    }
}

impl From<ecs_metadata::ProbeError> for IdentityError {
    fn from(error: ecs_metadata::ProbeError) -> Self {
        if error.is_transient() {
            Self::MetadataTransient
        } else {
            Self::Metadata
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for IdentityError {}

pub(super) struct IdentityCredentials {
    pub(super) credentials: SysomCredentials,
    pub(super) expires_at: Instant,
    pub(super) instance_id: Option<String>,
}

#[cfg(test)]
tokio::task_local! { pub(super) static TEST_ENDPOINT: String; }

pub(super) type AdmissionCheck = Arc<dyn Fn() -> bool + Send + Sync>;

pub(super) struct IdentityClient {
    admission: AdmissionCheck,
    configured_endpoint: String,
    #[cfg(test)]
    test_endpoint: Option<String>,
    #[cfg(test)]
    metadata_endpoint: String,
}

impl IdentityClient {
    pub(super) fn new(configured_endpoint: String) -> Self {
        Self::with_admission(configured_endpoint, Arc::new(|| true))
    }

    pub(super) fn with_admission(configured_endpoint: String, admission: AdmissionCheck) -> Self {
        Self {
            admission,
            configured_endpoint,
            #[cfg(test)]
            test_endpoint: TEST_ENDPOINT.try_with(Clone::clone).ok(),
            #[cfg(test)]
            metadata_endpoint: ecs_metadata::metadata_endpoint(),
        }
    }

    pub(super) async fn check_admission(&self) -> Result<(), IdentityError> {
        let admission = self.admission.clone();
        if tokio::task::spawn_blocking(move || admission())
            .await
            .unwrap_or(false)
        {
            Ok(())
        } else {
            Err(IdentityError::TrialDisabled)
        }
    }

    async fn check_before_stage(&self, active: Option<&AtomicUsize>) -> Result<(), IdentityError> {
        check_active(active)?;
        self.check_admission().await?;
        check_active(active)
    }

    pub(super) async fn exchange(
        &self,
        active: Option<&AtomicUsize>,
    ) -> Result<IdentityCredentials, IdentityError> {
        timeout(PREPARATION_TIMEOUT, self.exchange_inner(active))
            .await
            .unwrap_or(Err(IdentityError::Timeout))
    }

    async fn exchange_inner(
        &self,
        active: Option<&AtomicUsize>,
    ) -> Result<IdentityCredentials, IdentityError> {
        self.check_before_stage(active).await?;
        let endpoint = endpoint::resolve(&self.configured_endpoint).await;
        #[cfg(test)]
        let endpoint = if let Some(value) = &self.test_endpoint {
            let url = reqwest::Url::parse(value).map_err(|_| IdentityError::UntrustedEndpoint)?;
            if url.scheme() != "http"
                || url
                    .host_str()
                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                    .is_none_or(|ip| !ip.is_loopback())
            {
                return Err(IdentityError::UntrustedEndpoint);
            }
            endpoint::ResolvedEndpoint {
                host: format!(
                    "{}:{}",
                    url.host_str().ok_or(IdentityError::UntrustedEndpoint)?,
                    url.port().ok_or(IdentityError::UntrustedEndpoint)?
                ),
                scheme: "http".into(),
                origin: endpoint::EndpointOrigin::VpcProxy,
            }
        } else {
            endpoint
        };
        let allowed = trusted_endpoint(&endpoint);
        #[cfg(test)]
        let allowed = allowed || self.test_endpoint.is_some();
        if !allowed {
            return Err(IdentityError::UntrustedEndpoint);
        }
        let client = endpoint
            .configure_client(reqwest::Client::builder())
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| IdentityError::Unavailable)?;
        self.check_before_stage(active).await?;
        let challenge_started = Instant::now();
        let body = post(
            &client,
            &endpoint,
            "/challenge",
            "CreateEcsModelChallenge",
            None,
        )
        .await?;
        let (challenge_id, audience, ttl) = challenge_data(&body)?;
        #[cfg(test)]
        let metadata_base = self.metadata_endpoint.clone();
        #[cfg(not(test))]
        let metadata_base = ecs_metadata::metadata_endpoint();
        self.check_before_stage(active).await?;
        let session = ecs_metadata::MetadataSession::start(&metadata_base)
            .await
            .map_err(IdentityError::from)?;
        self.check_before_stage(active).await?;
        let (status, document) = session
            .get("/latest/dynamic/instance-identity/document")
            .await
            .map_err(IdentityError::from)?;
        ecs_metadata::check_status(status).map_err(IdentityError::from)?;
        let signed = insert_audience(&document, &audience)?;
        let mut url =
            reqwest::Url::parse("http://localhost/latest/dynamic/instance-identity/pkcs7")
                .map_err(|_| IdentityError::InvalidResponse)?;
        url.query_pairs_mut().append_pair("audience", &audience);
        let path = format!(
            "{}?{}",
            url.path(),
            url.query().ok_or(IdentityError::InvalidResponse)?
        );
        self.check_before_stage(active).await?;
        let (status, proof) = session.get(&path).await.map_err(IdentityError::from)?;
        ecs_metadata::check_status(status).map_err(IdentityError::from)?;
        let payload = serde_json::json!({"challengeId":challenge_id,"signedDocumentBase64":base64(&signed),"pkcs7Base64":canonical_pkcs7(&proof)?});
        if Instant::now() >= challenge_started + ttl {
            return Err(IdentityError::ChallengeNotRedeemable);
        }
        self.check_before_stage(active).await?;
        let started = Instant::now();
        // No exchange retry: any following attempt must acquire a new challenge.
        let body = post(
            &client,
            &endpoint,
            "/credentials",
            "ExchangeEcsModelCredentials",
            Some(&payload),
        )
        .await?;
        let mut credentials = credentials_data(&body, started)?;
        let document: Value =
            serde_json::from_slice(&document).map_err(|_| IdentityError::InvalidResponse)?;
        let instance_id = field(&document, "instance-id")?;
        if !instance_id.starts_with("i-")
            || instance_id.len() <= 2
            || !instance_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(IdentityError::InvalidResponse);
        }
        credentials.instance_id = Some(instance_id.into());
        Ok(credentials)
    }
}

fn check_active(active: Option<&AtomicUsize>) -> Result<(), IdentityError> {
    if active.is_some_and(|count| count.load(Ordering::SeqCst) == 0) {
        Err(IdentityError::Cancelled)
    } else {
        Ok(())
    }
}

async fn post(
    client: &reqwest::Client,
    endpoint: &endpoint::ResolvedEndpoint,
    suffix: &str,
    action: &str,
    payload: Option<&Value>,
) -> Result<Vec<u8>, IdentityError> {
    let mut request = client
        .post(format!(
            "{}/api/v1/identity/ecs-model{suffix}",
            endpoint.base_url()
        ))
        .header("Accept", "application/json")
        .header("x-acs-action", action)
        .header("x-acs-version", "2023-12-30")
        .header(
            "Date",
            Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        );
    if let Some(payload) = payload {
        request = request.json(payload);
    }
    let mut response = request.send().await.map_err(transport_error)?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_JSON as u64)
    {
        return Err(IdentityError::InvalidResponse);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > MAX_JSON - body.len() {
            return Err(IdentityError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(envelope(&body)
            .err()
            .unwrap_or(IdentityError::InvalidResponse));
    }
    Ok(body)
}

fn transport_error(error: reqwest::Error) -> IdentityError {
    if error.is_timeout() {
        IdentityError::Timeout
    } else {
        IdentityError::Unavailable
    }
}

pub(super) fn trusted_endpoint(endpoint: &endpoint::ResolvedEndpoint) -> bool {
    endpoint.scheme == "https"
        && matches!(
            endpoint.host.as_str(),
            endpoint::PUBLIC_HOST | endpoint::VPC_PROXY_HOST
        )
}

fn envelope(body: &[u8]) -> Result<Value, IdentityError> {
    if body.len() > MAX_JSON {
        return Err(IdentityError::InvalidResponse);
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| IdentityError::InvalidResponse)?;
    let code = value
        .get("code")
        .or_else(|| value.get("Code"))
        .and_then(Value::as_str)
        .ok_or(IdentityError::InvalidResponse)?;
    if code != "Success" {
        return Err(match code {
            "Sysom.Identity.PermissionDenied" => IdentityError::PermissionDenied,
            "Sysom.Identity.InvalidInstanceProof" => IdentityError::InvalidProof,
            "Sysom.Identity.ChallengeNotRedeemable" => IdentityError::ChallengeNotRedeemable,
            "Sysom.Identity.Throttled" => IdentityError::Throttled,
            "Sysom.Identity.ProviderNotReady" => IdentityError::ProviderNotReady,
            "Sysom.Identity.StsUnavailable" => IdentityError::StsUnavailable,
            "Sysom.Identity.StsInvalidResponse" => IdentityError::StsInvalidResponse,
            "Sysom.Identity.InternalError" => IdentityError::InternalError,
            "Sysom.Identity.InvalidParameter" => IdentityError::InvalidParameter,
            _ => IdentityError::InvalidResponse,
        });
    }
    if value.get("success").and_then(Value::as_bool) != Some(true)
        || !value.get("data").is_some_and(Value::is_object)
    {
        return Err(IdentityError::InvalidResponse);
    }
    Ok(value["data"].clone())
}

fn field<'a>(data: &'a Value, name: &str) -> Result<&'a str, IdentityError> {
    data.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or(IdentityError::InvalidResponse)
}

fn lifetime(data: &Value, expires: &str, max: i64) -> Result<Duration, IdentityError> {
    let server = DateTime::parse_from_rfc3339(field(data, "serverTime")?)
        .map_err(|_| IdentityError::InvalidResponse)?;
    let expiry = DateTime::parse_from_rfc3339(field(data, expires)?)
        .map_err(|_| IdentityError::InvalidResponse)?;
    let ttl = expiry.signed_duration_since(server);
    if ttl <= chrono::Duration::zero() || ttl > chrono::Duration::seconds(max) {
        return Err(IdentityError::InvalidResponse);
    }
    ttl.to_std().map_err(|_| IdentityError::InvalidResponse)
}

fn challenge_data(body: &[u8]) -> Result<(String, String, Duration), IdentityError> {
    let data = envelope(body)?;
    let id = field(&data, "challengeId")?;
    if id.len() != 43
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(IdentityError::InvalidResponse);
    }
    let standard = id.replace('-', "+").replace('_', "/") + "=";
    let raw = decode_base64(standard.as_bytes())?;
    if raw.len() != 32 {
        return Err(IdentityError::InvalidResponse);
    }
    let audience = field(&data, "audience")?;
    if audience.len() > 1024
        || !audience
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._/-".contains(&b))
        || !audience.starts_with("sysom:ecs-model:")
        || !audience.ends_with(&format!(":v1:{id}"))
    {
        return Err(IdentityError::InvalidResponse);
    }
    Ok((
        id.into(),
        audience.into(),
        lifetime(&data, "expiresAt", 120)?,
    ))
}

fn credentials_data(body: &[u8], started: Instant) -> Result<IdentityCredentials, IdentityError> {
    let data = envelope(body)?;
    let expires_at = started + lifetime(&data, "expiration", 930)?;
    if expires_at <= Instant::now() {
        return Err(IdentityError::InvalidResponse);
    }
    Ok(IdentityCredentials {
        credentials: SysomCredentials {
            access_key_id: field(&data, "accessKeyId")?.into(),
            access_key_secret: field(&data, "accessKeySecret")?.into(),
            security_token: Some(field(&data, "securityToken")?.into()),
        },
        expires_at,
        instance_id: None,
    })
}

fn insert_audience(document: &[u8], audience: &str) -> Result<Vec<u8>, IdentityError> {
    if document.len() > MAX_DOCUMENT {
        return Err(IdentityError::InvalidResponse);
    }
    let parsed: Value =
        serde_json::from_slice(document).map_err(|_| IdentityError::InvalidResponse)?;
    let object = parsed.as_object().ok_or(IdentityError::InvalidResponse)?;
    if object.contains_key("audience") || !audience.is_ascii() {
        return Err(IdentityError::InvalidResponse);
    }
    let closing = document
        .iter()
        .rposition(|b| *b == b'}')
        .ok_or(IdentityError::InvalidResponse)?;
    let mut signed = document[..closing].to_vec();
    if !object.is_empty() {
        signed.push(b',');
    }
    signed.extend_from_slice(b"\"audience\":");
    signed.extend_from_slice(
        serde_json::to_string(audience)
            .map_err(|_| IdentityError::InvalidResponse)?
            .as_bytes(),
    );
    signed.extend_from_slice(&document[closing..]);
    if signed.len() > MAX_DOCUMENT {
        return Err(IdentityError::InvalidResponse);
    }
    Ok(signed)
}

fn base64(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn decode_base64(input: &[u8]) -> Result<Vec<u8>, IdentityError> {
    if input.is_empty() || !input.len().is_multiple_of(4) || input.len() > MAX_DER.div_ceil(3) * 4 {
        return Err(IdentityError::InvalidResponse);
    }
    let mut output = Vec::new();
    for group in input.chunks(4) {
        let mut word = 0u32;
        for byte in group {
            word = (word << 6)
                | if *byte == b'=' {
                    0
                } else {
                    ALPHABET
                        .iter()
                        .position(|b| b == byte)
                        .ok_or(IdentityError::InvalidResponse)? as u32
                };
        }
        output.push((word >> 16) as u8);
        if group[2] != b'=' {
            output.push((word >> 8) as u8);
        }
        if group[3] != b'=' {
            output.push(word as u8);
        }
    }
    if output.len() > MAX_DER || base64(&output).as_bytes() != input {
        return Err(IdentityError::InvalidResponse);
    }
    Ok(output)
}

fn canonical_pkcs7(bytes: &[u8]) -> Result<String, IdentityError> {
    let compact: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    decode_base64(&compact)?;
    String::from_utf8(compact).map_err(|_| IdentityError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn challenge() -> Value {
        serde_json::json!({"code":"Success","success":true,"data":{"challengeId":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","audience":"sysom:ecs-model:prod:cn-hangzhou:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","serverTime":"2026-01-01T00:00:00Z","expiresAt":"2026-01-01T00:02:00Z"}})
    }
    fn credentials() -> Value {
        serde_json::json!({"code":"Success","success":true,"data":{"accessKeyId":"STS.test","accessKeySecret":"secret-canary","securityToken":"token-canary","serverTime":"2026-01-01T00:00:00Z","expiration":"2026-01-01T00:15:00Z"}})
    }
    #[test]
    fn preserves_original_document_bytes() {
        assert_eq!(
            insert_audience(b"{\n  \"instance-id\" : \"i-test\" \n}\n", "sysom:test").unwrap(),
            b"{\n  \"instance-id\" : \"i-test\" \n,\"audience\":\"sysom:test\"}\n"
        );
        assert_eq!(
            insert_audience(b"{}", "a").unwrap(),
            b"{\"audience\":\"a\"}"
        );
        for invalid in [b"[]".as_slice(), b"{}garbage", b"{\"audience\":\"old\"}"] {
            assert!(insert_audience(invalid, "a").is_err());
        }
        assert!(insert_audience(&vec![b' '; 8193], "a").is_err());
    }
    #[test]
    fn base64_and_pkcs7_are_canonical() {
        for (raw, encoded) in [
            (b"f".as_slice(), "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (&[251, 255, 255], "+///"),
        ] {
            assert_eq!(base64(raw), encoded);
        }
        assert_eq!(canonical_pkcs7(b" Zm9v\r\n").unwrap(), "Zm9v");
        for invalid in [b"Zh==".as_slice(), b"Zg", b"Zg===", b"-___", b""] {
            assert!(canonical_pkcs7(invalid).is_err());
        }
    }
    #[test]
    fn validates_challenge_envelope_id_and_lifetime() {
        let good = challenge();
        assert_eq!(
            challenge_data(&serde_json::to_vec(&good).unwrap())
                .unwrap()
                .2,
            Duration::from_secs(120)
        );
        for (field, value) in [
            ("challengeId", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB"),
            (
                "challengeId",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ),
            ("audience", ""),
            ("expiresAt", "2026-01-01T00:02:01Z"),
            ("expiresAt", "2026-01-01T00:00:00Z"),
        ] {
            let mut bad = good.clone();
            bad["data"][field] = value.into();
            assert!(challenge_data(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = good;
        bad["success"] = false.into();
        assert!(challenge_data(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    #[test]
    fn expiry_is_server_relative_and_bounded() {
        let started = Instant::now() - Duration::from_secs(5);
        let good = credentials();
        let result = credentials_data(&serde_json::to_vec(&good).unwrap(), started).unwrap();
        assert_eq!(result.expires_at, started + Duration::from_secs(900));
        for expiry in ["2026-01-01T00:15:31Z", "2026-01-01T00:00:00Z", "bad"] {
            let mut bad = good.clone();
            bad["data"]["expiration"] = expiry.into();
            assert!(credentials_data(&serde_json::to_vec(&bad).unwrap(), started).is_err());
        }
    }
    #[test]
    fn identity_denial_is_not_business_activation() {
        assert!(matches!(challenge_data(br#"{"success":false,"code":"Sysom.Identity.PermissionDenied","message":"secret-canary"}"#),Err(IdentityError::PermissionDenied)));
    }
    #[test]
    fn only_trusted_production_endpoints_are_allowed() {
        use super::super::endpoint::{EndpointOrigin, ResolvedEndpoint};
        let endpoint = |host: &str, scheme: &str| ResolvedEndpoint {
            host: host.into(),
            scheme: scheme.into(),
            origin: EndpointOrigin::Configured,
        };
        assert!(trusted_endpoint(&endpoint(
            super::super::endpoint::PUBLIC_HOST,
            "https"
        )));
        for (host, scheme) in [
            ("evil.example", "https"),
            (super::super::endpoint::PUBLIC_HOST, "http"),
            ("127.0.0.1:1234", "http"),
        ] {
            assert!(!trusted_endpoint(&endpoint(host, scheme)));
        }
    }
    #[tokio::test]
    async fn anonymous_exchange_uses_imdsv2_and_only_three_fields() {
        let fixture = super::super::identity_test_support::Fixture::start("Success", false).await;
        let credentials = fixture.client().await.exchange(None).await.unwrap();
        assert_eq!(credentials.credentials.access_key_id, "STS.test");
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 5);
        let challenge = &requests[0];
        let exchange = &requests[4];
        for (request, action) in [
            (challenge, "createecsmodelchallenge"),
            (exchange, "exchangeecsmodelcredentials"),
        ] {
            let lower = request.to_lowercase();
            assert!(lower.contains(&format!("x-acs-action: {action}")));
            assert!(lower.contains("x-acs-version: 2023-12-30"));
            assert!(lower.contains("accept: application/json"));
            assert!(lower.contains("date: "));
            assert!(!lower.contains("authorization:"));
            assert!(!lower.contains("x-acs-security-token"));
        }
        assert!(requests[1].starts_with("PUT /latest/api/token"));
        for request in [&requests[2], &requests[3]] {
            assert!(request
                .to_lowercase()
                .contains("x-aliyun-ecs-metadata-token: imds-token-canary"));
        }
        assert!(
            requests[3].contains("pkcs7?audience=sysom%3Aecs-model%3Aprod%3Acn-hangzhou%3Av1%3A")
        );
        let payload: Value =
            serde_json::from_str(exchange.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(payload.as_object().unwrap().len(), 3);
        assert_eq!(payload["pkcs7Base64"], "Zm9v");
        assert_eq!(payload["signedDocumentBase64"],base64(b"{\n \"instance-id\": \"i-test\"\n,\"audience\":\"sysom:ecs-model:prod:cn-hangzhou:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}\n"));
    }
    #[tokio::test]
    async fn rejected_exchange_is_never_replayed() {
        let fixture = super::super::identity_test_support::Fixture::start(
            "Sysom.Identity.PermissionDenied",
            false,
        )
        .await;
        assert!(matches!(
            fixture.client().await.exchange(None).await,
            Err(IdentityError::PermissionDenied)
        ));
        assert_eq!(
            fixture
                .requests
                .lock()
                .await
                .iter()
                .filter(|r| r.starts_with("POST /api/v1/identity/ecs-model/credentials"))
                .count(),
            1
        );
    }
    #[test]
    fn credential_envelopes_are_complete_bounded_and_never_echo_secrets() {
        let good = credentials();
        for name in [
            "accessKeyId",
            "accessKeySecret",
            "securityToken",
            "serverTime",
            "expiration",
        ] {
            let mut bad = good.clone();
            bad["data"][name] = "".into();
            assert!(matches!(
                credentials_data(&serde_json::to_vec(&bad).unwrap(), Instant::now()),
                Err(IdentityError::InvalidResponse)
            ));
        }
        let mut latest = good.clone();
        latest["data"]["expiration"] = "2026-01-01T00:15:30Z".into();
        assert!(credentials_data(&serde_json::to_vec(&latest).unwrap(), Instant::now()).is_ok());
        assert!(matches!(
            credentials_data(&vec![b' '; MAX_JSON + 1], Instant::now()),
            Err(IdentityError::InvalidResponse)
        ));
        let issued = credentials_data(&serde_json::to_vec(&good).unwrap(), Instant::now()).unwrap();
        assert_eq!(
            format!("{:?}", issued.credentials),
            "SysomCredentials([REDACTED])"
        );
        for code in [
            "Sysom.Identity.PermissionDenied",
            "Sysom.Identity.InvalidInstanceProof",
            "Sysom.Identity.ChallengeNotRedeemable",
            "Sysom.Identity.Throttled",
            "Sysom.Identity.ProviderNotReady",
            "Sysom.Identity.StsUnavailable",
            "Sysom.Identity.StsInvalidResponse",
            "Sysom.Identity.InternalError",
            "Sysom.Identity.InvalidParameter",
        ] {
            let payload =
                serde_json::json!({"success":false,"code":code,"message":"secret-canary"});
            let error = envelope(&serde_json::to_vec(&payload).unwrap()).unwrap_err();
            assert_eq!(error.code(), code);
            assert!(!format!("{error:?} {error}").contains("canary"));
        }
        assert!(canonical_pkcs7(base64(&vec![0; MAX_DER]).as_bytes()).is_ok());
        assert!(canonical_pkcs7(base64(&vec![0; MAX_DER + 1]).as_bytes()).is_err());
    }
}
