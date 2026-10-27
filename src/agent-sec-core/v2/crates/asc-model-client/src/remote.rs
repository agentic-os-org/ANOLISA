//! Shared, explicitly selected Bailian access; the local scanner factory remains unchanged.

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

/// Resolved shared configuration. Debug output never includes the secret.
#[derive(Clone)]
pub struct RemoteModel {
    base_url: String,
    model: String,
    api_key: String,
    legacy: bool,
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
        let (base, model, key, legacy) = match std::fs::File::open(path) {
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
                if config.schema_version != 1 || config.remote.provider != "bailian-payg-anthropic"
                {
                    return Err(config_error("unsupported shared model schema or provider"));
                }
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
                (
                    config.remote.base_url,
                    config.remote.default_model,
                    key,
                    false,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = environment.get("ANTHROPIC_API_KEY").cloned();
                let base = environment.get("ANTHROPIC_BASE_URL").cloned();
                match (key, base) {
                    (Some(key), Some(base)) => (base, "qwen3.8-max".into(), key, true),
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
            base_url: validate_bailian_url(&base)?,
            model,
            api_key: key,
            legacy,
        })
    }

    /// Whether the deprecated environment pair supplied this configuration.
    pub fn uses_legacy_configuration(&self) -> bool {
        self.legacy
    }
    /// Model shared by the Judge and evaluated Agent.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Remove the resolved secret without changing ordinary source assignments.
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.api_key, "[REDACTED]")
    }
    /// Redact a diagnostic containing raw text or Debug-escaped paths.
    pub fn redact_diagnostic(&self, text: &str) -> String {
        let escaped = format!("{:?}", self.api_key);
        let escaped = &escaped[1..escaped.len() - 1];
        self.redact(&text.replace(escaped, &self.api_key))
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
        let body = json!({"model": self.model, "max_tokens": max_tokens,
            "messages": [{"role":"user", "content": user}], "system": system,
            "temperature": 0, "thinking":{"type":"enabled"},
            "output_config":{"effort": if high_effort { "high" } else { "medium" }}});
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
                legacy: false,
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
    fn diagnostics_redact_raw_and_escaped_credentials_without_rewriting_paths() {
        let model = RemoteModel {
            base_url: String::new(),
            model: String::new(),
            api_key: "fixture\"key\\with\ncontrol".into(),
            provider: Provider::AnthropicCompatible,
        };
        let message = format!(
            "path={:?}; raw={}; other=\"/tmp/line\\nname\"",
            model.api_key, model.api_key
        );
        assert_eq!(
            model.redact_diagnostic(&message),
            "path=\"[REDACTED]\"; raw=[REDACTED]; other=\"/tmp/line\\nname\""
        );
    }

    #[test]
    fn exact_credential_mask_preserves_binary_size_and_nonsecret_source() {
        let model = RemoteModel {
            base_url: String::new(),
            model: String::new(),
            api_key: "secret-value".into(),
            legacy: false,
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
