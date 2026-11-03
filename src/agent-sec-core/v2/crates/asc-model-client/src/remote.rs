//! Shared, explicitly selected Anthropic access; the local scanner factory remains unchanged.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

/// Failures deliberately exclude provider bodies and credentials.
#[derive(Debug, Error)]
pub enum RemoteError {
    /// The shared configuration or credential reference is invalid.
    #[error("invalid remote model configuration: {0}")]
    Configuration(String),
    /// A classified provider failure, safe to persist in evidence.
    #[error("remote model call failed: {code}")]
    Provider {
        /// Stable diagnostic code.
        code: &'static str,
        /// Only transient transport/status failures may be retried.
        retryable: bool,
        /// Provider-requested delay, bounded by the caller's deadline.
        retry_after: Option<Duration>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileConfig {
    schema_version: u32,
    remote: RemoteConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteConfig {
    provider: String,
    base_url: String,
    default_model: String,
    api_key_env: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Bailian,
    LegacyBailian,
    AnthropicCompatible,
}

/// Resolved shared configuration. Debug output never includes the secret.
#[derive(Clone)]
pub struct RemoteModel {
    base_url: String,
    model: String,
    api_key: String,
    provider: Provider,
}

impl std::fmt::Debug for RemoteModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteModel")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

fn config_error(message: &str) -> RemoteError {
    RemoteError::Configuration(message.into())
}

/// Validate the same official pay-as-you-go endpoints as the Python scoring oracle.
///
/// # Errors
/// Rejects non-HTTPS, userinfo, ports, alternate paths, and unapproved hosts.
pub fn validate_bailian_url(value: &str) -> Result<String, RemoteError> {
    let url = Url::parse(value).map_err(|_| config_error("malformed Bailian endpoint"))?;
    let host = url.host_str().unwrap_or_default();
    let authority = value.split('/').nth(2).unwrap_or_default();
    if value.trim() != value
        || value.chars().any(char::is_control)
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || authority.contains(':')
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "/apps/anthropic" | "/apps/anthropic/")
    {
        return Err(config_error(
            "Bailian requires HTTPS without credentials, ports, query or fragment at /apps/anthropic",
        ));
    }
    let shared = matches!(
        host,
        "dashscope.aliyuncs.com" | "dashscope-intl.aliyuncs.com" | "dashscope-us.aliyuncs.com"
    );
    let labels: Vec<_> = host.split('.').collect();
    let workspace = labels.len() == 5
        && matches!(
            labels[1],
            "cn-beijing" | "ap-southeast-1" | "us-east-1" | "eu-central-1" | "ap-northeast-1"
        )
        && labels[2..] == ["maas", "aliyuncs", "com"]
        && (1..=63).contains(&labels[0].len())
        && labels[0].starts_with(|c: char| c.is_ascii_alphanumeric())
        && labels[0].ends_with(|c: char| c.is_ascii_alphanumeric())
        && labels[0]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if (!shared && !workspace)
        || labels
            .iter()
            .any(|v| matches!(*v, "coding" | "token-plan" | "trial"))
    {
        return Err(config_error(
            "endpoint is not an approved Bailian pay-as-you-go host",
        ));
    }
    Ok(format!("https://{host}/apps/anthropic"))
}

// The operator owns this configuration. Platform callers must authorize the endpoint
// before passing it to ASC; provider-specific routing and billing stay outside ASC.
fn validate_compatible_url(value: &str) -> Result<String, RemoteError> {
    let url = Url::parse(value).map_err(|_| config_error("malformed Anthropic endpoint"))?;
    let authority = value.split('/').nth(2).unwrap_or_default();
    if !value.starts_with("http://") && !value.starts_with("https://")
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || value.contains('\\')
        || url.host_str().is_none_or(|host| {
            host.len() > 253
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || !label.as_bytes()[0].is_ascii_alphanumeric()
                        || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
        || matches!(url.host(), Some(url::Host::Ipv6(_)))
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(config_error(
            "Anthropic endpoint requires an explicit HTTP(S) DNS/IPv4 URL without credentials, query or fragment",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

impl RemoteModel {
    /// Resolve the shared file once, falling back to a legacy key/base pair only when absent.
    ///
    /// # Errors
    /// Invalid existing files never merge with environment configuration.
    pub fn from_environment() -> Result<Self, RemoteError> {
        let path = std::env::var_os("AGENT_SEC_MODEL_CONFIG").map_or_else(
            || "/etc/agent-sec/model-service.json".into(),
            std::path::PathBuf::from,
        );
        Self::resolve(
            &path,
            &std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .collect(),
        )
    }

    /// Resolve configuration using an explicit environment for isolated callers and tests.
    ///
    /// # Errors
    /// Missing keys, invalid models, unknown providers, and unsafe endpoints are rejected.
    pub fn resolve(
        path: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, RemoteError> {
        let (base, model, key, provider) = match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(65_537)
                    .read_to_end(&mut bytes)
                    .map_err(|_| config_error("cannot read shared model file"))?;
                if bytes.len() > 65_536 {
                    return Err(config_error("shared model file exceeds 64 KiB"));
                }
                let config: FileConfig = serde_json::from_slice(&bytes)
                    .map_err(|_| config_error("shared model file has invalid fields or JSON"))?;
                if config.schema_version != 1 {
                    return Err(config_error("unsupported shared model schema"));
                }
                let (base, provider) = match config.remote.provider.as_str() {
                    "bailian-payg-anthropic" => (
                        validate_bailian_url(&config.remote.base_url)?,
                        Provider::Bailian,
                    ),
                    "anthropic-compatible" => (
                        validate_compatible_url(&config.remote.base_url)?,
                        Provider::AnthropicCompatible,
                    ),
                    _ => return Err(config_error("unsupported shared model provider")),
                };
                let name = &config.remote.api_key_env;
                if name.is_empty()
                    || !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(config_error("apiKeyEnv must name an environment variable"));
                }
                let key = environment
                    .get(name)
                    .cloned()
                    .ok_or_else(|| config_error("referenced model key is missing"))?;
                (base, config.remote.default_model, key, provider)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = environment.get("ANTHROPIC_API_KEY").cloned();
                let base = environment.get("ANTHROPIC_BASE_URL").cloned();
                match (key, base) {
                    (Some(key), Some(base)) => (
                        validate_bailian_url(&base)?,
                        "qwen3.8-max".into(),
                        key,
                        Provider::LegacyBailian,
                    ),
                    _ => {
                        return Err(config_error(
                            "configure model-service.json or both ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL",
                        ));
                    }
                }
            }
            Err(_) => return Err(config_error("cannot open shared model file")),
        };
        if key.is_empty() || key.trim() != key || key.chars().any(char::is_control) {
            return Err(config_error(
                "model key is empty or contains whitespace/control characters",
            ));
        }
        if model.is_empty()
            || model.len() > 128
            || !model.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        {
            return Err(config_error("model contains unsupported characters"));
        }
        Ok(Self {
            base_url: base,
            model,
            api_key: key,
            provider,
        })
    }

    /// Whether the deprecated environment pair supplied this configuration.
    pub fn uses_legacy_configuration(&self) -> bool {
        self.provider == Provider::LegacyBailian
    }
    /// Model shared by the Judge and evaluated Agent.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Remove the resolved secret without changing ordinary source assignments.
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.api_key, "[REDACTED]")
    }
    /// Redact decoded evidence without expanding repeated short credentials in memory.
    pub fn redact_without_growth(&self, text: &str) -> String {
        let mask = if self.api_key.len() < "[REDACTED]".len() {
            "*"
        } else {
            "[REDACTED]"
        };
        text.replace(&self.api_key, mask)
    }

    /// Detect the resolved secret in opaque bytes without exposing the credential.
    pub fn contains_secret(&self, bytes: &[u8]) -> bool {
        bytes
            .windows(self.api_key.len())
            .any(|part| part == self.api_key.as_bytes())
    }
    /// Mask exact credential bytes in place; memory use and payload length stay bounded.
    pub fn redact_bytes(&self, bytes: &mut [u8]) {
        let key = self.api_key.as_bytes();
        let mut position = 0;
        while position + key.len() <= bytes.len() {
            if &bytes[position..position + key.len()] == key {
                bytes[position..position + key.len()].fill(b'*');
                position += key.len();
            } else {
                position += 1;
            }
        }
    }
    /// Minimal credential mapping for the evaluated Agent, never serialized as evidence.
    pub fn worker_environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ANTHROPIC_API_KEY".into(), self.api_key.clone()),
            ("ANTHROPIC_AUTH_TOKEN".into(), self.api_key.clone()),
            ("ANTHROPIC_BASE_URL".into(), self.base_url.clone()),
            ("ANTHROPIC_MODEL".into(), self.model.clone()),
            ("ANTHROPIC_SMALL_FAST_MODEL".into(), self.model.clone()),
        ])
    }

