//! Shared HTTP client for local model inference backends.
//!
//! Only Ollama is supported today.  Configuration is read from the same
//! environment variables the rest of the toolchain uses:
//!
//! - `AGENT_SEC_MODEL_SERVICE_BACKEND` (default `ollama`)
//! - `AGENT_SEC_MODEL_SERVICE_BASE_URL` (default `http://localhost:11434`)
//! - `AGENT_SEC_MODEL_SERVICE_TIMEOUT` seconds (default `30`, max `300`)
//!
//! The model service must be local: a non-loopback `base_url` is refused.
//! Scanned prompts can carry credentials and PII, and the URL comes from the
//! environment, so anything but loopback is treated as exfiltration.
//!
//! Consumers (prompt-scanner, future code/pii scanners) inject a
//! [`ModelClient`] so their transport stays decoupled from this crate.

use std::time::Duration;

use serde_json::{json, Map, Value};
use thiserror::Error;
use url::{Host, Url};

const ENV_BACKEND: &str = "AGENT_SEC_MODEL_SERVICE_BACKEND";
const ENV_BASE_URL: &str = "AGENT_SEC_MODEL_SERVICE_BASE_URL";
const ENV_TIMEOUT: &str = "AGENT_SEC_MODEL_SERVICE_TIMEOUT";

const DEFAULT_BACKEND: &str = "ollama";
const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Pause before the single retry of a transient failure; long enough for
/// an Ollama restart to finish binding, short enough to stay invisible
/// inside the middleware's per-scan budget.
const RETRY_BACKOFF: Duration = Duration::from_millis(200);

/// Upper bound for the configurable timeout; values beyond this would let a
/// single scan hang far longer than any caller expects.
const MAX_TIMEOUT_SECS: u64 = 300;

/// Errors raised by the model service client.
#[derive(Debug, Error)]
pub enum ModelServiceError {
    /// Invalid configuration (unsupported backend name, bad timeout).
    #[error("invalid model service configuration: {0}")]
    Config(String),

    /// The service is unreachable or returned an unusable response.
    #[error("model inference failed: {0}")]
    Inference(String),
}

/// Options forwarded to the backend's `options` field.
pub type ModelOptions = Map<String, Value>;

/// Parameters for a single-shot completion request.
#[derive(Debug, Clone)]
pub struct GenerateRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    /// Bypass the server-side chat template; the caller supplies the
    /// fully templated prompt.
    pub raw: bool,
    /// Request per-token logprobs.
    pub logprobs: bool,
    /// How many alternatives per position to return (only when
    /// `logprobs` is set).
    pub top_logprobs: u32,
    pub options: ModelOptions,
}

/// Unified interface for local model inference services.
///
/// Implemented by [`OllamaClient`]; tests inject fakes.
pub trait ModelClient: Send + Sync {
    /// Whether `model` is available in the backend.
    ///
    /// Never fails: network errors are reported as `false` so callers can
    /// treat availability as a simple predicate.
    fn check_model(&self, model: &str) -> bool;

    /// Single-shot completion (`POST /api/generate`).
    ///
    /// # Errors
    ///
    /// Returns [`ModelServiceError::Inference`] when the service is
    /// unreachable or the response body is not valid JSON.
    fn generate(&self, request: &GenerateRequest<'_>) -> Result<Value, ModelServiceError>;

    /// Chat completion with structured messages (`POST /api/chat`).
    ///
    /// `logprobs` requests per-token log probabilities in the response;
    /// `top_logprobs` limits how many candidate tokens are returned at each
    /// position (ignored when `logprobs` is false).  Requires Ollama
    /// v0.12.11+; older versions silently omit the `logprobs` field and
    /// callers must treat that as "no confidence available".
    ///
    /// # Errors
    ///
    /// Returns [`ModelServiceError::Inference`] when the service is
    /// unreachable or the response body is not valid JSON.
    fn chat(
        &self,
        model: &str,
        messages: &[(&str, &str)],
        options: &ModelOptions,
        logprobs: bool,
        top_logprobs: u32,
    ) -> Result<Value, ModelServiceError>;
}

/// Ollama REST backend.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    base_url: String,
    /// Built once and reused so repeated scans share the connection pool
    /// instead of paying a fresh TCP + TLS handshake per request.
    agent: ureq::Agent,
}

