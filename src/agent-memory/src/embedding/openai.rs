use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::Deserialize;

use super::{Embedding, EmbeddingProvider};
use crate::error::{MemoryError, Result};

/// OpenAI `/v1/embeddings` compatible provider.
/// Works with Azure OpenAI, local LiteLLM proxies, and any other
/// service that exposes the same endpoint shape.
pub struct OpenAiEmbedding {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    /// Best-effort dimensionality. Seeded from a small model→dim table at
    /// construction (an estimate for unknown models, e.g. DashScope
    /// text-embedding-v3 is 1024, not the 1536 fallback) and overwritten
    /// with the true value observed on the first successful embed. Uses
    /// atomic storage so the shared provider can be updated from the
    /// worker thread without a lock.
    dimensions: AtomicUsize,
}

impl OpenAiEmbedding {
    pub fn new(api_key: &str, model: &str, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url
            .filter(|u| !u.is_empty())
            .unwrap_or("https://api.openai.com")
            .trim_end_matches('/')
            .to_string();

        let estimate = match model {
            "text-embedding-3-small" => 1536,
            "text-embedding-3-large" => 3072,
            "text-embedding-ada-002" => 1536,
            _ => {
                // Unknown model — 1536 is just a pre-flight estimate used
                // only for the startup log and the empty-input zero vector.
                // The first real embed response carries the true length and
                // overwrites this value.
                1536
            }
        };

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| MemoryError::Other(format!("OpenAI client init: {e}")))?,
            base_url,
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions: AtomicUsize::new(estimate),
        })
    }

    /// Record the true dimensionality observed in a response so subsequent
    /// empty-input zero vectors and the dimensions() accessor are accurate.
    fn observe_dim(&self, len: usize) {
        if len != 0 {
            self.dimensions.store(len, Ordering::Relaxed);
        }
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Embedding {
                vector: vec![0.0_f32; self.dimensions.load(Ordering::Relaxed)],
            });
        }

        let body = serde_json::json!({
            "model": self.model,
            "input": trimmed,
        });

        let resp = self
            .client
            .post(format!("{}/v1/embeddings", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::Other(format!("OpenAI embed request: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Truncate to prevent API key or verbose proxy errors from
            // leaking into logs / error messages returned to callers.
            let summary: String = body.chars().take(200).collect();
            return Err(MemoryError::Other(format!(
                "OpenAI embed error {status}: {summary}"
            )));
        }

        let data: EmbeddingResponse = resp
            .json()
            .await
            .map_err(|e| MemoryError::Other(format!("OpenAI embed parse: {e}")))?;

        // An empty `data` array is a provider contract violation, not an
        // embedding: falling back to a zero vector here would silently
        // persist a meaningless vector for the document (it can never
        // match anything) while the caller believes the embed succeeded.
        let vector = data
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| {
                MemoryError::Other(format!(
                    "OpenAI embed error: model {} returned an empty data array",
                    self.model
                ))
            })?;

        // Lock in the true dimensionality from the response so the
        // startup estimate doesn't mislabel the provider (e.g. DashScope
        // text-embedding-v3 = 1024, not the 1536 fallback).
        self.observe_dim(vector.len());

        Ok(Embedding { vector })
    }

    fn dimensions(&self) -> usize {
        self.dimensions.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::testutil::serve_once;

    #[tokio::test]
    async fn empty_data_array_is_an_error_not_a_zero_vector() {
        let url = serve_once(200, r#"{"data": []}"#);
        let provider = OpenAiEmbedding::new("k", "text-embedding-3-small", Some(&url)).unwrap();
        let result = provider.embed("hello world").await;
        let err = result.expect_err("a 200 response carrying no embedding must surface an error");
        assert!(
            err.to_string().contains("empty data array"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_real_embedding_round_trips_and_observes_dims() {
        let url = serve_once(200, r#"{"data": [{"embedding": [0.25, 0.5]}]}"#);
        let provider = OpenAiEmbedding::new("k", "text-embedding-3-small", Some(&url)).unwrap();
        let embedding = provider.embed("hello world").await.unwrap();
        assert_eq!(embedding.vector, vec![0.25_f32, 0.5]);
        assert_eq!(provider.dimensions(), 2);
    }
}
