//! Detection layers of the scanning pipeline.
//!
//! Each layer implements [`DetectionLayer`]; the scanner runs the layers
//! listed in its config in order, honouring `fast_fail`.

pub mod ml_classifier;
pub mod multi_turn_intent;
pub mod rule_engine;

use crate::error::ScannerError;
use crate::models::multi_turn_intent::Turn;
use crate::result::LayerResult;

/// The conversation surrounding a prompt, required by layers that judge
/// intent across turns (L4).
#[derive(Debug, Clone)]
pub struct Conversation<'a> {
    /// Prior turns, oldest first.
    pub history: &'a [Turn],
    /// The assistant reply that was just generated.
    pub assistant_response: &'a str,
}

/// Everything a detection layer may inspect for one scan.
///
/// Passing an explicit struct instead of a free-form metadata map keeps
/// each layer's required inputs visible in the type system.
#[derive(Debug, Clone)]
pub struct DetectInput<'a> {
    /// Preprocessed prompt text.
    pub text: &'a str,
    /// Raw input as received by the scanner, before normalisation removed
    /// zero-width and tag characters.  Encoding-evasion rules
    /// (INJ-008 / INJ-009) can only match here.
    pub raw_text: &'a str,
    /// Decoded obfuscation variants produced by the preprocessor.
    pub decoded_variants: &'a [String],
    /// Conversation context; `None` for single-prompt scans.
    pub conversation: Option<Conversation<'a>>,
}

impl<'a> DetectInput<'a> {
    /// Input for a single-prompt scan; `text` doubles as the raw input.
    pub fn new(text: &'a str, decoded_variants: &'a [String]) -> Self {
        DetectInput {
            text,
            raw_text: text,
            decoded_variants,
            conversation: None,
        }
    }
}

/// A single detection layer (L1 rules, L2 ML, L4 multi-turn intent).
pub trait DetectionLayer: Send + Sync {
    /// Stable layer name used in results and verdict rules
    /// (e.g. "rule_engine", "ml_classifier").
    fn name(&self) -> &'static str;

    /// Whether this layer's dependencies are available.
    ///
    /// Layers backed by an external service should report reachability
    /// here so the scanner can skip or reject them up front.
    fn is_available(&self) -> bool {
        true
    }

    /// Check the layer's prerequisites before the first scan.
    ///
    /// Availability only: a layer reported ready may still pay a cold-start
    /// cost on its first scan.
    ///
    /// # Errors
    ///
    /// Returns an error when a required model is missing.
    fn warmup(&self) -> Result<(), ScannerError> {
        Ok(())
    }

    /// Scan `input` and report the outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when the layer cannot produce a verdict at all
    /// (e.g. the backing service is unreachable).  Layers that are
    /// designed to fail open return a non-detection instead.
    fn detect(&self, input: &DetectInput<'_>) -> Result<LayerResult, ScannerError>;
}
