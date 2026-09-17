//! Model wrappers that turn raw inference output into classifier results.

pub mod multi_turn_intent;
pub mod qwen3_guard;
pub mod warden_gen;

use std::collections::BTreeMap;

use crate::error::ScannerError;

/// Unified result returned by any L2 classifier wrapper.
///
/// The wrapper owns the model-specific interpretation: it decides whether the
/// output is a threat (`detected`) and passes the model's native descriptors
/// through unchanged (`category`, `reason`).  The scanner keeps those as
/// display/audit detail and forces no shared taxonomy; only `detected` feeds
/// the verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierResult {
    /// Raw model label, e.g. "UNSAFE_VIOLENT", "SAFE".
    pub label: String,
    /// Whether the wrapper judged this output a threat.  The only field that
    /// influences the verdict.
    pub detected: bool,
    /// Probability of the predicted label, or `None` for models that do
    /// not expose confidence scores.
    pub confidence: Option<f64>,
    /// Model-native threat category (e.g. "violent"); `None` for models that
    /// emit only a verdict without a category (e.g. deny + reason models).
    pub category: Option<String>,
    /// Model-native free-text rationale, when the model provides one.
    pub reason: Option<String>,
    /// Full label -> probability mapping (one-hot for label-only models).
    pub probabilities: BTreeMap<String, f64>,
}

/// A model-backed classifier for the L2 detection layer.
///
/// Each backend (Qwen3Guard and Warden-Gen today; future verdict+reason
/// models) implements this trait so
/// [`MlClassifier`](crate::detectors::ml_classifier::MlClassifier)
/// stays decoupled from any single model's label schema. Adding a model means
/// writing a wrapper and registering it in the layer's factory — nothing
/// downstream changes.
pub trait Classifier: Send + Sync {
    /// Model name backing this classifier.
    fn model_name(&self) -> &str;

    /// Verify the backing model is ready before the first scan.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::ModelLoad`] when the model is unavailable.
    fn warmup(&self) -> Result<(), ScannerError>;

    /// Classify `text`, translating model output into a [`ClassifierResult`].
    ///
    /// # Errors
    ///
    /// Returns an error when inference fails (e.g. the service is unreachable).
    fn classify(&self, text: &str) -> Result<ClassifierResult, ScannerError>;
}