    /// Perform one bounded Messages call. Retry policy belongs to the caller's total budget.
    ///
    /// # Errors
    /// Returns sanitized classified HTTP, transport, or response failures.
    pub fn messages(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        high_effort: bool,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        if max_tokens == 0
            || timeout.is_zero()
            || user.trim().is_empty()
            || [system, user].iter().any(|text| {
                text.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            })
        {
            return Err(config_error(
                "model request has an invalid budget or prompt",
            ));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(timeout)
            .redirects(0)
            .build();
        let mut body = json!({"model": self.model, "max_tokens": max_tokens,
            "messages": [{"role":"user", "content": user}], "system": system});
        if self.provider != Provider::AnthropicCompatible {
            body["temperature"] = json!(0);
            body["thinking"] = json!({"type": "enabled"});
            body["output_config"] = json!({"effort": if high_effort { "high" } else { "medium" }});
        }
        let response = agent
            .post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .send_json(body)
            .map_err(|error| match error {
                ureq::Error::Status(status, response) => RemoteError::Provider {
                    code: match status {
                        401 | 403 => "authentication_failed",
                        429 => "rate_limited",
                        408 => "timeout",
                        500..=599 => "server_error",
                        _ => "request_rejected",
                    },
                    retryable: status == 408 || status == 429 || status >= 500,
                    retry_after: response
                        .header("Retry-After")
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs),
                },
                ureq::Error::Transport(transport) => transport_failure(&transport),
            })?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(4_194_305)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    provider_timeout()
                } else {
                    provider_shape()
                }
            })?;
        if bytes.len() > 4_194_304 {
            return Err(provider_shape());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| provider_shape())?;
        let content = value["content"].as_array().ok_or_else(provider_shape)?;
        let text = content
            .iter()
            .filter(|v| v["type"] == "text")
            .map(|v| v["text"].as_str().ok_or_else(provider_shape))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        if text.trim().is_empty()
            || value["usage"]["input_tokens"].as_u64().is_none()
            || value["usage"]["output_tokens"].as_u64().is_none()
        {
            return Err(provider_shape());
        }
        Ok(
            json!({"text": self.redact(&text), "usage": value["usage"], "stop_reason": value["stop_reason"]}),
        )
    }
}

