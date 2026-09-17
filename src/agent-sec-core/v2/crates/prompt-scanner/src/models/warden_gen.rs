//! Warden-Gen classifier wrapper backed by Ollama.
//!
//! Warden-Gen speaks the same `Safety:`/`Categories:` protocol as Qwen3Guard,
//! so parsing and the chat round trip are reused from
//! [`crate::models::qwen3_guard`]; only the model tag, the category vocabulary
//! and the generation options live here.
//!
//! The checkpoint is trained over two domains (prompt, code), which shows up
//! in one behaviour a caller must expect:
//!
//! - Code and command inputs are judged `Unsafe` with `Categories: None` —
//!   concrete categories only appear for the prompt domain, so a
//!   detection without a category is normal rather than a parse failure.
//!
//! ## The `Reason:` line is intentionally not enabled
//!
//! The chat template carries a `with_reason` jinja variable (default `false`).
//! When set it appends a third `Reason: <=220 chars` line explaining the
//! verdict.  This wrapper never enables it, so `classify` always returns
//! `reason: None`.  Two facts, both verified against the live model, make that
//! the right default rather than a limitation to paper over:
//!
//! - The variable is unreachable over Ollama's `/api/chat`, which is the only
//!   endpoint used here: passing `chat_template_kwargs: {with_reason: true}`
//!   is silently ignored (no `Reason:` line appears).  Enabling it would mean
//!   rendering the template client-side and switching to `/api/generate` with
//!   `raw: true` — a transport change this backend deliberately avoids.
//! - Even when forced on that way, the reason line degrades into the same
//!   phrase-repetition loop observed on the category line (the failure mode
//!   `NUM_PREDICT` caps), running to the token budget.  A 220-char reason
//!   also does not fit that cap, so enabling reasons would require raising
//!   it.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map};

use crate::error::ScannerError;
use crate::models::qwen3_guard::{build_category_re, classify_via_chat, Qwen3GuardDialect};
use crate::models::{Classifier, ClassifierResult};
use model_service::{create_client, ModelClient, ModelOptions};

/// Ollama tag for the Warden-Gen model (prompt and code domains).
///
/// Points at the project-owned ModelScope repository, which Ollama can pull
/// directly by this path — the repository resolves an untagged name to
/// `:latest`, so no quantisation tag is needed (mirroring
/// [`MODEL_QWEN3_GUARD`](crate::models::qwen3_guard::MODEL_QWEN3_GUARD)).
pub const MODEL_WARDEN_GEN: &str = "modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF";

/// Categories declared by Warden-Gen's safety policy, lowercase.
///
/// The first nine are inherited from Qwen3Guard's policy, the rest cover the
/// agent/code domain.  Note the short `pii`: this model does not use
/// Qwen3Guard's long `personally identifiable information`, and the two
/// vocabularies must stay separate — see [`Qwen3GuardDialect`].
const WARDEN_CATEGORIES: [&str; 18] = [
    "violent",
    "non-violent illegal acts",
    "sexual content or sexual acts",
    "pii",
    "suicide & self-harm",
    "unethical acts",
    "politically sensitive topics",
    "copyright violation",
    "jailbreak",
    "data exfiltration",
    "destructive operation",
    "privilege escalation and persistence",
    "credential access and recon",
    "indirect prompt injection",
    "resource exhaustion",
    "unauthorized network access",
    "supply chain compromise",
    "obfuscation or evasion",
];

/// Cap on generated tokens.
///
/// Valid replies are 8–11 tokens, but some inputs send the `Categories:` line
/// into a phrase loop that runs to the default 256-token budget and costs ~5x
/// the latency.  32 stops the loop while leaving roughly 3x headroom over the
/// longest legitimate reply; a reply that does hit the cap is reported by the
/// truncation warning in [`classify_via_chat`].
const NUM_PREDICT: u32 = 32;