impl OllamaClient {
    /// Build a client for `base_url` with the given request timeout.
    ///
    /// A trailing slash in `base_url` is stripped so path concatenation
    /// never produces a double slash.
    ///
    /// `timeout` bounds connect, read and write alike.  Setting the connect
    /// phase explicitly matters: ureq defaults it to 30s, so leaving it
    /// unset would let a single request block for `timeout + 30s` when the
    /// host is unreachable rather than the configured budget.
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .build();
        OllamaClient { base_url, agent }
    }
}

impl ModelClient for OllamaClient {
    fn check_model(&self, model: &str) -> bool {
        let url = format!("{}/api/tags", self.base_url);
        let response = match self.agent.get(&url).call() {
            Ok(response) => response,
            Err(err) => {
                log::warn!("Ollama check_model failed (url={}): {err}", self.base_url);
                return false;
            }
        };
        let body: Value = match response.into_json() {
            Ok(body) => body,
            Err(err) => {
                log::warn!("Ollama check_model returned invalid JSON: {err}");
                return false;
            }
        };
        let names: Vec<&str> = body
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        // Match the exact name or a name:tag prefix, so "warden" matches
        // "warden:latest" but not "warden-tmp".
        let prefix = format!("{model}:");
        let found = names
            .iter()
            .any(|name| *name == model || name.starts_with(&prefix));
        if found {
            log::info!("Model '{model}' verified in Ollama.");
        } else {
            log::warn!("Ollama reachable but model '{model}' not in: {names:?}");
        }
        found
    }

    fn generate(&self, request: &GenerateRequest<'_>) -> Result<Value, ModelServiceError> {
        let mut payload = Map::new();
        payload.insert("model".into(), json!(request.model));
        payload.insert("prompt".into(), json!(request.prompt));
        payload.insert("stream".into(), json!(false));
        payload.insert("raw".into(), json!(request.raw));
        if request.logprobs {
            payload.insert("logprobs".into(), json!(true));
            payload.insert("top_logprobs".into(), json!(request.top_logprobs));
        }
        if !request.options.is_empty() {
            payload.insert("options".into(), Value::Object(request.options.clone()));
        }
        self.post("/api/generate", Value::Object(payload))
    }

    fn chat(
        &self,
        model: &str,
        messages: &[(&str, &str)],
        options: &ModelOptions,
        logprobs: bool,
        top_logprobs: u32,
    ) -> Result<Value, ModelServiceError> {
        let messages: Vec<Value> = messages
            .iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect();
        let mut payload = Map::new();
        payload.insert("model".into(), json!(model));
        payload.insert("messages".into(), Value::Array(messages));
        payload.insert("stream".into(), json!(false));
        if logprobs {
            payload.insert("logprobs".into(), json!(true));
            payload.insert("top_logprobs".into(), json!(top_logprobs));
        }
        if !options.is_empty() {
            payload.insert("options".into(), Value::Object(options.clone()));
        }
        self.post("/api/chat", Value::Object(payload))
    }
}

