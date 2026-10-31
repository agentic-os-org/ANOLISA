use async_trait::async_trait;
use serde::Deserialize;

use super::{Embedding, EmbeddingProvider};
use crate::error::{MemoryError, Result};

/// Ollama `/api/embed` endpoint provider.
/// Runs against a local Ollama server, no API key required.
pub struct OllamaEmbedding {
    client: reqwest::Client,
    base_url: String,
    model: String,
    dimensions: usize,
}

impl OllamaEmbedding {
    pub fn new(model: &str, base_url: &str) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();

        // Common Ollama embedding model dimensionalities.
        let dimensions = match model {
            "nomic-embed-text" => 768,
            "all-minilm" | "all-minilm:l6-v2" => 384,
            "all-minilm:l12-v2" => 384,
            "mxbai-embed-large" => 1024,
            "bge-m3" => 1024,
            "bge-large" => 1024,
            "snowflake-arctic-embed" | "snowflake-arctic-embed2" => 1024,
            _ => 768, // unknown model — assume 768
        };

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| MemoryError::Other(format!("Ollama client init: {e}")))?,
            base_url,
            model: model.to_string(),
            dimensions,
        })
    }
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbedding {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Embedding {
                vector: vec![0.0_f32; self.dimensions],
            });
        }

        let body = serde_json::json!({
            "model": self.model,
            "input": trimmed,
        });

        let resp = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::Other(format!("Ollama embed request: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Truncate to prevent potentially verbose error responses
            // from leaking sensitive data into logs.
            let summary: String = body.chars().take(200).collect();
            return Err(MemoryError::Other(format!(
                "Ollama embed error {status}: {summary}"
            )));
        }

        let data: OllamaEmbedResponse = resp
            .json()
            .await
            .map_err(|e| MemoryError::Other(format!("Ollama embed parse: {e}")))?;

        // An empty `embeddings` array is a provider contract violation,
        // not an embedding: falling back to a zero vector here would
        // silently persist a meaningless vector for the document (it can
        // never match anything) while the caller believes the embed
        // succeeded. Mirrors the OpenAI provider's empty-data handling.
        let vector = data.embeddings.into_iter().next().ok_or_else(|| {
            MemoryError::Other(format!(
                "Ollama embed error: model {} returned an empty embeddings array",
                self.model
            ))
        })?;

        Ok(Embedding { vector })
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::testutil::serve_once;

    #[tokio::test]
    async fn empty_embeddings_array_is_an_error_not_a_zero_vector() {
        let url = serve_once(200, r#"{"embeddings": []}"#);
        let provider = OllamaEmbedding::new("nomic-embed-text", &url).unwrap();
        let result = provider.embed("hello world").await;
        let err = result.expect_err("a 200 response carrying no embedding must surface an error");
        assert!(
            err.to_string().contains("empty embeddings array"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_real_embedding_round_trips() {
        let url = serve_once(200, r#"{"embeddings": [[0.25, 0.5]]}"#);
        let provider = OllamaEmbedding::new("nomic-embed-text", &url).unwrap();
        let embedding = provider.embed("hello world").await.unwrap();
        assert_eq!(embedding.vector, vec![0.25_f32, 0.5]);
    }
}
