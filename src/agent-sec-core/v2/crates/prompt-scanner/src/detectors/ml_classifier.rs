//! L2 ML classifier detector — model-backed classification.
//!
//! The concrete model is pluggable: [`MlClassifier`] holds a boxed
//! [`Classifier`] chosen by [`build_classifier`] from the configured model
//! name.  Two Ollama-served backends exist today:
//!
//! - Qwen3Guard (default): prompt-domain moderation, nine categories.
//! - Warden-Gen: adds nine categories and also covers the code domain.
//!
//! Adding another model means writing a wrapper and extending the factory —
//! nothing else changes.

use std::time::Instant;

use crate::detectors::{DetectInput, DetectionLayer};
use crate::error::ScannerError;
use crate::models::qwen3_guard::{is_qwen3_guard_model, Qwen3GuardClassifier, MODEL_QWEN3_GUARD};
use crate::models::warden_gen::{is_warden_gen_model, WardenGenClassifier, MODEL_WARDEN_GEN};
use crate::models::{Classifier, ClassifierResult};
use crate::result::{LayerResult, ThreatDetail};

/// Max characters of the prompt kept as evidence.
const MAX_EVIDENCE_CHARS: usize = 200;

/// Category recorded on a finding when the model reports a threat without a
/// native category (e.g. a verdict + reason model).
const DEFAULT_THREAT_CATEGORY: &str = "unsafe";

/// Select the classifier backend for `model_name`.
///
/// # Errors
///
/// Returns [`ScannerError::Config`] for a model name no backend claims, so a
/// typo fails fast instead of silently disabling L2.
fn build_classifier(model_name: &str) -> Result<Box<dyn Classifier>, ScannerError> {
    if is_qwen3_guard_model(model_name) {
        Ok(Box::new(Qwen3GuardClassifier::new(model_name)?))
    } else if is_warden_gen_model(model_name) {
        Ok(Box::new(WardenGenClassifier::new(model_name)?))
    } else {
        Err(ScannerError::Config(format!(
            "Unsupported L2 model: {model_name:?}. \
             Supported: {MODEL_QWEN3_GUARD}, {MODEL_WARDEN_GEN}"
        )))
    }
}

/// L2 detection layer: model-based classification.
pub struct MlClassifier {
    classifier: Box<dyn Classifier>,
}

impl MlClassifier {
    /// Build the layer for `model_name`.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::Config`] for a model name no backend claims.
    pub fn new(model_name: &str) -> Result<Self, ScannerError> {
        Ok(MlClassifier {
            classifier: build_classifier(model_name)?,
        })
    }

    /// Build the layer over an injected classifier (used by tests).
    pub fn with_classifier(classifier: Box<dyn Classifier>) -> Self {
        MlClassifier { classifier }
    }

    /// Model name backing this layer.
    pub fn model_name(&self) -> &str {
        self.classifier.model_name()
    }
}

impl DetectionLayer for MlClassifier {
    fn name(&self) -> &'static str {
        "ml_classifier"
    }

    /// Always `true`: reachability is not probed here.
    ///
    /// L2 is mandatory in standard/strict modes, so a transient service
    /// outage must not silently drop the layer at construction time.  It
    /// surfaces at scan time instead, where the scanner records it in
    /// `layers_failed` and degrades — preserving what other layers found —
    /// or propagates it as an error when no layer at all could answer.
    fn is_available(&self) -> bool {
        true
    }

    fn warmup(&self) -> Result<(), ScannerError> {
        self.classifier.warmup()
    }

    fn detect(&self, input: &DetectInput<'_>) -> Result<LayerResult, ScannerError> {
        let t0 = Instant::now();
        let result = self.classifier.classify(input.text)?;
        let latency_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // The wrapper owns the threat decision; category / reason are opaque
        // passthrough detail the scanner does not reinterpret.
        let detected = result.detected;

        let mut details: Vec<ThreatDetail> = Vec::new();
        if detected {
            details.push(ThreatDetail {
                rule_id: format!("ML-{}", result.label),
                description: finding_description(&result),
                matched_text: input.text.chars().take(MAX_EVIDENCE_CHARS).collect(),
                category: result
                    .category
                    .clone()
                    .unwrap_or_else(|| DEFAULT_THREAT_CATEGORY.to_string()),
            });
        }

        // A clean scan scores 0.0; a threat carries the backend's confidence,
        // which is absent for label-only models.
        let score = if detected {
            result.confidence
        } else {
            Some(0.0)
        };

        Ok(LayerResult {
            layer_name: self.name().to_string(),
            detected,
            score,
            details,
            latency_ms,
        })
    }
}