impl OllamaClient {
    /// POST `payload` to `path` and parse the JSON response body.
    ///
    /// Transient failures (see [`is_transient`]) are retried once after
    /// [`RETRY_BACKOFF`], since middleware callers issue a request per scan
    /// and a lone hiccup would otherwise fail the whole hook.
    fn post(&self, path: &str, payload: Value) -> Result<Value, ModelServiceError> {
        let url = format!("{}{path}", self.base_url);
        let response = match self.send(&url, &payload) {
            Err(err) if is_transient(&err) => {
                log::warn!("Ollama request failed (url={url}): {err}; retrying once");
                std::thread::sleep(RETRY_BACKOFF);
                self.send(&url, &payload)
            }
            attempt => attempt,
        }
        .map_err(|err| {
            ModelServiceError::Inference(format!("Ollama request failed (url={url}): {err}"))
        })?;
        response.into_json().map_err(|err| {
            ModelServiceError::Inference(format!("Ollama returned invalid JSON: {err}"))
        })
    }

    /// Single POST attempt, kept separate so `post` can retry it.
    // The large Err (ureq::Error embeds a Response) is consumed immediately
    // by `post`; boxing it would only obscure the transient-error check.
    #[allow(clippy::result_large_err)]
    fn send(&self, url: &str, payload: &Value) -> Result<ureq::Response, ureq::Error> {
        self.agent
            .post(url)
            .set("Content-Type", "application/json")
            .send_json(payload)
    }
}

/// Whether `err` is transient enough that one short-backoff retry can
/// realistically succeed: an HTTP 5xx or a failed connect (refused or
/// connect timeout, e.g. Ollama mid-restart).  Read timeouts map to
/// `ErrorKind::Io` and are deliberately excluded — retrying them would
/// double the caller's latency budget on slow inference instead of
/// masking a transient fault.
fn is_transient(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Status(code, _) => *code >= 500,
        ureq::Error::Transport(transport) => transport.kind() == ureq::ErrorKind::ConnectionFailed,
    }
}

/// Build a client from the environment.
///
/// A fresh client per call keeps configuration read-on-use, which suits the
/// one-process-per-scan CLI.  Requests issued through the same client still
/// share its connection pool — see [`OllamaClient::new`].
///
/// # Errors
///
/// Returns [`ModelServiceError::Config`] for an unsupported backend name or
/// a base URL without an `http://`/`https://` scheme.  An unparseable or
/// out-of-range timeout is logged and falls back to the default, matching the
/// tolerant behaviour of the surrounding tooling.
pub fn create_client() -> Result<Box<dyn ModelClient>, ModelServiceError> {
    Ok(Box::new(ollama_from_env()?))
}

/// Read the Ollama configuration from the environment.
fn ollama_from_env() -> Result<OllamaClient, ModelServiceError> {
    let backend = env_or(ENV_BACKEND, DEFAULT_BACKEND);
    if backend != DEFAULT_BACKEND {
        return Err(ModelServiceError::Config(format!(
            "Unsupported model service backend: {backend:?}"
        )));
    }
    let base_url = env_or(ENV_BASE_URL, DEFAULT_BASE_URL);
    validate_base_url(&base_url)?;
    let timeout_secs = timeout_secs_or_default(std::env::var(ENV_TIMEOUT).ok());
    Ok(OllamaClient::new(
        base_url,
        Duration::from_secs(timeout_secs),
    ))
}

/// Reject a base URL that is unparseable, whose scheme is not `http://` or
/// `https://`, or which targets a host other than loopback.
///
/// The URL comes from an environment variable, so a hijacked value
/// (compromised orchestration config, injected `.env`) would otherwise
/// exfiltrate every scanned prompt — credentials and PII included — to that
/// host.  Only a locally hosted model service is supported, so refusing here
/// turns that silent egress into a startup error.
///
/// Parsing goes through [`Url`], the same crate `ureq` resolves requests with,
/// so this check cannot disagree with the transport about which host is being
/// contacted.  A hand-rolled host scan does: in
/// `http://localhost:8080@attacker.example/` the leading label is userinfo and
/// the real destination is `attacker.example`.
///
/// # Errors
///
/// [`ModelServiceError::Config`] when the URL does not parse, the scheme is
/// neither `http://` nor `https://`, or the host is not loopback.
fn validate_base_url(base_url: &str) -> Result<(), ModelServiceError> {
    let url = Url::parse(base_url).map_err(|error| {
        ModelServiceError::Config(format!("base_url is not a valid URL {base_url:?}: {error}"))
    })?;
    // `Url::parse` accepts any scheme, so `localhost:11434` parses with scheme
    // `localhost` rather than failing.
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ModelServiceError::Config(format!(
            "base_url must use http:// or https:// scheme: {base_url:?}"
        )));
    }
    if !is_loopback_host(&url) {
        return Err(ModelServiceError::Config(format!(
            "refusing non-loopback model service base_url {base_url:?}: only a local model \
             service is supported, and scanned prompts must not leave the host"
        )));
    }
    Ok(())
}

