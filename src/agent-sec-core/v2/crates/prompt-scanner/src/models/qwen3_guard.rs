//! Qwen3Guard classifier wrapper backed by Ollama.
//!
//! Qwen3Guard is served by Ollama rather than loaded in-process.  The Gen
//! variant returns a structured moderation result with a three-tier
//! severity label and optional safety categories:
//!
//! ```text
//! Safety: Unsafe
//! Categories: Violent
//! ```
//!
//! Models derived from Qwen3Guard speak the same protocol, so the parsing and
//! chat plumbing here take a crate-internal `Qwen3GuardDialect` instead of
//! hard-coding Qwen3Guard's vocabulary — see [`crate::models::warden_gen`].

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::error::ScannerError;
use crate::models::{Classifier, ClassifierResult};
use model_service::{create_client, ModelClient, ModelOptions};

/// Ollama tag for the guard model.
///
/// Points at the project-owned ModelScope repository, which Ollama can pull
/// directly by this path — no local renaming step is required.
pub const MODEL_QWEN3_GUARD: &str = "modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF";

/// Label emitted when the model output cannot be parsed into a known
/// safety verdict.  Consumers fail open on it — see [`is_unknown_label`].
const LABEL_UNKNOWN: &str = "UNKNOWN";

/// Official Qwen3Guard category names, lowercase.
///
/// Only these are accepted; anything else is logged and dropped so a
/// drifting model cannot inject arbitrary strings into result labels.
const KNOWN_CATEGORIES: [&str; 10] = [
    "violent",
    "non-violent illegal acts",
    "sexual content or sexual acts",
    "personally identifiable information",
    "pii",
    "suicide & self-harm",
    "unethical acts",
    "politically sensitive topics",
    "copyright violation",
    "jailbreak",
];

/// Sentinel values that mean "no categories" and must not be logged as
/// non-standard output.
const EMPTY_CATEGORY_SENTINELS: [&str; 5] = ["none", "null", "n/a", "na", "safe"];

/// Alternation over [`KNOWN_CATEGORIES`], longest first so that
/// "personally identifiable information" wins over "pii".
static CATEGORY_RE: LazyLock<Regex> = LazyLock::new(|| build_category_re(&KNOWN_CATEGORIES));

/// Qwen3Guard's own dialect of the protocol.
static QWEN3GUARD_DIALECT: Qwen3GuardDialect = Qwen3GuardDialect {
    display_name: "Qwen3Guard",
    category_re: &CATEGORY_RE,
};

/// The per-model half of the Qwen3Guard output protocol.
///
/// Qwen3Guard and the models derived from it (today Warden-Gen) emit the same
/// `Safety:`/`Categories:` shape, so parsing, confidence recovery and the chat
/// round trip are shared; only the accepted category vocabulary and the name
/// shown in logs differ.
///
/// Vocabularies must stay per-model and must never be merged: Warden-Gen
/// declares the short `pii` while Qwen3Guard also declares the long
/// `personally identifiable information`, so an alias a model never emits
/// would disturb the longest-first match order in [`build_category_re`].
pub(crate) struct Qwen3GuardDialect {
    /// Model name used in logs and error messages.
    pub(crate) display_name: &'static str,
    /// Alternation over this model's category vocabulary.
    pub(crate) category_re: &'static LazyLock<Regex>,
}

/// Compile a case-insensitive alternation over `names`, longest first.
///
/// Longest first so that a long category name wins over a shorter alias of
/// itself (`personally identifiable information` over `pii`).
pub(crate) fn build_category_re(names: &[&str]) -> Regex {
    if names.is_empty() {
        // An empty alternation matches the empty string at every position,
        // which would push blank categories into the parsed result.
        return Regex::new(r"[^\s\S]").expect("never-matching class is valid");
    }
    let mut names: Vec<&str> = names.to_vec();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let alternation = names
        .iter()
        .map(|name| regex::escape(name))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!("(?i){alternation}")).expect("category alternation is valid")
}