/// Build a human-readable ML finding description.
///
/// Prefers the model's own rationale when present (verdict + reason models);
/// otherwise synthesizes one from severity, the native category and the
/// confidence.
fn finding_description(result: &ClassifierResult) -> String {
    if let Some(reason) = result.reason.as_deref() {
        if !reason.trim().is_empty() {
            return reason.to_string();
        }
    }
    // "content" (not the finding's "unsafe" fallback) reads naturally in the
    // synthesized sentence when the model gives no category.
    let category = result.category.as_deref().unwrap_or("content");
    let confidence = result
        .confidence
        .map(|c| format!(" (confidence {:.2}%)", c * 100.0))
        .unwrap_or_default();
    if result.label.starts_with("CONTROVERSIAL") {
        format!("ML classifier reported controversial {category}{confidence}")
    } else if result.label.starts_with("UNSAFE") {
        format!("ML classifier reported unsafe {category}{confidence}")
    } else {
        format!("ML classifier detected {category}{confidence}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::qwen3_guard::Qwen3GuardClassifier;
    use model_service::{GenerateRequest, ModelClient, ModelOptions};
    use serde_json::{json, Value};

    struct FakeClient {
        content: String,
        ready: bool,
    }

    impl ModelClient for FakeClient {
        fn check_model(&self, _model: &str) -> bool {
            self.ready
        }

        fn generate(
            &self,
            _request: &GenerateRequest<'_>,
        ) -> Result<Value, model_service::ModelServiceError> {
            unreachable!("L2 uses the chat endpoint only")
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[(&str, &str)],
            _options: &ModelOptions,
            _logprobs: bool,
            _top_logprobs: u32,
        ) -> Result<Value, model_service::ModelServiceError> {
            Ok(json!({"message": {"role": "assistant", "content": self.content}}))
        }
    }

    struct DownClient;

    impl ModelClient for DownClient {
        fn check_model(&self, _model: &str) -> bool {
            false
        }

        fn generate(
            &self,
            _request: &GenerateRequest<'_>,
        ) -> Result<Value, model_service::ModelServiceError> {
            unreachable!()
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[(&str, &str)],
            _options: &ModelOptions,
            _logprobs: bool,
            _top_logprobs: u32,
        ) -> Result<Value, model_service::ModelServiceError> {
            Err(model_service::ModelServiceError::Inference(
                "connection refused".into(),
            ))
        }
    }

    /// A minimal verdict + reason backend: no native category, a free-text
    /// rationale. Stands in for a future model with that output shape.
    struct FakeVerdictReasonClassifier {
        detected: bool,
        reason: String,
    }

    impl Classifier for FakeVerdictReasonClassifier {
        fn model_name(&self) -> &str {
            "fake-verdict-reason"
        }

        fn warmup(&self) -> Result<(), ScannerError> {
            Ok(())
        }

        fn classify(&self, _text: &str) -> Result<ClassifierResult, ScannerError> {
            Ok(ClassifierResult {
                label: "DENY".to_string(),
                detected: self.detected,
                confidence: None,
                category: None,
                reason: Some(self.reason.clone()),
                probabilities: Default::default(),
            })
        }
    }

    fn layer_with(content: &str) -> MlClassifier {
        MlClassifier::with_classifier(Box::new(Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient {
                content: content.to_string(),
                ready: true,
            }),
        )))
    }

    fn detect(layer: &MlClassifier, text: &str) -> Result<LayerResult, ScannerError> {
        let variants: Vec<String> = Vec::new();
        layer.detect(&DetectInput::new(text, &variants))
    }

    #[test]
    fn unsupported_model_is_rejected_at_construction() {
        let Err(ScannerError::Config(message)) =
            MlClassifier::new("LLM-Research/Llama-Prompt-Guard-2-86M")
        else {
            panic!("unknown model must fail fast with a config error");
        };
        // The message must list every selectable backend, otherwise a typo
        // gives no hint about what is available.
        assert!(message.contains(MODEL_QWEN3_GUARD), "{message}");
        assert!(message.contains(MODEL_WARDEN_GEN), "{message}");
    }

    #[test]
    fn both_backends_are_selectable_by_model_name() {
        for model in [MODEL_QWEN3_GUARD, MODEL_WARDEN_GEN] {
            let Ok(layer) = MlClassifier::new(model) else {
                panic!("backend {model} must be selectable");
            };
            assert_eq!(layer.model_name(), model);
        }
    }

    #[test]
    fn safe_verdict_is_a_clean_layer_result() {
        let lr = detect(&layer_with("Safety: Safe"), "hello").unwrap();
        assert!(!lr.detected);
        assert_eq!(lr.score, Some(0.0));
        assert!(lr.details.is_empty());
        assert_eq!(lr.layer_name, "ml_classifier");
    }

    #[test]
    fn unsafe_verdict_passes_native_category_through() {
        let lr = detect(
            &layer_with("Safety: Unsafe\nCategories: Violent"),
            "burn it down",
        )
        .unwrap();
        assert!(lr.detected);
        // Qwen3Guard emits no confidence, so the score stays absent.
        assert_eq!(lr.score, None);
        let detail = &lr.details[0];
        assert_eq!(detail.rule_id, "ML-UNSAFE_VIOLENT");
        // The native category is passed through, not collapsed to jailbreak.
        assert_eq!(detail.category, "violent");
        assert_eq!(detail.matched_text, "burn it down");
        assert_eq!(detail.description, "ML classifier reported unsafe violent");
    }

    #[test]
    fn controversial_verdict_uses_its_own_wording() {
        let lr = detect(&layer_with("Safety: Controversial"), "spicy take").unwrap();
        assert!(lr.detected);
        // No category line → finding falls back to the generic marker, and the
        // description uses the neutral "content" phrasing.
        assert_eq!(lr.details[0].category, "unsafe");
        assert_eq!(
            lr.details[0].description,
            "ML classifier reported controversial content"
        );
    }

    #[test]
    fn verdict_reason_model_uses_reason_and_default_category() {
        // Exercises the trait boundary with a non-Qwen3Guard backend shape.
        let layer = MlClassifier::with_classifier(Box::new(FakeVerdictReasonClassifier {
            detected: true,
            reason: "Refusing: request describes weapon construction".to_string(),
        }));
        let lr = detect(&layer, "make a bomb").unwrap();
        assert!(lr.detected);
        let detail = &lr.details[0];
        assert_eq!(detail.rule_id, "ML-DENY");
        // No native category → generic fallback marker on the finding.
        assert_eq!(detail.category, "unsafe");
        // The model's own rationale becomes the finding message verbatim.
        assert_eq!(
            detail.description,
            "Refusing: request describes weapon construction"
        );
    }

    #[test]
    fn verdict_reason_model_safe_is_a_clean_result() {
        let layer = MlClassifier::with_classifier(Box::new(FakeVerdictReasonClassifier {
            detected: false,
            reason: "allowed".to_string(),
        }));
        let lr = detect(&layer, "hello").unwrap();
        assert!(!lr.detected);
        assert_eq!(lr.score, Some(0.0));
        assert!(lr.details.is_empty());
    }

    #[test]
    fn unknown_output_fails_open() {
        let lr = detect(
            &layer_with("I am not sure what to make of this particular request"),
            "hmm",
        )
        .unwrap();
        assert!(!lr.detected, "unparseable output must not block");
        assert_eq!(lr.score, Some(0.0));
    }

    #[test]
    fn evidence_is_clamped_to_200_chars() {
        let long = "a".repeat(500);
        let lr = detect(&layer_with("Safety: Unsafe"), &long).unwrap();
        assert_eq!(lr.details[0].matched_text.chars().count(), 200);
    }

    #[test]
    fn service_outage_surfaces_as_inference_error() {
        let layer = MlClassifier::with_classifier(Box::new(Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(DownClient),
        )));
        // L2 is mandatory: the error must propagate, not degrade silently.
        assert!(matches!(
            detect(&layer, "hello"),
            Err(ScannerError::ModelInference(_))
        ));
        assert!(layer.is_available(), "availability is not probed per scan");
    }

    #[test]
    fn warmup_reports_missing_model() {
        let layer = MlClassifier::with_classifier(Box::new(Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(DownClient),
        )));
        assert!(matches!(layer.warmup(), Err(ScannerError::ModelLoad(_))));
    }

    #[test]
    fn description_includes_confidence_when_backend_provides_one() {
        // Guards the formatting path for a score-bearing backend.
        let result = ClassifierResult {
            label: "UNSAFE_VIOLENT".to_string(),
            detected: true,
            confidence: Some(0.8734),
            category: Some("violent".to_string()),
            reason: None,
            probabilities: Default::default(),
        };
        assert_eq!(
            finding_description(&result),
            "ML classifier reported unsafe violent (confidence 87.34%)"
        );
    }
}