fn transport_failure(transport: &ureq::Transport) -> RemoteError {
    // ureq also wraps TLS InvalidData as ConnectionFailed; inspect typed causes only.
    let mut retryable = transport.kind() == ureq::ErrorKind::ConnectionFailed;
    let mut source = std::error::Error::source(transport);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            match error.kind() {
                std::io::ErrorKind::TimedOut => return provider_timeout(),
                std::io::ErrorKind::InvalidData => retryable = false,
                _ => {}
            }
        }
        source = error.source();
    }
    RemoteError::Provider {
        code: "transport_failed",
        retryable,
        retry_after: None,
    }
}

fn provider_timeout() -> RemoteError {
    RemoteError::Provider {
        code: "timeout",
        retryable: true,
        retry_after: None,
    }
}

fn provider_shape() -> RemoteError {
    RemoteError::Provider {
        code: "invalid_response",
        retryable: false,
        retry_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;

    fn configured_model(provider: &str, base_url: &str) -> Result<RemoteModel, RemoteError> {
        let mut file = tempfile::NamedTempFile::new().expect("create private model fixture");
        serde_json::to_writer(
            &mut file,
            &json!({"schemaVersion":1,"remote":{
                "provider":provider,"baseUrl":base_url,"defaultModel":"fixture-model",
                "apiKeyEnv":"AGENT_SEC_MODEL_API_KEY"
            }}),
        )
        .expect("write shared config");
        RemoteModel::resolve(
            file.path(),
            &BTreeMap::from([(
                "AGENT_SEC_MODEL_API_KEY".into(),
                "opaque-test-context".into(),
            )]),
        )
    }

    fn mock_response(
        status: &str,
        body: &str,
        headers: &str,
    ) -> (String, std::thread::JoinHandle<(String, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy fixture");
        let address = listener.local_addr().expect("fixture address");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept proxy request");
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("bound request read");
            let mut reader = std::io::BufReader::new(&socket);
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(
                    std::io::BufRead::read_line(&mut reader, &mut line).expect("read header"),
                    0
                );
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().expect("body length");
                }
                assert!(head.len() < 16_384);
            }
            assert!(length < 16_384);
            let mut payload = vec![0; length];
            reader.read_exact(&mut payload).expect("read request body");
            drop(reader);
            socket
                .write_all(response.as_bytes())
                .expect("write response");
            (
                head,
                serde_json::from_slice(&payload).expect("JSON request"),
            )
        });
        (format!("http://{address}"), server)
    }

    #[test]
    fn compatible_endpoints_are_explicit_and_do_not_expand_bailian() {
        for base in [
            "https://gateway.example.invalid/anthropic",
            "https://gateway.example.invalid:8443/prefix/anthropic/",
            "http://gateway.svc.cluster.local:8080/proxy",
            "http://127.0.0.1:8080/",
        ] {
            assert!(
                configured_model("anthropic-compatible", base).is_ok(),
                "{base}"
            );
            assert!(configured_model("bailian-payg-anthropic", base).is_err());
        }
        for base in [
            "https://user:secret@gateway.example.invalid/proxy",
            "https://@gateway.example.invalid/proxy",
            "https://gateway.example.invalid/proxy?key=secret",
            "https://gateway.example.invalid/proxy#fragment",
            " https://gateway.example.invalid/proxy",
            "https://gateway.example.invalid/white space",
            "https://gateway.example.invalid/proxy\n",
            "https:gateway.example.invalid/proxy",
            "https://gateway.example.invalid\\proxy",
            "file:///proxy",
            "ftp://gateway.example.invalid/proxy",
            "http://[::1]/proxy",
            "https://gateway.example.invalid./proxy",
            "https://bad_host.example.invalid/proxy",
            "https://-gateway.example.invalid/proxy",
        ] {
            let error = configured_model("anthropic-compatible", base).expect_err("unsafe URL");
            assert!(!error.to_string().contains("secret"));
        }
        assert!(configured_model("unknown-provider", "https://gateway.example.invalid").is_err());
        assert!(
            configured_model(
                "bailian-payg-anthropic",
                "https://dashscope.aliyuncs.com/apps/anthropic"
            )
            .is_ok()
        );
    }

    #[test]
    fn compatible_messages_and_worker_share_opaque_credentials_and_model() {
        let body = r#"{"content":[{"type":"text","text":"result opaque-test-context"}],"usage":{"input_tokens":7,"output_tokens":3},"stop_reason":"end_turn"}"#;
        let (base, server) = mock_response("200 OK", body, "");
        let model = configured_model("anthropic-compatible", &format!("{base}/proxy/anthropic/"))
            .expect("compatible model");
        assert!(!model.uses_legacy_configuration());
        let env = model.worker_environment();
        assert_eq!(env["ANTHROPIC_BASE_URL"], format!("{base}/proxy/anthropic"));
        assert_eq!(env["ANTHROPIC_API_KEY"], "opaque-test-context");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "opaque-test-context");
        assert_eq!(env["ANTHROPIC_MODEL"], "fixture-model");
        assert_eq!(env["ANTHROPIC_SMALL_FAST_MODEL"], "fixture-model");
        assert_eq!(env.len(), 5);
        let result = model
            .messages("system", "user", 123, true, Duration::from_secs(2))
            .expect("Messages response");
        let (head, payload) = server.join().expect("fixture completed");
        assert!(head.starts_with("POST /proxy/anthropic/v1/messages HTTP/1.1\r\n"));
        assert!(
            head.to_ascii_lowercase()
                .contains("x-api-key: opaque-test-context\r\n")
        );
        assert!(
            head.to_ascii_lowercase()
                .contains("anthropic-version: 2023-06-01\r\n")
        );
        assert_eq!(
            payload,
            json!({"model":"fixture-model","max_tokens":123,
            "messages":[{"role":"user","content":"user"}],"system":"system"})
        );
        assert_eq!(result["text"], "result [REDACTED]");
        assert_eq!(result["usage"], json!({"input_tokens":7,"output_tokens":3}));
        assert!(!format!("{model:?}").contains("opaque-test-context"));
    }

    #[test]
    fn explicit_and_legacy_bailian_preserve_reasoning_options() {
        let base = "https://dashscope.aliyuncs.com/apps/anthropic";
        let env = BTreeMap::from([
            ("ANTHROPIC_BASE_URL".into(), base.into()),
            ("ANTHROPIC_API_KEY".into(), "test-key".into()),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let explicit = configured_model("bailian-payg-anthropic", base).unwrap();
        let legacy = RemoteModel::resolve(&directory.path().join("absent.json"), &env).unwrap();
        for (mut model, high_effort) in [(explicit, false), (legacy, true)] {
            let (address, server) = mock_response(
                "200 OK",
                r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
                "",
            );
            model.base_url = address;
            model
                .messages("system", "user", 256, high_effort, Duration::from_secs(2))
                .unwrap();
            let (_, body) = server.join().unwrap();
            assert_eq!(body["temperature"], 0);
            assert_eq!(body["thinking"], json!({"type":"enabled"}));
            assert_eq!(
                body["output_config"]["effort"],
                if high_effort { "high" } else { "medium" }
            );
        }
    }

    #[test]
    fn compatible_errors_preserve_retry_and_usage_boundaries() {
        for (status, body, code, retryable) in [
            (
                "401 Unauthorized",
                "opaque-test-context",
                "authentication_failed",
                false,
            ),
            (
                "403 Forbidden",
                "opaque-test-context",
                "authentication_failed",
                false,
            ),
            (
                "429 Too Many Requests",
                "opaque-test-context",
                "rate_limited",
                true,
            ),
            (
                "503 Unavailable",
                "opaque-test-context",
                "server_error",
                true,
            ),
            (
                "200 OK",
                r#"{"content":[{"type":"text","text":"ok"}]}"#,
                "invalid_response",
                false,
            ),
        ] {
            let (base, server) = mock_response(status, body, "Retry-After: 2\r\n");
            let model = configured_model("anthropic-compatible", &base).expect("model");
            let error = model
                .messages("system", "user", 10, false, Duration::from_secs(2))
                .expect_err("provider failure");
            server.join().expect("fixture completed");
            assert!(matches!(&error, RemoteError::Provider {
                code: actual, retryable: retry, ..
            } if *actual == code && *retry == retryable));
            if retryable {
                assert!(matches!(&error, RemoteError::Provider {
                    retry_after: Some(delay), ..
                } if *delay == Duration::from_secs(2)));
            }
            assert!(!error.to_string().contains("opaque-test-context"));
        }
    }

    #[test]
    fn compatible_credentials_are_not_forwarded_on_redirect() {
        let forbidden = TcpListener::bind("127.0.0.1:0").expect("redirect target");
        forbidden.set_nonblocking(true).expect("nonblocking target");
        let address = forbidden.local_addr().expect("target address");
        let (base, server) =
            mock_response("302 Found", "", &format!("Location: http://{address}/\r\n"));
        let model = configured_model("anthropic-compatible", &base).expect("model");
        assert!(
            model
                .messages("system", "user", 10, false, Duration::from_secs(1))
                .is_err()
        );
        server.join().expect("fixture completed");
        assert!(
            matches!(forbidden.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn typed_transport_timeouts_do_not_expose_or_parse_error_text() {
        for (kind, code, retryable) in [
            (std::io::ErrorKind::TimedOut, "timeout", true),
            (std::io::ErrorKind::InvalidData, "transport_failed", false),
            (std::io::ErrorKind::Other, "transport_failed", false),
        ] {
            let error = ureq::Error::from(std::io::Error::new(
                kind,
                "timeout https://secret:credential@example.invalid/?api_key=secret",
            ));
            let ureq::Error::Transport(transport) = error else {
                panic!("I/O errors must be transports");
            };
            let classified = transport_failure(&transport);
            assert!(matches!(
                classified,
                RemoteError::Provider { code: actual, retryable: actual_retryable, .. }
                    if actual == code && actual_retryable == retryable
            ));
            assert!(!format!("{classified:?}").contains("secret"));
        }
    }

    #[test]
    fn tls_protocol_failure_is_not_a_retryable_connection_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local TLS fixture");
        let address = listener.local_addr().expect("local address");
        let (done, wait) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept TLS request");
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("bound fixture read");
            let mut hello = [0_u8; 4096];
            assert_ne!(
                std::io::Read::read(&mut socket, &mut hello).expect("read TLS hello"),
                0
            );
            socket
                .write_all(&[0xff, 0x03, 0x03, 0, 0])
                .expect("write invalid TLS record");
            // Keep the socket open so a reset cannot replace the TLS protocol error.
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let result = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(1))
            .build()
            .get(&format!("https://{address}/"))
            .call();
        let _ = done.send(());
        server.join().expect("TLS fixture completed");
        let Err(ureq::Error::Transport(transport)) = result else {
            panic!("invalid TLS must fail before receiving HTTP");
        };
        assert_eq!(transport.kind(), ureq::ErrorKind::ConnectionFailed);
        assert_eq!(
            std::error::Error::source(&transport)
                .and_then(|error| error.downcast_ref::<std::io::Error>())
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::InvalidData)
        );
        assert!(matches!(
            transport_failure(&transport),
            RemoteError::Provider {
                code: "transport_failed",
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn http_timeouts_are_retryable_but_invalid_responses_are_not() {
        for (reply, delay, code, retryable) in [
            ("", 250, "timeout", true),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n",
                250,
                "timeout",
                true,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}",
                0,
                "invalid_response",
                false,
            ),
            (
                "invalid HTTP response\r\n\r\n",
                0,
                "transport_failed",
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
            let address = listener.local_addr().expect("local address");
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("accept request");
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("bound fixture read");
                let mut request = std::io::BufReader::new(&socket);
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    assert_ne!(
                        std::io::BufRead::read_line(&mut request, &mut line)
                            .expect("read request header"),
                        0
                    );
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        content_length = value.trim().parse::<usize>().expect("body length");
                    }
                }
                assert!(content_length <= 4096);
                let mut body = vec![0; content_length];
                std::io::Read::read_exact(&mut request, &mut body).expect("read request body");
                drop(request);
                if !reply.is_empty() {
                    socket
                        .write_all(reply.as_bytes())
                        .expect("write fixture response");
                }
                std::thread::sleep(Duration::from_millis(delay));
            });
            let model = RemoteModel {
                base_url: format!("http://{address}"),
                model: "fixture".into(),
                api_key: "fixture-secret".into(),
                provider: Provider::AnthropicCompatible,
            };
            let result = model.messages("system", "user", 10, true, Duration::from_millis(100));
            server.join().expect("HTTP fixture completed");
            assert!(matches!(
                result,
                Err(RemoteError::Provider {
                    code: actual_code,
                    retryable: actual_retryable,
                    ..
                }) if actual_code == code && actual_retryable == retryable
            ));
        }
    }

    #[test]
    fn endpoints_preserve_payg_boundary() {
        assert!(validate_bailian_url("https://dashscope.aliyuncs.com/apps/anthropic").is_ok());
        assert!(
            validate_bailian_url("https://workspace.cn-beijing.maas.aliyuncs.com/apps/anthropic/")
                .is_ok()
        );
        for url in [
            "http://dashscope.aliyuncs.com/apps/anthropic",
            "https://dashscope.aliyuncs.com:443/apps/anthropic",
            "https://coding.cn-beijing.maas.aliyuncs.com/apps/anthropic",
            "https://attacker.example/apps/anthropic",
            "https://dashscope.aliyuncs.com/apps/anthropic?x=1",
        ] {
            assert!(validate_bailian_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn legacy_requires_pair_and_redaction_preserves_source() {
        let absent = Path::new("/nonexistent/asc-test-model-config");
        let mut env = BTreeMap::from([("ANTHROPIC_API_KEY".into(), "test-secret".into())]);
        assert!(RemoteModel::resolve(absent, &env).is_err());
        env.insert(
            "ANTHROPIC_BASE_URL".into(),
            "https://dashscope.aliyuncs.com/apps/anthropic".into(),
        );
        let model = RemoteModel::resolve(absent, &env).expect("valid pair");
        assert_eq!(
            model.redact("token = Path('test-secret').read_text()"),
            "token = Path('[REDACTED]').read_text()"
        );
        assert!(!format!("{model:?}").contains("test-secret"));
    }
    #[test]
    fn exact_credential_mask_preserves_binary_size_and_nonsecret_source() {
        let model = RemoteModel {
            base_url: String::new(),
            model: String::new(),
            api_key: "secret-value".into(),
            provider: Provider::AnthropicCompatible,
        };
        let mut bytes = b"\xffsecret-value\0secret-value/path/read_text()".to_vec();
        let size = bytes.len();
        assert!(model.contains_secret(&bytes));
        model.redact_bytes(&mut bytes);
        assert_eq!(bytes.len(), size);
        assert!(!model.contains_secret(&bytes));
        assert_eq!(
            &bytes[bytes.len() - b"/path/read_text()".len()..],
            b"/path/read_text()"
        );
    }
}