/// Runs of characters that are not alphanumeric, collapsed to `_` when
/// building a label identifier.
static NON_WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[^a-z0-9]+").expect("static regex is valid"));

/// Whether `model_name` is the supported Qwen3Guard model
/// (case-insensitive).
///
/// Compares case-insensitively rather than lowercasing the input first: the
/// ModelScope path carries mixed case (`ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`),
/// which a lowercased input would never match.
pub fn is_qwen3_guard_model(model_name: &str) -> bool {
    MODEL_QWEN3_GUARD.eq_ignore_ascii_case(model_name.trim())
}

/// Whether `label` marks an unparseable Qwen3Guard response.
///
/// [`Qwen3GuardClassifier`] emits it when the output cannot be parsed
/// into Safe/Controversial/Unsafe.  It is not positive evidence of a
/// threat, so callers fail open on it.
pub fn is_unknown_label(label: &str) -> bool {
    label == LABEL_UNKNOWN
}

/// Wrapper around the Qwen3Guard model served by Ollama.
pub struct Qwen3GuardClassifier {
    model_name: String,
    client: Box<dyn ModelClient>,
}

impl Qwen3GuardClassifier {
    /// Build a classifier that talks to the environment-configured
    /// service.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::Config`] when the configured model service
    /// backend is unsupported.
    pub fn new(model_name: impl Into<String>) -> Result<Self, ScannerError> {
        Ok(Qwen3GuardClassifier {
            model_name: model_name.into(),
            client: create_client()?,
        })
    }

    /// Build a classifier over an injected client (used by tests).
    pub fn with_client(model_name: impl Into<String>, client: Box<dyn ModelClient>) -> Self {
        Qwen3GuardClassifier {
            model_name: model_name.into(),
            client,
        }
    }

    /// Ollama model name used by this classifier.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Whether Ollama can serve the configured Qwen3Guard model.
    pub fn check_ready(&self) -> bool {
        self.client.check_model(&self.model_name)
    }

    /// Verify that Qwen3Guard is already available in Ollama.
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
                "Qwen3Guard is not available in Ollama. Run `ollama pull {}` first.",
                self.model_name
            )));
        }
        Ok(())
    }

    /// Classify a single prompt.
    ///
    /// # Errors
    ///
    /// Returns [`ScannerError::ModelInference`] when inference fails or
    /// the response carries no assistant text.
    pub fn classify(&self, text: &str) -> Result<ClassifierResult, ScannerError> {
        let mut options: ModelOptions = Map::new();
        options.insert("temperature".into(), json!(0));
        // No `num_predict` cap: Qwen3Guard has not shown the runaway
        // repetition that made Warden-Gen need one.
        classify_via_chat(
            self.client.as_ref(),
            &QWEN3GUARD_DIALECT,
            &self.model_name,
            text,
            &options,
        )
    }

    /// Classify prompts sequentially; each text triggers its own request.
    ///
    /// # Errors
    ///
    /// Propagates the first failure from [`classify`](Self::classify).
    pub fn classify_batch(&self, texts: &[String]) -> Result<Vec<ClassifierResult>, ScannerError> {
        texts.iter().map(|text| self.classify(text)).collect()
    }
}

