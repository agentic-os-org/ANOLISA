//! Bounded JSON completions through an OpenAI-compatible HTTPS service or an
//! offline fixture. This module owns transport, not task semantics or policy.
//!
//! Cloud requests disable redirects, environment proxies and automatic retries.
//! System DNS resolution cannot be interrupted; expired results are rejected.

#![deny(missing_docs)]

use std::io::Read as _;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

/// Maximum assistant content size in bytes, independent of the HTTP envelope.
pub const MAX_CONTENT_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 128 * 1024;

/// Provider failures without credentials, request text or raw service errors.
#[derive(Debug, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// The URL or API key violates the cloud configuration constraints.
    #[error("provider configuration is invalid; use an HTTPS base URL and a nonempty API key")]
    InvalidConfiguration,
    /// The supplied deadline expired before accepting a response.
    #[error("model deadline exceeded")]
    DeadlineExceeded,
    /// Transport or response reading failed without a usable response.
    #[error("model request failed")]
    Transport,
    /// The service returned a status other than HTTP 200.
    #[error("model service returned HTTP {0}")]
    Http(u16),
    /// The completion is malformed, unfinished, ambiguous or contains tool calls.
    #[error("model response is incomplete or invalid")]
    InvalidResponse,
    /// The HTTP envelope or assistant content exceeds its byte limit.
    #[error("model response exceeds the byte limit")]
    ResponseTooLarge,
}

/// Injectable JSON completion transport, independent of caller semantics.
///
/// Implementations must reject expired results and enforce [`MAX_CONTENT_BYTES`].
/// Callers still validate the content against their own semantic contract.
pub trait ModelProvider: Send + Sync {
    /// Complete one request using separate system instructions and user data.
    ///
    /// `deadline` covers dispatch and content retrieval, not a fresh timeout for
    /// each phase. Blocking system DNS may delay returning a timeout error.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] for expiry, transport or HTTP failure, invalid
    /// completion envelopes, or oversized responses. Errors omit input and keys.
    fn complete(
        &self,
        model: &str,
        system: &str,
        input: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError>;
}

/// Synchronous HTTPS JSON completion provider with no retries or redirects.
///
/// Sends caller-supplied system and user text to the configured service. Accepts
/// one completed assistant response, bounded to 128 KiB for the HTTP envelope
/// and [`MAX_CONTENT_BYTES`] for content; keys are confined to the auth header.
pub struct OpenAiCompatibleProvider {
    endpoint: String,
    api_key: String,
    disable_thinking: bool,
}

impl OpenAiCompatibleProvider {
    /// Configure the API base URL and owned key without making a network request.
    ///
    /// Appends `/chat/completions` to `base_url`. The key must contain 1..4096
    /// printable ASCII bytes without spaces; the URL is limited to 2048 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidConfiguration`] for invalid keys or URLs,
    /// non-HTTPS schemes, missing hosts, userinfo, query strings or fragments.
    pub fn new(base_url: &str, api_key: String) -> Result<Self, ProviderError> {
        let url = Url::parse(base_url).map_err(|_| ProviderError::InvalidConfiguration)?;
        if base_url.len() > 2048
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || api_key.is_empty()
            || api_key.len() > 4096
            || !api_key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        Ok(Self {
            endpoint: format!("{}/chat/completions", url.as_str().trim_end_matches('/')),
            api_key,
            disable_thinking: false,
        })
    }

    /// Sends `enable_thinking: false` so hybrid reasoning models answer directly.
    ///
    /// Reasoning tokens share `max_tokens` with the answer; a model that keeps
    /// reasoning ends with `finish_reason: length` and no content. The field is a
    /// `DashScope` extension; services that reject unknown parameters cannot use it.
    #[must_use]
    pub fn without_thinking(mut self) -> Self {
        self.disable_thinking = true;
        self
    }