/// Whether the parsed URL's host is `localhost` or a loopback IP.
///
/// [`Url`] normalises the many spellings of a loopback address — `127.1`,
/// `2130706433`, `0x7f.0.0.1`, a trailing dot — into the same [`Host::Ipv4`],
/// so all of them are accepted here.  That matches what the transport would
/// have resolved them to, which is the point of not enumerating hosts by hand.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        // Schemes without an authority (`file:`) never reach here, but a
        // hostless URL is not local either way.
        None => false,
    }
}

/// Parse a timeout in seconds, falling back to [`DEFAULT_TIMEOUT_SECS`] when
/// the value is missing.  A present but unusable value (unparseable, zero, or
/// above [`MAX_TIMEOUT_SECS`]) also falls back, but is logged rather than
/// silently dropped: the operator picked a scan budget deliberately, so
/// quietly serving a different one turns a typo into unexplained latency with
/// nothing to trace it to.
fn timeout_secs_or_default(raw: Option<String>) -> u64 {
    let Some(raw) = raw
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
    else {
        return DEFAULT_TIMEOUT_SECS;
    };
    match raw.parse::<u64>() {
        Ok(secs) if (1..=MAX_TIMEOUT_SECS).contains(&secs) => secs,
        _ => {
            log::warn!(
                "Ignoring {ENV_TIMEOUT}={raw:?}: expected an integer in 1..={MAX_TIMEOUT_SECS}; \
                 falling back to {DEFAULT_TIMEOUT_SECS}s"
            );
            DEFAULT_TIMEOUT_SECS
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Spawn a minimal keep-alive HTTP/1.1 server that answers `expected`
    /// GET requests, returning its port and the accepted-connection counter.
    fn spawn_counting_server(
        expected: usize,
    ) -> (u16, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_in_server = Arc::clone(&accepted);

        let handle = std::thread::spawn(move || {
            let mut served = 0;
            for stream in listener.incoming() {
                let mut stream = stream.expect("accept");
                accepted_in_server.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                while served < expected {
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                        break; // client closed the connection
                    }
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 {
                            break;
                        }
                        if header == "\r\n" || header == "\n" {
                            break;
                        }
                    }
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\n\
                              Content-Type: application/json\r\n\
                              Content-Length: 2\r\n\r\n{}",
                        )
                        .expect("write response");
                    stream.flush().ok();
                    served += 1;
                }
                if served >= expected {
                    break;
                }
            }
        });
        (port, accepted, handle)
    }

    /// Spawn a minimal HTTP/1.1 server that answers each request with the
    /// next status in `statuses` (body `{}`), returning its port and a
    /// served-request counter.  Exits once every status has been sent.
    fn spawn_scripted_server(
        statuses: &'static [u16],
    ) -> (u16, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let served = Arc::new(AtomicUsize::new(0));
        let served_in_server = Arc::clone(&served);

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.expect("accept");
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                loop {
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                        break; // client closed the connection
                    }
                    let mut content_length = 0usize;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 {
                            break;
                        }
                        if header == "\r\n" || header == "\n" {
                            break;
                        }
                        let lower = header.to_ascii_lowercase();
                        if let Some(value) = lower.strip_prefix("content-length:") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    // Consume the request body so the client never sees a
                    // reset while still writing.
                    let mut body = vec![0u8; content_length];
                    reader.read_exact(&mut body).ok();

                    let index = served_in_server.fetch_add(1, Ordering::SeqCst);
                    let status = statuses.get(index).copied().unwrap_or(200);
                    let reason = if status < 400 { "OK" } else { "Error" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\n\
                         Content-Type: application/json\r\n\
                         Content-Length: 2\r\n\r\n{{}}"
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write response");
                    stream.flush().ok();
                    if index + 1 >= statuses.len() {
                        return;
                    }
                }
            }
        });
        (port, served, handle)
    }

    /// Minimal generate request for retry tests.
    fn generate_request(prompt: &str) -> GenerateRequest<'_> {
        GenerateRequest {
            model: "warden",
            prompt,
            raw: true,
            logprobs: false,
            top_logprobs: 0,
            options: Map::new(),
        }
    }

    #[test]
    fn timeout_in_range_is_used() {
        assert_eq!(timeout_secs_or_default(Some("45".into())), 45);
        assert_eq!(timeout_secs_or_default(Some("1".into())), 1);
        assert_eq!(timeout_secs_or_default(Some(" 45 ".into())), 45);
        assert_eq!(
            timeout_secs_or_default(Some(MAX_TIMEOUT_SECS.to_string())),
            MAX_TIMEOUT_SECS
        );
    }

    #[test]
    fn timeout_out_of_range_falls_back_to_default() {
        assert_eq!(
            timeout_secs_or_default(Some("0".into())),
            DEFAULT_TIMEOUT_SECS
        );
        assert_eq!(
            timeout_secs_or_default(Some((MAX_TIMEOUT_SECS + 1).to_string())),
            DEFAULT_TIMEOUT_SECS
        );
    }

    #[test]
    fn timeout_missing_or_unparseable_falls_back_to_default() {
        for raw in [
            None,
            Some(String::new()),
            Some("   ".into()),
            Some("not-a-number".into()),
            Some("-5".into()),
            // A digit-transposing typo: 30 mistyped as 3O.
            Some("3O".into()),
        ] {
            assert_eq!(
                timeout_secs_or_default(raw.clone()),
                DEFAULT_TIMEOUT_SECS,
                "{raw:?} must fall back to the default timeout"
            );
        }
    }

    #[test]
    fn base_url_without_http_scheme_is_rejected() {
        for bad in [
            "ftp://localhost:11434",
            "file:///etc/passwd",
            "localhost:11434",
            "//attacker.example",
        ] {
            assert!(
                matches!(validate_base_url(bad), Err(ModelServiceError::Config(_))),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn loopback_base_url_is_accepted() {
        assert!(validate_base_url("http://localhost:11434").is_ok());
        assert!(validate_base_url("http://127.0.0.1:11434").is_ok());
        assert!(validate_base_url("http://[::1]:11434").is_ok());
    }

    /// Regression guard for the exfiltration path: a hijacked env var pointing
    /// at an arbitrary host must fail closed rather than ship prompts there.
    #[test]
    fn non_loopback_base_url_is_rejected() {
        for remote in [
            "https://model.internal:8443",
            "http://attacker.example:11434",
            "http://10.0.0.5:18099",
            "http://[2001:db8::1]:11434",
        ] {
            let error = validate_base_url(remote).expect_err("must be rejected");
            let ModelServiceError::Config(message) = &error else {
                panic!("{remote:?} must fail with Config, got {error:?}");
            };
            assert!(
                message.contains(remote),
                "rejection must name the offending URL so operators can fix it; got {message:?}"
            );
        }
    }

    #[test]
    fn loopback_detection_matches_local_hosts_only() {
        for local in [
            "http://localhost:11434",
            "http://127.0.0.1:11434",
            "http://127.1.2.3:11434/api",
            "http://[::1]:11434",
        ] {
            assert!(
                validate_base_url(local).is_ok(),
                "{local:?} must be accepted"
            );
        }
        for remote in [
            "http://attacker.example:11434",
            "http://10.0.0.5:11434",
            "http://[2001:db8::1]:11434",
        ] {
            assert!(
                validate_base_url(remote).is_err(),
                "{remote:?} must be refused"
            );
        }
    }

    /// Userinfo makes the authority's leading label a red herring: in
    /// `http://localhost:8080@attacker.example/` the host is `attacker.example`,
    /// which is where `ureq` sends the body.  Verified against a live listener:
    /// the request arrived with `Host: <post-@ authority>` and the fake
    /// loopback label demoted to an `Authorization: Basic` header.
    #[test]
    fn userinfo_does_not_disguise_a_remote_host() {
        for disguised in [
            "http://localhost@attacker.example:11434",
            "http://localhost:8080@attacker.example:11434",
            "http://127.0.0.1:8080@attacker.example/api",
            "http://[::1]:8080@attacker.example",
            "http://user:pass@attacker.example",
        ] {
            let error = validate_base_url(disguised).expect_err("must be refused");
            let ModelServiceError::Config(message) = &error else {
                panic!("{disguised:?} must fail with Config, got {error:?}");
            };
            assert!(
                message.contains(disguised),
                "rejection must name the offending URL; got {message:?}"
            );
        }
        // Userinfo itself is not the thing being refused: here the real host is
        // loopback, so the credential never leaves the machine.
        assert!(validate_base_url("http://user:pass@127.0.0.1:11434").is_ok());
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let client = OllamaClient::new("http://localhost:11434/", Duration::from_secs(1));
        assert_eq!(client.base_url, "http://localhost:11434");
    }

    #[test]
    fn requests_reuse_one_pooled_connection() {
        let (port, accepted, server) = spawn_counting_server(2);
        let client = OllamaClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        client.check_model("a");
        client.check_model("b");
        server.join().expect("server thread");

        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "two requests must share one pooled connection"
        );
    }

    #[test]
    fn unreachable_service_reports_model_missing() {
        // Port 1 is never a live Ollama; check_model must not propagate.
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(50));
        assert!(!client.check_model("qwen3guard:0.6b"));
    }

    #[test]
    fn unreachable_service_generate_is_inference_error() {
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(50));
        let request = GenerateRequest {
            model: "warden",
            prompt: "hi",
            raw: true,
            logprobs: true,
            top_logprobs: 10,
            options: Map::new(),
        };
        assert!(matches!(
            client.generate(&request),
            Err(ModelServiceError::Inference(_))
        ));
    }

    #[test]
    fn transient_5xx_is_retried_once_and_succeeds() {
        let (port, served, server) = spawn_scripted_server(&[500, 200]);
        let client = OllamaClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let result = client.generate(&generate_request("hi"));
        server.join().expect("server thread");

        assert!(result.is_ok(), "retry after a 500 must succeed: {result:?}");
        assert_eq!(served.load(Ordering::SeqCst), 2, "exactly one retry");
    }

    #[test]
    fn persistent_5xx_fails_after_single_retry() {
        let (port, served, server) = spawn_scripted_server(&[500, 500]);
        let client = OllamaClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let result = client.generate(&generate_request("hi"));
        server.join().expect("server thread");

        assert!(matches!(result, Err(ModelServiceError::Inference(_))));
        assert_eq!(served.load(Ordering::SeqCst), 2, "one retry, then give up");
    }

    #[test]
    fn client_error_is_not_retried() {
        let (port, served, server) = spawn_scripted_server(&[400]);
        let client = OllamaClient::new(format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let result = client.generate(&generate_request("hi"));
        server.join().expect("server thread");

        assert!(matches!(result, Err(ModelServiceError::Inference(_))));
        assert_eq!(served.load(Ordering::SeqCst), 1, "4xx must not be retried");
    }
}
