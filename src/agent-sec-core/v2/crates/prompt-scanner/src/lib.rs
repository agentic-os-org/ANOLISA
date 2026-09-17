//! Prompt injection / jailbreak scanner core.
//!
//! Multi-layer prompt scanning:
//!
//! - preprocessing: Unicode normalisation and obfuscation decoding
//! - L1 `rule_engine`: regex rules over the prompt and decoded variants
//! - L2 `ml_classifier`: model-backed classification (Qwen3Guard or
//!   Warden-Gen on Ollama)
//! - L4 `multi_turn_intent`: conversation-level intent classification
//!
//! Layers are selected by [`ScanMode`] presets and combined into a final
//! [`Verdict`].

/// Scanner engine version reported in the JSON output.
///
/// Tied to this crate's package version so a bump propagates to every
/// consumer instead of drifting across hand-written literals.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod config;
pub mod detectors;
pub mod error;
pub mod models;
pub mod preprocessor;
pub mod result;
pub mod rules;
pub mod scanner;
pub mod verdict;

pub use config::{ScanConfig, ScanMode};
pub use detectors::{Conversation, DetectInput, DetectionLayer};
pub use error::ScannerError;
pub use model_service::{ModelClient, OllamaClient};
pub use models::multi_turn_intent::Turn;
pub use models::qwen3_guard::MODEL_QWEN3_GUARD;
pub use models::warden_gen::MODEL_WARDEN_GEN;
pub use models::{Classifier, ClassifierResult};
pub use result::{LayerResult, ScanResult, Severity, ThreatDetail, ThreatType, Verdict};
pub use scanner::PromptScanner;