    fn agent_builder(deadline: Instant) -> Result<ureq::AgentBuilder, ProviderError> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(ProviderError::DeadlineExceeded)?;
        Ok(ureq::AgentBuilder::new()
            .redirects(0)
            .try_proxy_from_env(false)
            // ureq's default connect timeout overrides its overall request timeout.
            .timeout_connect(remaining)
            .timeout(remaining))
    }
}

impl ModelProvider for OpenAiCompatibleProvider {
    fn complete(
        &self,
        model: &str,
        system: &str,
        input: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError> {
        if Instant::now() >= deadline {
            return Err(ProviderError::DeadlineExceeded);
        }
        let mut payload = json!({
            "model": model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": input}],
            "stream": false,
            "n": 1,
            "temperature": 0,
            "seed": 42,
            "max_tokens": 4096,
            "response_format": {"type": "json_object"},
        });
        if self.disable_thinking {
            payload["enable_thinking"] = Value::Bool(false);
        }
        // shortcut: system DNS is blocking; use a cancellable resolver for a hard deadline.
        let response = Self::agent_builder(deadline)?
            .build()
            .post(&self.endpoint)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(&payload)
            .map_err(|error| match error {
                ureq::Error::Status(status, _) => ProviderError::Http(status),
                ureq::Error::Transport(_) if Instant::now() >= deadline => {
                    ProviderError::DeadlineExceeded
                }
                ureq::Error::Transport(_) => ProviderError::Transport,
            })?;
        if Instant::now() >= deadline {
            return Err(ProviderError::DeadlineExceeded);
        }
        if response.status() != 200 {
            return Err(ProviderError::Http(response.status()));
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                if Instant::now() >= deadline {
                    ProviderError::DeadlineExceeded
                } else {
                    ProviderError::Transport
                }
            })?;
        if u64::try_from(bytes.len()).map_err(|_| ProviderError::ResponseTooLarge)?
            > MAX_RESPONSE_BYTES
        {
            return Err(ProviderError::ResponseTooLarge);
        }
        let content = response_content(&bytes)?;
        if Instant::now() >= deadline {
            return Err(ProviderError::DeadlineExceeded);
        }
        Ok(content)
    }
}

/// Offline replay of fixed content, not semantic interpretation of new inputs.
///
/// Model, system and input arguments are ignored; callers must pair this fixture
/// with the task it represents. Size and deadline checks still apply.
pub struct FakeProvider {
    content: String,
}

impl FakeProvider {
    /// Store fixture content; its size is checked when a completion is requested.
    pub fn new(content: String) -> Self {
        Self { content }
    }
}