/// Alternation over [`WARDEN_CATEGORIES`], built by the shared helper so both
/// models order their vocabulary the same way.
static WARDEN_CATEGORY_RE: LazyLock<Regex> =
    LazyLock::new(|| build_category_re(&WARDEN_CATEGORIES));

/// Warden-Gen's dialect of the Qwen3Guard protocol.
static WARDEN_DIALECT: Qwen3GuardDialect = Qwen3GuardDialect {
    display_name: "Warden-Gen",
    category_re: &WARDEN_CATEGORY_RE,
};

/// Whether `model_name` is the supported Warden-Gen model
/// (case-insensitive).
///
/// Compares case-insensitively rather than lowercasing the input first: the
/// ModelScope path carries mixed case (`ANOLISA/Warden-Gen-0.6B-GGUF`), which a
/// lowercased input would never match.
pub fn is_warden_gen_model(model_name: &str) -> bool {
    MODEL_WARDEN_GEN.eq_ignore_ascii_case(model_name.trim())
}

/// Wrapper around the Warden-Gen model served by Ollama.
pub struct WardenGenClassifier {
    model_name: String,
    client: Box<dyn ModelClient>,
}

impl WardenGenClassifier {
    /// Build a classifier that talks to the environment-configured service.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::Config`] when the configured model service
    /// backend is unsupported.
    pub fn new(model_name: impl Into<String>) -> Result<Self, ScannerError> {
        Ok(WardenGenClassifier {
            model_name: model_name.into(),
            client: create_client()?,
        })
    }

    /// Build a classifier over an injected client (used by tests).
    pub fn with_client(model_name: impl Into<String>, client: Box<dyn ModelClient>) -> Self {
        WardenGenClassifier {
            model_name: model_name.into(),
            client,
        }
    }

    /// Ollama model name used by this classifier.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Whether Ollama can serve the configured Warden-Gen model.
    pub fn check_ready(&self) -> bool {
        self.client.check_model(&self.model_name)
    }

    /// Verify that Warden-Gen is already available in Ollama.
    ///
    /// The model is never downloaded automatically; operators must pull it
    /// before scanning.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::ModelLoad`] when the model is absent.
    pub fn warmup(&self) -> Result<(), ScannerError> {
        if !self.check_ready() {
            return Err(ScannerError::ModelLoad(format!(
                "Warden-Gen is not available in Ollama. Run `ollama pull {}` first.",
                self.model_name
            )));
        }
        Ok(())
    }

    /// Classify a single prompt.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::ModelInference`] when inference fails or the
    /// response carries no assistant text.
    pub fn classify(&self, text: &str) -> Result<ClassifierResult, ScannerError> {
        let mut options: ModelOptions = Map::new();
        options.insert("temperature".into(), json!(0));
        options.insert("num_predict".into(), json!(NUM_PREDICT));
        classify_via_chat(
            self.client.as_ref(),
            &WARDEN_DIALECT,
            &self.model_name,
            text,
            &options,
        )
    }
}

/// L2 classifier implementation.
///
/// Delegates to the inherent methods (which stay available for readiness
/// probing); inherent methods win name resolution, so this is plain
/// delegation, not recursion.
impl Classifier for WardenGenClassifier {
    fn model_name(&self) -> &str {
        self.model_name()
    }

    fn warmup(&self) -> Result<(), ScannerError> {
        self.warmup()
    }