/// L2 classifier implementation.
///
/// Delegates to the inherent methods (which stay available for batch
/// classification and readiness probing); inherent methods win name
/// resolution, so this is plain delegation, not recursion.
impl Classifier for Qwen3GuardClassifier {
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

/// Run one classification over the chat endpoint and parse the reply.
///
/// Shared by every model of this family: the request shape, the empty-response
/// check and the truncation warning are protocol-level concerns, so a new
/// backend cannot forget one of them.
///
/// # Errors
///
/// Returns [`ScannerError::ModelInference`] when inference fails or the
/// response carries no assistant text.
pub(crate) fn classify_via_chat(
    client: &dyn ModelClient,
    dialect: &Qwen3GuardDialect,
    model_name: &str,
    text: &str,
    options: &ModelOptions,
) -> Result<ClassifierResult, ScannerError> {
    let model = dialect.display_name;
    let body = client
        .chat(
            model_name,
            &[("user", text)],
            options,
            // Request per-token logprobs so the chosen label's confidence
            // can be recovered at the `Safety:` token position.  Old
            // Ollama versions silently omit the field; that degrades to
            // `None`, matching the original label-only behaviour.
            true,
            // Cover Safe / Controversial / Unsafe plus two spillover
            // tokens.
            5,
        )
        .map_err(|err| {
            ScannerError::ModelInference(format!(
                "{model} inference failed ({err}). Ensure Ollama is reachable \
                 and run `ollama pull {model_name}` before scanning."
            ))
        })?;
    let raw_text = extract_response_text(&body);
    if raw_text.is_empty() {
        // An empty response indicates a service error, not a valid
        // classification.
        return Err(ScannerError::ModelInference(format!(
            "{model} returned empty response for model={model_name}. \
             Check Ollama service status."
        )));
    }
    // A reply cut off by the token budget can lose part of the `Categories:`
    // line, so the truncation must stay visible instead of being classified on
    // silently.
    if body.get("done_reason").and_then(Value::as_str) == Some("length") {
        log::warn!("{model} response hit the token limit: {raw_text:?}");
    }
    let logprobs = body
        .get("logprobs")
        .and_then(Value::as_array)
        .map(|v| v.as_slice());
    Ok(response_to_result(&raw_text, logprobs, dialect))
}

/// Extract assistant text from an Ollama chat response.
///
/// A present-but-malformed `message` object yields an empty string rather
/// than falling through to `response`, so a structurally broken chat
/// reply is reported as a service error instead of being misread.
fn extract_response_text(body: &Value) -> String {
    if let Some(message) = body.get("message") {
        if message.is_object() {
            return message
                .get("content")
                .and_then(Value::as_str)
                .map(|content| content.trim().to_string())
                .unwrap_or_default();
        }
    }
    body.get("response")
        .and_then(Value::as_str)
        .map(|response| response.trim().to_string())
        .unwrap_or_default()
}

/// Convert Qwen3Guard-family text output into a [`ClassifierResult`].
///
/// `logprobs` carries Ollama's per-token log probabilities (when available);
/// the confidence is recovered from the label token position so the result
/// reflects how certain the model was when it chose Safe / Controversial /
/// Unsafe.  Missing logprobs (old Ollama or unparseable output) yields
/// `confidence: None`, matching the original label-only behaviour.
/// `probabilities` stays a one-hot representation for interface compatibility.
fn response_to_result(
    raw_text: &str,
    logprobs: Option<&[Value]>,
    dialect: &Qwen3GuardDialect,
) -> ClassifierResult {
    let parsed = parse_guard_response(raw_text);
    let safety = normalize_safety(parsed.get("safety").map(String::as_str).unwrap_or(""));
    let categories = parse_categories(
        parsed.get("categories").map(String::as_str).unwrap_or(""),
        dialect,
    );

    // The wrapper owns the threat decision: Controversial/Unsafe are threats,
    // Safe is benign, and unparseable output fails open (detected = false).
    // The model-native category is passed through untranslated.
    let (label, detected, category) = match safety.as_str() {
        "safe" => ("SAFE".to_string(), false, None),
        "controversial" => (
            build_label("CONTROVERSIAL", &categories),
            true,
            categories.first().cloned(),
        ),
        "unsafe" => (
            build_label("UNSAFE", &categories),
            true,
            categories.first().cloned(),
        ),
        _ => {
            // Unparseable output — surfaced as UNKNOWN and logged.  Not
            // positive evidence of a threat, so nothing is blocked.
            log::warn!("Unparseable {} output: {raw_text:?}", dialect.display_name);
            (LABEL_UNKNOWN.to_string(), false, None)
        }
    };

    let confidence = logprobs.and_then(|lp| extract_label_confidence(lp, &safety));

    let mut probabilities = BTreeMap::new();
    probabilities.insert(label.clone(), 1.0);
    ClassifierResult {
        label,
        detected,
        confidence,
        category,
        // This family emits structured categories, not a free-text rationale.
        reason: None,
        probabilities,
    }
}

/// Recover the model's confidence in the chosen safety label from Ollama's
/// per-token logprobs.
///
/// Qwen3Guard emits `Safety: <label>\nCategories: ...`; the label is decided
/// at the first token after `Safety` + `:`.  That token's `top_logprobs`
/// lists the candidate labels (Safe / Controversial / Unsafe) with their
/// log-probabilities — `exp` + normalise across the matched candidates yields
/// a comparable confidence.  Returns `None` when the structure cannot be
/// located (old Ollama without logprobs, unparseable output).
fn extract_label_confidence(logprobs: &[Value], safety: &str) -> Option<f64> {
    let label_idx = find_label_token_index(logprobs)?;
    let top = logprobs.get(label_idx)?.get("top_logprobs")?.as_array()?;
    let probs = collect_label_probabilities(top);
    probs.get(safety).copied()
}

/// Locate the index in `logprobs` of the first label token — the token
/// immediately after `Safety` + `:`.
///
/// A malformed entry (missing or non-string `token`) is skipped rather than
/// aborting the scan, so one odd element cannot hide an otherwise locatable
/// label position.
fn find_label_token_index(logprobs: &[Value]) -> Option<usize> {
    let token_at = |i: usize| -> Option<&str> { logprobs.get(i)?.get("token")?.as_str() };
    for i in 0..logprobs.len().saturating_sub(2) {
        let (Some(t0), Some(t1)) = (token_at(i), token_at(i + 1)) else {
            continue;
        };
        if t0.trim().eq_ignore_ascii_case("safety") && t1.trim().starts_with(':') {
            return Some(i + 2);
        }
    }
    None
}

/// The three safety labels Qwen3Guard may emit, lowercase.
const SAFETY_LABELS: [&str; 3] = ["safe", "controversial", "unsafe"];

/// Map a label token (e.g. ` Safe`, ` Cont`, ` Unsafe`) to its base safety
/// category; `None` when the token is not a label prefix.
///
/// Matches the token as a *prefix of* a label rather than the reverse, so a
/// truncated token like ` Cont` resolves to `controversial` while a longer
/// word that merely starts with a label — e.g. the ` Safety` header token —
/// is correctly rejected.
fn match_label_token(token: &str) -> Option<&'static str> {
    let t = token.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    SAFETY_LABELS
        .iter()
        .find(|label| label.starts_with(&t))
        .copied()
}