impl ModelProvider for FakeProvider {
    fn complete(
        &self,
        _: &str,
        _: &str,
        _: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError> {
        if Instant::now() >= deadline {
            return Err(ProviderError::DeadlineExceeded);
        }
        if self.content.len() > MAX_CONTENT_BYTES {
            return Err(ProviderError::ResponseTooLarge);
        }
        Ok(self.content.clone())
    }
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    finish_reason: String,
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    role: String,
    content: String,
    tool_calls: Option<Value>,
    function_call: Option<Value>,
    refusal: Option<Value>,
}

fn response_content(bytes: &[u8]) -> Result<String, ProviderError> {
    let completion: Completion =
        serde_json::from_slice(bytes).map_err(|_| ProviderError::InvalidResponse)?;
    if completion.error.is_some() || completion.choices.len() != 1 {
        return Err(ProviderError::InvalidResponse);
    }
    let choice = completion
        .choices
        .into_iter()
        .next()
        .ok_or(ProviderError::InvalidResponse)?;
    if choice.finish_reason != "stop"
        || choice.message.role != "assistant"
        || choice.message.content.trim().is_empty()
        || choice.message.tool_calls.is_some()
        || choice.message.function_call.is_some()
        || choice.message.refusal.is_some()
    {
        return Err(ProviderError::InvalidResponse);
    }
    if choice.message.content.len() > MAX_CONTENT_BYTES {
        return Err(ProviderError::ResponseTooLarge);
    }
    Ok(choice.message.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;
    use std::time::Duration;

    fn envelope(content: &str) -> Value {
        json!({"choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant", "content": content,
                "tool_calls": null, "function_call": null, "refusal": null
            }
        }]})
    }

    fn server(
        status: u16,
        body: String,
        delay: Duration,
    ) -> (OpenAiCompatibleProvider, std::thread::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /compatible-mode/v1/chat/completions "));
            let mut length = 0;
            let mut authorized = false;
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                authorized |= lower.trim() == "authorization: bearer test-key";
            }
            assert!(authorized);
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request = serde_json::from_slice(&bytes).unwrap();
            let headers = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/redirect\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).unwrap();
            std::thread::sleep(delay);
            let _ = stream.write_all(body.as_bytes());
            request
        });
        let mut provider = OpenAiCompatibleProvider::new(
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "test-key".into(),
        )
        .unwrap();
        provider.endpoint = format!("http://127.0.0.1:{port}/compatible-mode/v1/chat/completions");
        (provider, handle)
    }

    #[test]
    fn openai_request_returns_content_without_caller_semantics() {
        let content = r#"{"answer":"fixture"}"#;
        let input = r#"{"input":"user data"}"#;
        let (provider, handle) = server(200, envelope(content).to_string(), Duration::ZERO);
        let output = provider
            .complete(
                "qwen-plus",
                "system instructions",
                input,
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(output, content);
        let request = handle.join().unwrap();
        assert_eq!(request["model"], "qwen-plus");
        assert_eq!(request["response_format"], json!({"type": "json_object"}));
        assert_eq!(request["max_tokens"], 4096);
        assert_eq!(request["temperature"], 0);
        assert_eq!(request["seed"], 42);
        assert_eq!(request["n"], 1);
        assert_eq!(request["stream"], false);
        assert_eq!(
            request["messages"],
            json!([
                {"role": "system", "content": "system instructions"},
                {"role": "user", "content": input}
            ])
        );
        for field in [
            "options",
            "format",
            "tools",
            "logprobs",
            "api_key",
            "enable_thinking",
        ] {
            assert!(request.get(field).is_none());
        }
    }

    #[test]
    fn disabled_thinking_is_sent_only_when_requested() {
        let (provider, handle) = server(200, envelope("{}").to_string(), Duration::ZERO);
        provider
            .without_thinking()
            .complete(
                "deepseek-v4.1-flash",
                "system",
                "{}",
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(handle.join().unwrap()["enable_thinking"], false);
    }

    #[test]
    fn cloud_configuration_rejects_plaintext_userinfo_and_header_injection() {
        for url in [
            "http://127.0.0.1:11434",
            "ftp://example.com",
            "not a URL",
            "https://user:SECRET@example.com/v1",
            "https://example.com/v1?key=SECRET",
            "https://example.com/v1#SECRET",
        ] {
            let error = OpenAiCompatibleProvider::new(url, "test-key".into())
                .err()
                .unwrap();
            assert_eq!(error, ProviderError::InvalidConfiguration);
            assert!(!format!("{error:?}: {error}").contains("SECRET"));
        }
        for key in [
            String::new(),
            "SECRET\r\nInjected: yes".into(),
            "has space".into(),
            "x".repeat(4097),
        ] {
            assert!(OpenAiCompatibleProvider::new("https://example.com/v1", key).is_err());
        }
        let provider =
            OpenAiCompatibleProvider::new("https://example.com/v1/", "test-key".into()).unwrap();
        assert_eq!(provider.endpoint, "https://example.com/v1/chat/completions");
    }

    #[test]
    fn incomplete_ambiguous_tool_and_refusal_responses_are_rejected() {
        let valid = envelope("{}");
        let mut cases = vec!["SECRET_INVALID_JSON".into(), "{}".into()];
        for (pointer, value) in [
            ("/choices/0/finish_reason", json!("length")),
            ("/choices/0/message/role", json!("user")),
            ("/choices/0/message/content", Value::Null),
            ("/choices/0/message/tool_calls", json!([])),
            ("/choices/0/message/function_call", json!({})),
            ("/choices/0/message/refusal", json!("SECRET_REFUSAL")),
            ("/choices", json!([])),
            (
                "/choices",
                json!([valid["choices"][0].clone(), valid["choices"][0].clone()]),
            ),
        ] {
            let mut response = valid.clone();
            *response
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("missing {pointer}")) = value;
            cases.push(response.to_string());
        }
        cases.push("{\"choices\":[],\"error\":\"SECRET_ERROR\"}".into());
        for body in cases {
            let error = response_content(body.as_bytes()).unwrap_err();
            assert_eq!(error, ProviderError::InvalidResponse);
            assert!(!format!("{error:?}: {error}").contains("SECRET"));
        }
    }

    #[test]
    fn response_and_content_byte_limits_are_enforced() {
        for length in [128 * 1024, 128 * 1024 + 1] {
            let mut body = envelope("{}").to_string();
            body.push_str(&" ".repeat(length - body.len()));
            let (provider, handle) = server(200, body, Duration::ZERO);
            let result = provider.complete(
                "model",
                "system",
                "input",
                Instant::now() + Duration::from_secs(2),
            );
            assert_eq!(result.is_ok(), length == 128 * 1024);
            handle.join().unwrap();
        }
        for length in [MAX_CONTENT_BYTES, MAX_CONTENT_BYTES + 1] {
            let body = envelope(&"x".repeat(length));
            assert_eq!(
                response_content(body.to_string().as_bytes()).is_ok(),
                length == MAX_CONTENT_BYTES
            );
        }
    }

    #[test]
    fn errors_redirects_and_slow_response_bodies_fail_without_retry_or_leaks() {
        for status in [307, 401, 403, 429, 500] {
            let (provider, handle) = server(status, "SECRET_ERROR_BODY".into(), Duration::ZERO);
            let error = provider
                .complete(
                    "model",
                    "system",
                    "input",
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap_err();
            assert_eq!(error, ProviderError::Http(status));
            assert!(!format!("{error:?}: {error}").contains("SECRET"));
            handle.join().unwrap();
        }
        let (provider, handle) = server(200, envelope("{}").to_string(), Duration::from_secs(1));
        let start = Instant::now();
        assert!(
            provider
                .complete(
                    "model",
                    "system",
                    "input",
                    start + Duration::from_millis(50)
                )
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(500));
        handle.join().unwrap();
    }

    #[test]
    fn connect_budget_expires_during_blocking_resolution_without_opening_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        let agent = OpenAiCompatibleProvider::agent_builder(deadline)
            .unwrap()
            .resolver(move |_: &str| {
                std::thread::sleep(Duration::from_millis(80));
                Ok(vec![address])
            })
            .build();
        assert!(agent.post(&format!("http://{address}/")).call().is_err());
        assert!(Instant::now() >= deadline);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn fake_provider_is_deterministic_bounded_and_expires_without_network() {
        let fake = FakeProvider::new("{\"goal\":\"fixture\"}".into());
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            fake.complete("model", "system", "input", deadline).unwrap(),
            fake.complete("model", "system", "input", deadline).unwrap()
        );
        assert_eq!(
            fake.complete("model", "system", "input", Instant::now())
                .unwrap_err(),
            ProviderError::DeadlineExceeded
        );
        assert_eq!(
            FakeProvider::new("x".repeat(MAX_CONTENT_BYTES + 1))
                .complete("model", "system", "input", deadline)
                .unwrap_err(),
            ProviderError::ResponseTooLarge
        );
        let cloud =
            OpenAiCompatibleProvider::new("https://example.com/v1", "test-key".into()).unwrap();
        assert_eq!(
            cloud
                .complete("model", "system", "input", Instant::now())
                .unwrap_err(),
            ProviderError::DeadlineExceeded
        );
    }
}