    fn classify(&self, text: &str) -> Result<ClassifierResult, ScannerError> {
        self.classify(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::qwen3_guard::{Qwen3GuardClassifier, MODEL_QWEN3_GUARD};
    use model_service::GenerateRequest;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    /// Client returning a canned chat reply and recording the request options,
    /// so the generation parameters sent to Ollama can be asserted.
    struct FakeClient {
        reply: Value,
        ready: bool,
        seen_options: Arc<Mutex<Option<ModelOptions>>>,
    }

    impl FakeClient {
        fn with_content(content: &str) -> Self {
            FakeClient::with_reply(json!({"message": {"role": "assistant", "content": content}}))
        }

        fn with_reply(reply: Value) -> Self {
            FakeClient {
                reply,
                ready: true,
                seen_options: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl ModelClient for FakeClient {
        fn check_model(&self, _model: &str) -> bool {
            self.ready
        }

        fn generate(
            &self,
            _request: &GenerateRequest<'_>,
        ) -> Result<Value, model_service::ModelServiceError> {
            unreachable!("Warden-Gen uses the chat endpoint only")
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[(&str, &str)],
            options: &ModelOptions,
            _logprobs: bool,
            _top_logprobs: u32,
        ) -> Result<Value, model_service::ModelServiceError> {
            *self.seen_options.lock().expect("options lock") = Some(options.clone());
            Ok(self.reply.clone())
        }
    }

    /// Client whose chat call always fails and which serves no model.
    struct FailingClient;

    impl ModelClient for FailingClient {
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

    fn classify(content: &str) -> ClassifierResult {
        let classifier = WardenGenClassifier::with_client(
            MODEL_WARDEN_GEN,
            Box::new(FakeClient::with_content(content)),
        );
        classifier.classify("some prompt").expect("classify")
    }

    /// Options the classifier sent for one `Safety: Safe` round trip.
    fn options_sent_by(
        build: impl FnOnce(Box<dyn ModelClient>) -> Box<dyn Classifier>,
    ) -> ModelOptions {
        let client = FakeClient::with_content("Safety: Safe");
        let seen = Arc::clone(&client.seen_options);
        let classifier = build(Box::new(client));
        classifier.classify("prompt").expect("classify");
        let captured = seen.lock().expect("options lock").clone();
        captured.expect("chat was called")
    }

    #[test]
    fn model_tag_recognition_is_case_insensitive() {
        assert!(is_warden_gen_model(MODEL_WARDEN_GEN));
        assert!(is_warden_gen_model(
            "modelscope.cn/anolisa/warden-gen-0.6b-gguf"
        ));
        assert!(is_warden_gen_model(&format!("  {MODEL_WARDEN_GEN}  ")));
        assert!(!is_warden_gen_model("warden"));
        assert!(!is_warden_gen_model(MODEL_QWEN3_GUARD));
        // A neighbouring repository name must not match on a shared prefix.
        assert!(!is_warden_gen_model(&format!("{MODEL_WARDEN_GEN}-3Domain")));
        // A different ModelScope owner must not be mistaken for ours.
        assert!(!is_warden_gen_model(
            "modelscope.cn/someone-else/Warden-Gen-0.6B-GGUF"
        ));
    }

    #[test]
    fn every_declared_category_is_recognised() {
        for category in WARDEN_CATEGORIES {
            let result = classify(&format!("Safety: Unsafe\nCategories: {category}"));
            assert_eq!(result.category.as_deref(), Some(category));
            assert!(result.detected, "category {category:?}");
        }
    }

    #[test]
    fn agent_domain_category_reaches_the_label() {
        let result = classify("Safety: Unsafe\nCategories: Data Exfiltration");
        assert_eq!(result.label, "UNSAFE_DATA_EXFILTRATION");
        assert_eq!(result.category.as_deref(), Some("data exfiltration"));
        assert!(result.detected);
    }

    #[test]
    fn vocabularies_stay_disjoint_from_qwen3guard() {
        // Warden-Gen's policy declares only the short `pii`; matching the long
        // Qwen3Guard form here would mean the two vocabularies had been merged.
        let result = classify("Safety: Unsafe\nCategories: Personally Identifiable Information");
        assert_eq!(result.label, "UNSAFE");
        assert_eq!(result.category, None);
        assert!(result.detected);

        let short = classify("Safety: Unsafe\nCategories: PII");
        assert_eq!(short.label, "UNSAFE_PII");
    }

    #[test]
    fn code_domain_verdict_without_category_is_still_a_threat() {
        // Code and command inputs are judged unsafe but carry no category.
        let result = classify("Safety: Unsafe\nCategories: None");
        assert_eq!(result.label, "UNSAFE");
        assert_eq!(result.category, None);
        assert!(result.detected);
    }

    #[test]
    fn safe_verdict_is_not_detected() {
        let result = classify("Safety: Safe\nCategories: None");
        assert_eq!(result.label, "SAFE");
        assert!(!result.detected);
        assert_eq!(result.category, None);
    }

    #[test]
    fn controversial_jailbreak_is_a_threat() {
        let result = classify("Safety: Controversial\nCategories: Jailbreak");
        assert_eq!(result.label, "CONTROVERSIAL_JAILBREAK");
        assert!(result.detected);
    }

    #[test]
    fn repetition_degraded_tail_still_yields_the_first_category() {
        // Observed degradation: the category line loops the same phrase until
        // the token budget runs out.  The leading known category must survive.
        let result =
            classify("Safety: Unsafe\nCategories: Violent. 安全不可行。安全不可行。安全不");
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        assert!(result.detected);
    }

    #[test]
    fn unparseable_output_fails_open() {
        let result = classify("I would rather not answer that question in detail");
        assert_eq!(result.label, "UNKNOWN");
        assert!(!result.detected);
    }

    #[test]
    fn generation_options_cap_the_token_budget() {
        let options = options_sent_by(|client| {
            Box::new(WardenGenClassifier::with_client(MODEL_WARDEN_GEN, client))
        });
        assert_eq!(options.get("temperature"), Some(&json!(0)));
        assert_eq!(options.get("num_predict"), Some(&json!(NUM_PREDICT)));
    }

    #[test]
    fn truncated_reply_is_still_parsed() {
        // The truncation warning is a log-only signal: the verdict recovered
        // from the partial reply must stay usable.
        let classifier = WardenGenClassifier::with_client(
            MODEL_WARDEN_GEN,
            Box::new(FakeClient::with_reply(json!({
                "message": {"role": "assistant", "content": "Safety: Unsafe\nCategories: Violent"},
                "done_reason": "length"
            }))),
        );
        let result = classifier.classify("prompt").expect("classify");
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        assert!(result.detected);
    }

    #[test]
    fn empty_response_is_inference_error() {
        let classifier = WardenGenClassifier::with_client(
            MODEL_WARDEN_GEN,
            Box::new(FakeClient::with_content("   ")),
        );
        assert!(matches!(
            classifier.classify("prompt"),
            Err(ScannerError::ModelInference(_))
        ));
    }

    #[test]
    fn transport_failure_names_the_model_and_hints_the_pull() {
        let classifier =
            WardenGenClassifier::with_client(MODEL_WARDEN_GEN, Box::new(FailingClient));
        let message = classifier
            .classify("prompt")
            .expect_err("must fail")
            .to_string();
        assert!(message.contains("Warden-Gen inference failed"), "{message}");
        assert!(message.contains("ollama pull"), "{message}");
    }

    #[test]
    fn warmup_requires_model_present() {
        let ready = WardenGenClassifier::with_client(
            MODEL_WARDEN_GEN,
            Box::new(FakeClient::with_content("Safety: Safe")),
        );
        assert!(ready.warmup().is_ok());

        let missing = WardenGenClassifier::with_client(MODEL_WARDEN_GEN, Box::new(FailingClient));
        assert!(matches!(missing.warmup(), Err(ScannerError::ModelLoad(_))));
    }

    #[test]
    fn qwen3guard_keeps_its_uncapped_budget() {
        // Guards the one intentional difference between the two backends: only
        // Warden-Gen needs the `num_predict` cap, so Qwen3Guard must not
        // acquire one by accident when the shared chat path changes.
        let options = options_sent_by(|client| {
            Box::new(Qwen3GuardClassifier::with_client(MODEL_QWEN3_GUARD, client))
        });
        assert_eq!(options.get("temperature"), Some(&json!(0)));
        assert!(!options.contains_key("num_predict"), "{options:?}");
    }
}