/// Collect normalised probabilities for the three safety labels from a
/// token position's `top_logprobs` array.
///
/// `top_logprobs` may carry the same label via near-duplicate surface forms;
/// the largest probability wins so the result reflects the most likely form.
/// When only one label is present there is no distribution to normalise
/// against — doing so would always report 1.0 — so the raw token probability
/// is kept instead.
fn collect_label_probabilities(top: &[Value]) -> HashMap<String, f64> {
    let mut raw: HashMap<String, f64> = HashMap::new();
    for t in top {
        let tok = t.get("token").and_then(Value::as_str).unwrap_or("");
        let lp = match t.get("logprob").and_then(Value::as_f64) {
            Some(v) => v,
            None => continue,
        };
        if let Some(label) = match_label_token(tok) {
            let p = lp.exp();
            raw.entry(label.to_string())
                .and_modify(|e| *e = e.max(p))
                .or_insert(p);
        }
    }
    if raw.len() < 2 {
        return raw;
    }
    let sum: f64 = raw.values().sum();
    if sum <= 0.0 {
        return HashMap::new();
    }
    raw.into_iter().map(|(k, v)| (k, v / sum)).collect()
}

/// Parse `Safety: ...` / `Categories: ...` lines from Qwen3Guard output.
fn parse_guard_response(raw_text: &str) -> BTreeMap<String, String> {
    let mut parsed = BTreeMap::new();
    for line in raw_text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        if key == "safety" || key == "categories" {
            parsed.insert(key, value.trim().to_string());
        }
    }
    // Fallback: only treat the whole text as a bare safety label when it
    // is short and single-line, so verbose explanations are not misread.
    let text = raw_text.trim();
    if parsed.is_empty()
        && !text.is_empty()
        && !text.contains('\n')
        && text.split_whitespace().count() <= 3
    {
        parsed.insert("safety".to_string(), text.to_string());
    }
    parsed
}

