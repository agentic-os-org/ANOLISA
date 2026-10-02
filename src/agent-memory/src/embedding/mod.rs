//! Embedding provider abstraction — allows agent-memory to generate
//! dense vector representations of text for semantic (vector) search.
//!
//! Providers are configured via `EmbeddingConfig` in the main config.
//! When `None`, only BM25 search is available.

pub mod ollama;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A single embedding vector (f32 for cosine similarity).
#[derive(Debug, Clone)]
pub struct Embedding {
    pub vector: Vec<f32>,
}

/// Backend-agnostic embedding interface.
/// Providers are `Send + Sync` so they can be shared across tokio tasks.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding for `text`. Implementations should strip
    /// leading/trailing whitespace and handle empty input gracefully
    /// (return a zero-vector of correct dimensionality).
    async fn embed(&self, text: &str) -> Result<Embedding>;

    /// Number of dimensions in produced vectors.
    fn dimensions(&self) -> usize;
}

/// Configuration for the embedding backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
#[derive(Default)]
pub enum EmbeddingConfig {
    /// No embedding — only BM25 keyword search is available.
    #[serde(rename = "none")]
    #[default]
    None,
    /// OpenAI `/v1/embeddings` compatible API.
    #[serde(rename = "openai")]
    OpenAI {
        /// API key. Read from env `OPENAI_API_KEY` when empty.
        #[serde(default)]
        api_key: String,
        /// Model name (default: `text-embedding-3-small`).
        #[serde(default = "default_openai_model")]
        model: String,
        /// Base URL override for proxies / compatible services.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Ollama `/api/embed` endpoint.
    #[serde(rename = "ollama")]
    Ollama {
        /// Model name (default: `nomic-embed-text`).
        #[serde(default = "default_ollama_model")]
        model: String,
        /// Base URL (default: `http://localhost:11434`).
        #[serde(default = "default_ollama_url")]
        base_url: String,
    },
}

fn default_openai_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_ollama_model() -> String {
    "nomic-embed-text".to_string()
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

/// Build a provider from config. Returns `Ok(None)` when `backend=none`
/// or when required credentials are absent.
pub fn build_provider(config: &EmbeddingConfig) -> Result<Option<Arc<dyn EmbeddingProvider>>> {
    match config {
        EmbeddingConfig::None => Ok(None),
        EmbeddingConfig::OpenAI {
            api_key,
            model,
            base_url,
        } => {
            let key = resolve_api_key(api_key, "OPENAI_API_KEY");
            if key.is_empty() {
                tracing::warn!(
                    "OpenAI embedding configured but OPENAI_API_KEY is empty; embedding disabled"
                );
                return Ok(None);
            }
            let provider = openai::OpenAiEmbedding::new(&key, model, base_url.as_deref())?;
            tracing::info!(
                "OpenAI embedding provider ready (model={}, dims={} estimated; resolves on first embed)",
                model,
                provider.dimensions()
            );
            Ok(Some(Arc::new(provider)))
        }
        EmbeddingConfig::Ollama { model, base_url } => {
            // Ollama doesn't need auth — just a running server.
            let provider = ollama::OllamaEmbedding::new(model, base_url)?;
            tracing::info!(
                "Ollama embedding provider ready (model={}, dims={})",
                model,
                provider.dimensions()
            );
            Ok(Some(Arc::new(provider)))
        }
    }
}

fn resolve_api_key(config_value: &str, env_name: &str) -> String {
    if !config_value.is_empty() {
        return config_value.to_string();
    }
    std::env::var(env_name).unwrap_or_default()
}
