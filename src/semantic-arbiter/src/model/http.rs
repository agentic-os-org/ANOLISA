//! Single-attempt HTTP adapters with bounded bodies and a shared absolute deadline.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use tokio::time::{timeout_at, Instant};

use super::{
    check_deadline, BackendIdentity, BackendKind, ModelBackend, ModelError, ModelReply,
    ModelRequest, NoulProbability, NoulQuestion,
};
use crate::config::{ModelConfig, TransportMode};
use crate::protocol::MAX_MESSAGE_BYTES;

/// Configured provider client; credentials deliberately have no Debug representation.
pub struct HttpBackend {
    client: Client,
    endpoint: String,
    authorization: HeaderValue,
    identity: BackendIdentity,
    timeout: Duration,
    max_response_bytes: usize,
}

impl HttpBackend {
    /// Read only the validated configured variable, without loading dotenv files.
    pub fn from_env(config: &ModelConfig) -> Result<Self, ModelError> {
        config.validate().map_err(|_| ModelError::InvalidResponse)?;
        let key = std::env::var(config.key_env()).map_err(|_| ModelError::MissingCredentials)?;
        Self::new(config, key)
    }

    /// Construct a client from an explicitly supplied key without logging it.
    pub fn new(config: &ModelConfig, key: String) -> Result<Self, ModelError> {
        config.validate().map_err(|_| ModelError::InvalidResponse)?;
        if key.is_empty() || key.len() > 8192 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ModelError::MissingCredentials);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| ModelError::MissingCredentials)?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            // Hyper can retry canceled pooled requests below reqwest's retry policy.
            .pool_max_idle_per_host(0)
            .https_only(config.transport == TransportMode::Https)
            .build()
            .map_err(|_| ModelError::Transport)?;
        Ok(Self {
            client,
            endpoint: config.endpoint(),
            authorization,
            identity: config.identity(),
            timeout: Duration::from_millis(config.timeout_ms),
            max_response_bytes: config.max_response_bytes,
        })
    }

    async fn attempt(
        &self,
        request: &ModelRequest,
        deadline: Instant,
    ) -> Result<ModelReply, ModelError> {
        check_deadline(deadline)?;
        if request.questions.is_empty() {
            return Err(ModelError::InvalidResponse);
        }
        let mut body = BoundedBody(Vec::new());
        let serialized = serde_json::to_writer(
            &mut body,
            &ProviderRequest {
                model: &self.identity.model,
                state: &request.state,
                questions: &request.questions,
            },
        );
        check_deadline(deadline)?;
        serialized.map_err(|_| ModelError::InvalidResponse)?;
        let outgoing = self
            .client
            .post(&self.endpoint)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body.0)
            .build();
        check_deadline(deadline)?;
        let mut response = self
            .client
            .execute(outgoing.map_err(transport_error)?)
            .await
            .map_err(transport_error)?;
        check_deadline(deadline)?;
        match response.status().as_u16() {
            200..=299 => {}
            401 | 403 => return Err(ModelError::Authentication),
            429 => return Err(ModelError::RateLimited),
            _ => return Err(ModelError::Http),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            check_deadline(deadline)?;
            if chunk.len() > self.max_response_bytes - bytes.len() {
                return Err(ModelError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        check_deadline(deadline)?;
        let parsed = parse_reply(&bytes, request, self.identity.kind);
        check_deadline(deadline)?;
        parsed
    }
}

impl ModelBackend for HttpBackend {
    fn identity(&self) -> BackendIdentity {
        self.identity.clone()
    }

    fn infer<'a>(
        &'a self,
        request: &'a ModelRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<ModelReply, ModelError>> + Send + 'a>> {
        let deadline = deadline.min(Instant::now() + self.timeout);
        Box::pin(async move {
            let result = timeout_at(deadline, self.attempt(request, deadline))
                .await
                .map_err(|_| ModelError::Timeout)?;
            check_deadline(deadline)?;
            result
        })
    }
}

fn transport_error(error: reqwest::Error) -> ModelError {
    if error.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Transport
    }
}

#[derive(Serialize)]
struct ProviderRequest<'a> {
    model: &'a str,
    state: &'a serde_json::Value,
    questions: &'a BTreeMap<String, NoulQuestion>,
}

struct BoundedBody(Vec<u8>);

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MESSAGE_BYTES - self.0.len() {
            return Err(io::Error::other("request too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderReply {
    answers: BTreeMap<String, ProviderAnswer>,
    #[serde(default, rename = "model")]
    _model: Option<IgnoredAny>,
    #[serde(default, rename = "usage")]
    _usage: Option<IgnoredAny>,
    #[serde(default, rename = "request_id")]
    _request_id: Option<IgnoredAny>,
    #[serde(default, rename = "latency_ms")]
    _latency_ms: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderAnswer {
    #[serde(default, rename = "type", deserialize_with = "present_type")]
    kind: Option<NoulType>,
    noul: Box<RawValue>,
}

#[derive(Deserialize)]
enum NoulType {
    #[serde(rename = "noul")]
    Noul,
}

fn present_type<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<NoulType>, D::Error> {
    NoulType::deserialize(deserializer).map(Some)
}

fn parse_reply(
    bytes: &[u8],
    request: &ModelRequest,
    kind: BackendKind,
) -> Result<ModelReply, ModelError> {
    let reply: ProviderReply =
        crate::wire::decode(bytes).map_err(|_| ModelError::InvalidResponse)?;
    if !reply.answers.keys().eq(request.questions.keys()) {
        return Err(ModelError::InvalidResponse);
    }
    let mut answers = BTreeMap::new();
    for (id, answer) in reply.answers {
        if kind == BackendKind::Typesafe && answer.kind.is_none() {
            return Err(ModelError::InvalidResponse);
        }
        answers.insert(id, NoulProbability::from_json_number(answer.noul.get())?);
    }
    Ok(ModelReply { answers })
}
