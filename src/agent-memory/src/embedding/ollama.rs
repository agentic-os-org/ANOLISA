use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// Best-effort dimensionality. Seeded from a small model→dim table at
    /// construction (an estimate for unknown models) and overwritten with
    /// the true value observed on the first successful embed. Atomic
    /// storage so the shared provider can be updated from worker threads
    /// without a lock, mirroring the OpenAI provider.
    dimensions: AtomicUsize,
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
            dimensions: AtomicUsize::new(dimensions),
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
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbedding {
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

        let vector = data
            .embeddings
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0_f32; self.dimensions.load(Ordering::Relaxed)]);

        // Lock in the true dimensionality from the response so the static
        // table guess cannot mislabel unknown models (which default to
        // 768) for empty-input zero vectors and dimensions() consumers.
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

    /// Minimal one-shot HTTP server answering POSTs with a canned
    /// `/api/embed` JSON body. Uses the same seam production code uses:
    /// an injectable `base_url`.
    fn spawn_embed_server(response_body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 8192];
                let mut read = stream.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    continue;
                }
                let header_end = buf[..read]
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .unwrap_or(read);
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let content_length: usize = headers
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while read < header_end + 4 + content_length {
                    match stream.read(&mut buf[read..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => read += n,
                    }
                }
                let body = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn unknown_model_learns_dimension_from_first_response() {
        // 5-dim vectors from a model absent from the static table.
        let base_url = spawn_embed_server(r#"{"embeddings":[[0.1,0.2,0.3,0.4,0.5]]}"#);
        let provider = OllamaEmbedding::new("totally-unknown-model", &base_url).unwrap();

        // Before any response the static-table guess is reported.
        assert_eq!(provider.dimensions(), 768);

        let embedding = provider.embed("hello").await.unwrap();
        assert_eq!(embedding.vector.len(), 5);

        // The first real response must lock in the observed length, so
        // the provider stops reporting the 768 default for this model.
        assert_eq!(provider.dimensions(), 5);

        // Empty input must produce a zero vector of the OBSERVED size,
        // not of the stale startup guess.
        let empty = provider.embed("   ").await.unwrap();
        assert_eq!(empty.vector.len(), 5);
        assert!(empty.vector.iter().all(|&v| v == 0.0));
    }
}