/// Normalize the three-tier safety label; unknown values yield `""`.
fn normalize_safety(raw_safety: &str) -> String {
    let safety = raw_safety.trim().to_lowercase();
    match safety.as_str() {
        "safe" | "unsafe" | "controversial" => safety,
        _ => String::new(),
    }
}

/// Extract known categories as normalized lowercase strings.
///
/// Matches against the dialect's category alternation, lowercases, and
/// deduplicates while preserving first-seen order.
fn parse_categories(raw_categories: &str, dialect: &Qwen3GuardDialect) -> Vec<String> {
    if raw_categories.is_empty() {
        return Vec::new();
    }
    let mut categories: Vec<String> = Vec::new();
    for m in dialect.category_re.find_iter(raw_categories) {
        let category = m.as_str().to_lowercase();
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    if !categories.is_empty() {
        return categories;
    }
    let stripped = raw_categories.trim().to_lowercase();
    if !stripped.is_empty() && !EMPTY_CATEGORY_SENTINELS.contains(&stripped.as_str()) {
        log::warn!(
            "{} returned non-standard categories: {raw_categories:?}",
            dialect.display_name
        );
    }
    Vec::new()
}

/// Build a stable raw label that preserves severity and, when known, the
/// leading category (e.g. `UNSAFE_VIOLENT`).
fn build_label(severity: &str, categories: &[String]) -> String {
    match categories.first() {
        Some(category) => format!("{severity}_{}", normalize_label(category)),
        None => severity.to_string(),
    }
}

/// Normalize a model category into an uppercase identifier.
fn normalize_label(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let label = NON_WORD_RE.replace_all(&lowered, "_");
    let label = label.trim_matches('_');
    if label.is_empty() {
        "UNCLASSIFIED_VIOLATION".to_string()
    } else {
        label.to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_service::GenerateRequest;

    /// Client returning a canned chat reply.
    struct FakeClient {
        reply: Value,
        ready: bool,
    }

    impl FakeClient {
        fn with_content(content: &str) -> Self {
            FakeClient {
                reply: json!({"message": {"role": "assistant", "content": content}}),
                ready: true,
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
            unreachable!("Qwen3Guard uses the chat endpoint only")
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[(&str, &str)],
            _options: &ModelOptions,
            _logprobs: bool,
            _top_logprobs: u32,
        ) -> Result<Value, model_service::ModelServiceError> {
            Ok(self.reply.clone())
        }
    }

    /// Client whose chat call always fails.
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
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient::with_content(content)),
        );
        classifier.classify("some prompt").expect("classify")
    }

    #[test]
    fn model_tag_recognition_is_case_insensitive() {
        // The ModelScope path is the only accepted repository; its mixed case
        // must survive matching.
        assert!(is_qwen3_guard_model(MODEL_QWEN3_GUARD));
        assert!(is_qwen3_guard_model(
            "modelscope.cn/anolisa/qwen3guard-gen-0.6b-gguf"
        ));
        assert!(is_qwen3_guard_model(
            "  modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF  "
        ));
        assert!(!is_qwen3_guard_model("qwen3guard:0.6b"));
        assert!(!is_qwen3_guard_model(
            "LLM-Research/Llama-Prompt-Guard-2-86M"
        ));
        // A different ModelScope owner must not be mistaken for ours.
        assert!(!is_qwen3_guard_model(
            "modelscope.cn/someone-else/Qwen3Guard-Gen-0.6B-GGUF"
        ));
    }

    #[test]
    fn safe_verdict_is_not_detected() {
        let result = classify("Safety: Safe");
        assert_eq!(result.label, "SAFE");
        assert!(!result.detected);
        assert_eq!(result.category, None);
        assert_eq!(result.confidence, None);
        assert_eq!(result.probabilities.get("SAFE"), Some(&1.0));
    }

    #[test]
    fn unsafe_verdict_carries_category_in_label() {
        let result = classify("Safety: Unsafe\nCategories: Violent");
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        // The model-native category is passed through untranslated.
        assert_eq!(result.category.as_deref(), Some("violent"));
        assert!(result.detected);
    }

    #[test]
    fn controversial_verdict_is_a_threat() {
        let result = classify("Safety: Controversial\nCategories: Politically Sensitive Topics");
        assert_eq!(result.label, "CONTROVERSIAL_POLITICALLY_SENSITIVE_TOPICS");
        assert_eq!(
            result.category.as_deref(),
            Some("politically sensitive topics")
        );
        assert!(result.detected);
    }

    #[test]
    fn unsafe_without_categories_keeps_bare_severity() {
        let result = classify("Safety: Unsafe\nCategories: None");
        assert_eq!(result.label, "UNSAFE");
        assert_eq!(result.category, None);
        assert!(result.detected);
    }

    #[test]
    fn longest_category_name_wins_over_pii_alias() {
        let result = classify("Safety: Unsafe\nCategories: Personally Identifiable Information");
        assert_eq!(result.label, "UNSAFE_PERSONALLY_IDENTIFIABLE_INFORMATION");
    }

    #[test]
    fn jailbreak_category_is_recognised() {
        let result = classify("Safety: Unsafe\nCategories: Violent, Jailbreak");
        // First-seen category leads the label; both are known so no warning.
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        assert_eq!(result.category.as_deref(), Some("violent"));
        assert!(result.detected);
    }

    #[test]
    fn bare_short_single_line_is_treated_as_safety_label() {
        let result = classify("Unsafe");
        assert_eq!(result.label, "UNSAFE");
    }

    #[test]
    fn verbose_prose_is_not_mistaken_for_a_label() {
        let result = classify(
            "I think this prompt is probably unsafe because it asks for secrets repeatedly",
        );
        assert_eq!(result.label, LABEL_UNKNOWN);
        // UNKNOWN fails open: never treated as evidence of a threat.
        assert!(!result.detected);
    }

    #[test]
    fn unknown_safety_value_is_unknown_label() {
        let result = classify("Safety: Perhaps\nCategories: Violent");
        assert_eq!(result.label, LABEL_UNKNOWN);
        assert!(!result.detected);
    }

    #[test]
    fn empty_response_is_inference_error() {
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient::with_content("   ")),
        );
        assert!(matches!(
            classifier.classify("prompt"),
            Err(ScannerError::ModelInference(_))
        ));
    }

    #[test]
    fn transport_failure_is_inference_error_with_pull_hint() {
        let classifier =
            Qwen3GuardClassifier::with_client(MODEL_QWEN3_GUARD, Box::new(FailingClient));
        let err = classifier.classify("prompt").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("Qwen3Guard inference failed"), "{message}");
        assert!(message.contains("ollama pull"), "{message}");
    }

    #[test]
    fn warmup_requires_model_present() {
        let ready = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient::with_content("Safety: Safe")),
        );
        assert!(ready.warmup().is_ok());

        let missing = Qwen3GuardClassifier::with_client(MODEL_QWEN3_GUARD, Box::new(FailingClient));
        assert!(matches!(missing.warmup(), Err(ScannerError::ModelLoad(_))));
    }

    #[test]
    fn generate_style_response_field_is_also_accepted() {
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient {
                reply: json!({"response": "Safety: Unsafe"}),
                ready: true,
            }),
        );
        let result = classifier.classify("prompt").unwrap();
        assert_eq!(result.label, "UNSAFE");
    }

    #[test]
    fn malformed_message_object_does_not_fall_back_to_response() {
        // A structurally broken chat reply must surface as a service
        // error rather than silently reading the legacy field.
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient {
                reply: json!({"message": {"role": "assistant"}, "response": "Safety: Unsafe"}),
                ready: true,
            }),
        );
        assert!(matches!(
            classifier.classify("prompt"),
            Err(ScannerError::ModelInference(_))
        ));
    }

    #[test]
    fn batch_classification_preserves_order() {
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient::with_content("Safety: Safe")),
        );
        let texts = vec!["a".to_string(), "b".to_string()];
        let results = classifier.classify_batch(&texts).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.label == "SAFE"));
    }

    #[test]
    fn category_sentinels_are_not_logged_as_categories() {
        for sentinel in ["None", "null", "N/A", "safe", ""] {
            let categories = parse_categories(sentinel, &QWEN3GUARD_DIALECT);
            assert!(categories.is_empty(), "sentinel {sentinel:?}");
        }
    }

    #[test]
    fn an_empty_vocabulary_matches_nothing() {
        // An empty alternation would match the empty string at every position
        // and turn blanks into categories.
        assert!(!build_category_re(&[]).is_match("Violent"));
        assert!(!build_category_re(&[]).is_match(""));
    }

    #[test]
    fn normalize_label_collapses_punctuation() {
        assert_eq!(normalize_label("suicide & self-harm"), "SUICIDE_SELF_HARM");
        assert_eq!(normalize_label("  ***  "), "UNCLASSIFIED_VIOLATION");
    }

    /// Build a chat reply carrying Ollama-style per-token logprobs for the
    /// `Safety: <label>` position.  Only the label token's `top_logprobs`
    /// matter for confidence recovery; other tokens carry trivial values.
    fn reply_with_logprobs(content: &str, label_top: &[(&str, f64)]) -> Value {
        let top: Vec<Value> = label_top
            .iter()
            .map(|(tok, lp)| json!({"token": tok, "logprob": lp}))
            .collect();
        json!({
            "message": {"role": "assistant", "content": content},
            "logprobs": [
                {"token": "Safety", "logprob": 0.0, "top_logprobs": [{"token": "Safety", "logprob": 0.0}]},
                {"token": ":", "logprob": 0.0, "top_logprobs": [{"token": ":", "logprob": 0.0}]},
                {"token": label_top[0].0, "logprob": label_top[0].1, "top_logprobs": top},
                {"token": "filler", "logprob": 0.0, "top_logprobs": [{"token": "filler", "logprob": 0.0}]}
            ]
        })
    }

    #[test]
    fn logprobs_recover_confidence_for_controversial() {
        // Mirrors a real Qwen3Guard + Ollama response: at the label token
        // position the model picked ` Cont` (-0.33) over ` Safe` (-1.717)
        // and ` Unsafe` (-2.293).  exp + normalise yields ~0.72.
        let reply = reply_with_logprobs(
            "Safety: Controversial\nCategories: Non-violent Illegal Acts",
            &[(" Cont", -0.33), (" Safe", -1.717), (" Unsafe", -2.293)],
        );
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient { reply, ready: true }),
        );
        let result = classifier.classify("some prompt").expect("classify");
        assert_eq!(result.label, "CONTROVERSIAL_NON_VIOLENT_ILLEGAL_ACTS");
        assert_eq!(result.category.as_deref(), Some("non-violent illegal acts"));
        assert!(result.detected);

        let conf = result.confidence.expect("confidence recovered");
        let p_cont = (-0.33f64).exp();
        let p_safe = (-1.717f64).exp();
        let p_unsafe = (-2.293f64).exp();
        let expected = p_cont / (p_cont + p_safe + p_unsafe);
        assert!(
            (conf - expected).abs() < 1e-6,
            "conf={conf} expected={expected}"
        );
        assert!(conf > 0.7 && conf < 0.73, "conf={conf}");
    }

    #[test]
    fn logprobs_recover_confidence_for_unsafe() {
        // Model picked ` Unsafe` (-0.5) over ` Safe` (-2.0) and ` Cont` (-2.5).
        let reply = reply_with_logprobs(
            "Safety: Unsafe\nCategories: Violent",
            &[(" Unsafe", -0.5), (" Safe", -2.0), (" Cont", -2.5)],
        );
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient { reply, ready: true }),
        );
        let result = classifier.classify("some prompt").expect("classify");
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        let conf = result.confidence.expect("confidence recovered");
        let p_unsafe = (-0.5f64).exp();
        let p_safe = (-2.0f64).exp();
        let p_cont = (-2.5f64).exp();
        let expected = p_unsafe / (p_unsafe + p_safe + p_cont);
        assert!(
            (conf - expected).abs() < 1e-6,
            "conf={conf} expected={expected}"
        );
        assert!(conf > 0.7);
    }

    #[test]
    fn missing_logprobs_keep_confidence_none() {
        // FakeClient::with_content returns no `logprobs` field, matching old
        // Ollama or non-logprobs callers; confidence must degrade to `None`.
        let result = classify("Safety: Unsafe\nCategories: Violent");
        assert_eq!(result.label, "UNSAFE_VIOLENT");
        assert_eq!(result.confidence, None);
        assert!(result.detected);
    }

    #[test]
    fn malformed_logprobs_keep_confidence_none() {
        // logprobs present but the `Safety:` + `:` pattern is absent, so the
        // label token position cannot be located — degrade to `None`.
        let reply = json!({
            "message": {"role": "assistant", "content": "Unsafe"},
            "logprobs": [{"token": "Unsafe", "logprob": -0.1, "top_logprobs": [{"token": "Unsafe", "logprob": -0.1}]}]
        });
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient { reply, ready: true }),
        );
        let result = classifier.classify("some prompt").expect("classify");
        // Bare `Unsafe` is still parsed as the safety label (fallback path).
        assert_eq!(result.label, "UNSAFE");
        assert_eq!(result.confidence, None);
    }

    #[test]
    fn a_malformed_logprobs_entry_does_not_hide_the_label_position() {
        // The leading entry carries no string `token`; the scan must skip it
        // and still find the `Safety` + `:` pair that follows.
        let reply = json!({
            "message": {"role": "assistant", "content": "Safety: Unsafe"},
            "logprobs": [
                {"logprob": 0.0},
                {"token": "Safety", "logprob": 0.0},
                {"token": ":", "logprob": 0.0},
                {"token": " Unsafe", "logprob": -0.5, "top_logprobs": [
                    {"token": " Unsafe", "logprob": -0.5},
                    {"token": " Safe", "logprob": -2.0},
                ]}
            ]
        });
        let classifier = Qwen3GuardClassifier::with_client(
            MODEL_QWEN3_GUARD,
            Box::new(FakeClient { reply, ready: true }),
        );
        let result = classifier.classify("some prompt").expect("classify");
        assert_eq!(result.label, "UNSAFE");
        let conf = result
            .confidence
            .expect("confidence recovered past bad entry");
        let expected = (-0.5f64).exp() / ((-0.5f64).exp() + (-2.0f64).exp());
        assert!((conf - expected).abs() < 1e-6, "conf={conf}");
    }

    #[test]
    fn label_tokens_match_only_as_label_prefixes() {
        assert_eq!(match_label_token(" Safe"), Some("safe"));
        assert_eq!(match_label_token(" Cont"), Some("controversial"));
        assert_eq!(match_label_token(" Unsafe"), Some("unsafe"));
        // The `Safety` header token merely starts with "safe"; treating it as
        // the Safe label would corrupt the confidence distribution.
        assert_eq!(match_label_token("Safety"), None);
        assert_eq!(match_label_token("unsafely"), None);
        assert_eq!(match_label_token(""), None);
        assert_eq!(match_label_token("   "), None);
    }

    #[test]
    fn single_label_candidate_keeps_raw_probability() {
        // Only one label surface form present: normalising over a
        // one-element set would always yield 1.0 no matter how uncertain
        // the model actually was.  The raw token probability must survive.
        let top = vec![
            json!({"token": " Unsafe", "logprob": -1.0}),
            json!({"token": " weird", "logprob": -0.5}),
        ];
        let probs = collect_label_probabilities(&top);
        let expected = (-1.0f64).exp();
        assert!((probs["unsafe"] - expected).abs() < 1e-9, "{probs:?}");
    }
}
