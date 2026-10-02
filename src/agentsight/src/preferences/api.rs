//! Framework-free support layer shared by the preference API handlers.
//!
//! The Linux server (`crate::server::preferences`) and the macOS local
//! viewer (`crate::local::server::preferences`) expose the same two
//! endpoints over different app states. Everything that does not need
//! actix types lives here — source selection, the TTL response cache,
//! LLM-result merging and the Markdown export — so both handler sets stay
//! thin and behave identically.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agentsight_opt::preference::LlmPreference;
use serde::Deserialize;

use super::signals::{Preference, PreferenceCategory, PreferenceSource, PreferenceStatus};
use super::{DEFAULT_WINDOW_DAYS, EXPORT_MIN_CONFIDENCE, MAX_WINDOW_DAYS};

// ─── Source selection ────────────────────────────────────────────────────────

/// Requested data source for one preference API call (`?source=` parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreferenceSourceParam {
    /// Prefer genai events, fall back to collected trajectories.
    Auto,
    /// `genai_events.db` only (Linux eBPF pipeline).
    Genai,
    /// `trajectories.db` only (trajectory collector).
    Trajectory,
}

impl PreferenceSourceParam {
    /// Parse the raw query value; `None` means the parameter was omitted.
    ///
    /// # Errors
    /// Returns a human-readable message for unknown values so the handler
    /// can answer 400 instead of silently picking a default.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("") | Some("auto") => Ok(Self::Auto),
            Some("genai") => Ok(Self::Genai),
            Some("trajectory") => Ok(Self::Trajectory),
            Some(other) => Err(format!(
                "unknown source {other:?}; expected auto, genai or trajectory"
            )),
        }
    }

    /// Wire name, used in cache keys and response bodies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Genai => "genai",
            Self::Trajectory => "trajectory",
        }
    }
}

/// Outcome of `source=auto` selection, decided from cheap availability
/// probes before any analysis runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoResolution {
    /// Serve from genai events.
    Genai,
    /// Serve from collected trajectories.
    Trajectory,
    /// Neither store can be read — the request must fail loudly.
    Unavailable,
}

/// Pick the effective source for `source=auto`.
///
/// `genai_row_count` is `None` when the genai store cannot be read at all
/// (missing on Linux, or the platform has no eBPF pipeline). A readable but
/// empty genai window falls back to trajectories when they are available;
/// otherwise it stays on genai so the response keeps the pre-`source` shape
/// (an empty result, not an error).
pub fn resolve_auto(genai_row_count: Option<usize>, trajectory_available: bool) -> AutoResolution {
    match (genai_row_count, trajectory_available) {
        (Some(n), _) if n > 0 => AutoResolution::Genai,
        (_, true) => AutoResolution::Trajectory,
        (Some(_), false) => AutoResolution::Genai,
        (None, false) => AutoResolution::Unavailable,
    }
}

// ─── Request parsing ─────────────────────────────────────────────────────────

/// Query string accepted by both preference endpoints.
#[derive(Debug, Deserialize)]
pub struct PreferencesQuery {
    pub window_days: Option<u32>,
    pub llm: Option<bool>,
    pub source: Option<String>,
}

/// Query string accepted by the turns endpoint (`/api/preferences/turns`).
///
/// Mirrors [`PreferencesQuery`] for `window_days`/`source` but drops `llm`
/// (the turns endpoint serves raw text for the caller to reason over) and
/// adds `limit` to bound how many deduped user turns are returned.
#[derive(Debug, Deserialize)]
pub struct TurnsQuery {
    pub window_days: Option<u32>,
    pub source: Option<String>,
    pub limit: Option<usize>,
}

/// Default number of user turns returned when `limit` is omitted. Capped so a
/// single response stays manageable for an agent to reason over in one pass.
pub const DEFAULT_TURNS_LIMIT: usize = 200;

/// Hard maximum for `limit` — a misbehaving caller must not be able to pull an
/// unbounded blob of raw conversation text out of the API.
pub const MAX_TURNS_LIMIT: usize = 1000;

/// Clamp the requested window into `1..=MAX_WINDOW_DAYS`, defaulting to
/// [`DEFAULT_WINDOW_DAYS`].
pub fn clamp_window_days(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(DEFAULT_WINDOW_DAYS)
        .clamp(1, MAX_WINDOW_DAYS)
}

/// Window start in epoch nanoseconds: now − `window_days`. Clamped at 0 so
/// a misbehaving system clock cannot produce a negative bound that silently
/// degrades the query into a full-table scan of all history.
pub fn window_start_ns(window_days: u32) -> i64 {
    // Defensive conversion mirrors the collector's `now_ns()`: nanoseconds
    // overflow i64 in year 2262, and a pre-epoch clock yields 0 instead of
    // a panic or wraparound.
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    now_ns
        .saturating_sub(i64::from(window_days) * 86_400 * 1_000_000_000)
        .max(0)
}

// ─── In-process TTL cache ────────────────────────────────────────────────────

/// How long one computed response stays valid.
const CACHE_TTL: Duration = Duration::from_secs(600);

/// Cache key: (window_days, llm, requested source). Keying on the requested
/// (not the resolved) source keeps `auto` responsive to a store appearing
/// mid-TTL only via expiry, which is acceptable for a 10-minute cache.
pub type CacheKey = (u32, bool, PreferenceSourceParam);

struct CacheEntry {
    at: Instant,
    body: serde_json::Value,
}

/// Lazily initialized process-wide cache. A plain `Mutex<HashMap>` is enough:
/// the key space is tiny (window_days ∈ 1..=30 × llm ∈ {false, true} ×
/// three sources).
fn cache() -> &'static Mutex<HashMap<CacheKey, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fetch a still-valid cached response body for `key`, if any.
pub fn cache_get(key: CacheKey) -> Option<serde_json::Value> {
    let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get(&key)
        .filter(|entry| entry.at.elapsed() < CACHE_TTL)
        .map(|entry| entry.body.clone())
}

/// Store a freshly computed response body under `key`.
pub fn cache_put(key: CacheKey, body: serde_json::Value) {
    let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    // Drop stale entries in passing — the map never exceeds 180 keys anyway.
    guard.retain(|_, entry| entry.at.elapsed() < CACHE_TTL);
    guard.insert(
        key,
        CacheEntry {
            at: Instant::now(),
            body,
        },
    );
}

// ─── LLM result merging ──────────────────────────────────────────────────────

/// Confidence assigned to LLM findings — informative but not rule-verified.
pub const LLM_CONFIDENCE: f64 = 0.7;

/// Per-field character cap applied to LLM-produced key/value/rationale — a
/// misbehaving model must not be able to bloat the response body.
pub const MAX_LLM_FIELD_CHARS: usize = 128;

/// Fold validated LLM findings into the rule results. Tuples the rule layer
/// already reported keep their rule provenance (evidence beats inference);
/// duplicate findings within the LLM output itself are dropped too. All
/// free-text fields are capped at [`MAX_LLM_FIELD_CHARS`] characters.
pub fn merge_llm_preferences(prefs: &mut Vec<Preference>, findings: Vec<LlmPreference>) {
    for finding in findings {
        let Some(category) = PreferenceCategory::parse(finding.category.trim()) else {
            log::debug!(
                "Preference API: dropping LLM finding with unknown category {:?}",
                finding.category
            );
            continue;
        };
        let key = cap_llm_field(finding.key.trim());
        let value = cap_llm_field(finding.value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        if prefs
            .iter()
            .any(|p| p.category == category && p.key == key && p.value == value)
        {
            continue;
        }
        let rationale = cap_llm_field(finding.rationale.trim());
        prefs.push(Preference {
            category,
            key,
            value,
            confidence: LLM_CONFIDENCE,
            evidence_count: 1,
            source: PreferenceSource::Llm,
            status: PreferenceStatus::Active,
            sample_evidence: if rationale.is_empty() {
                Vec::new()
            } else {
                vec![rationale]
            },
        });
    }
}

/// Cap an LLM-produced field at [`MAX_LLM_FIELD_CHARS`] characters
/// (char-boundary safe; an ellipsis marks the cut).
fn cap_llm_field(text: &str) -> String {
    if text.chars().count() <= MAX_LLM_FIELD_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_LLM_FIELD_CHARS).collect();
    out.push('…');
    out
}

// ─── Markdown export ─────────────────────────────────────────────────────────

/// Render the Markdown export: category-grouped bullets, restricted to
/// confident (≥ [`EXPORT_MIN_CONFIDENCE`]) active entries.
pub fn render_markdown(prefs: &[Preference]) -> String {
    let mut out = String::from("## User Preferences\n");
    let mut any = false;
    for category in [
        PreferenceCategory::Communication,
        PreferenceCategory::Workflow,
        PreferenceCategory::Technical,
    ] {
        let entries: Vec<&Preference> = prefs
            .iter()
            .filter(|p| {
                p.category == category
                    && p.status == PreferenceStatus::Active
                    && p.confidence >= EXPORT_MIN_CONFIDENCE
            })
            .collect();
        if entries.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("\n### {}\n\n", capitalize(category.as_str())));
        for p in entries {
            out.push_str(&format!(
                "- **{}**: {} _(confidence {:.2}, {} observation{})_\n",
                escape_markdown(&p.key),
                escape_markdown(&p.value),
                p.confidence,
                p.evidence_count,
                if p.evidence_count == 1 { "" } else { "s" },
            ));
        }
    }
    if !any {
        out.push_str("\n_No high-confidence preferences detected in this window._\n");
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Minimal Markdown escaping for free-form text (keys/values include tool
/// names lifted straight from captured traffic): backslash-prefix every
/// character that could open emphasis/links/headings and break the exported
/// document structure.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pref(
        category: PreferenceCategory,
        key: &str,
        value: &str,
        confidence: f64,
        status: PreferenceStatus,
    ) -> Preference {
        Preference {
            category,
            key: key.to_string(),
            value: value.to_string(),
            confidence,
            evidence_count: 5,
            source: PreferenceSource::Rule,
            status,
            sample_evidence: Vec::new(),
        }
    }

    #[test]
    fn source_param_parses_known_values_and_rejects_unknown() {
        assert_eq!(
            PreferenceSourceParam::parse(None),
            Ok(PreferenceSourceParam::Auto)
        );
        assert_eq!(
            PreferenceSourceParam::parse(Some("")),
            Ok(PreferenceSourceParam::Auto)
        );
        assert_eq!(
            PreferenceSourceParam::parse(Some("auto")),
            Ok(PreferenceSourceParam::Auto)
        );
        assert_eq!(
            PreferenceSourceParam::parse(Some("genai")),
            Ok(PreferenceSourceParam::Genai)
        );
        assert_eq!(
            PreferenceSourceParam::parse(Some(" trajectory ")),
            Ok(PreferenceSourceParam::Trajectory)
        );
        let err = PreferenceSourceParam::parse(Some("ebpf")).unwrap_err();
        assert!(err.contains("ebpf"), "message names the bad value: {err}");
    }

    #[test]
    fn auto_prefers_populated_genai_and_falls_back_in_order() {
        // Populated genai window wins even when trajectories exist.
        assert_eq!(resolve_auto(Some(3), true), AutoResolution::Genai);
        assert_eq!(resolve_auto(Some(3), false), AutoResolution::Genai);
        // Empty or unreadable genai falls back when trajectories exist.
        assert_eq!(resolve_auto(Some(0), true), AutoResolution::Trajectory);
        assert_eq!(resolve_auto(None, true), AutoResolution::Trajectory);
        // Readable-but-empty genai without trajectories keeps the legacy
        // empty-result behavior instead of erroring.
        assert_eq!(resolve_auto(Some(0), false), AutoResolution::Genai);
        // Nothing to read at all.
        assert_eq!(resolve_auto(None, false), AutoResolution::Unavailable);
    }

    #[test]
    fn window_days_is_clamped_to_bounds() {
        assert_eq!(clamp_window_days(None), DEFAULT_WINDOW_DAYS);
        assert_eq!(clamp_window_days(Some(0)), 1);
        assert_eq!(clamp_window_days(Some(14)), 14);
        assert_eq!(clamp_window_days(Some(365)), MAX_WINDOW_DAYS);
    }

    #[test]
    fn merge_keeps_rule_entries_and_validates_llm_rows() {
        let mut prefs = vec![pref(
            PreferenceCategory::Technical,
            "testing",
            "require_tests",
            0.8,
            PreferenceStatus::Active,
        )];
        let findings = vec![
            // Duplicate of the rule entry: dropped.
            LlmPreference {
                category: "technical".to_string(),
                key: "testing".to_string(),
                value: "require_tests".to_string(),
                rationale: "asks for tests".to_string(),
            },
            // Unknown category: dropped.
            LlmPreference {
                category: "style".to_string(),
                key: "tone".to_string(),
                value: "casual".to_string(),
                rationale: String::new(),
            },
            // New finding: merged with LLM provenance.
            LlmPreference {
                category: "communication".to_string(),
                key: "verbosity".to_string(),
                value: "concise".to_string(),
                rationale: "repeatedly asks for short answers".to_string(),
            },
        ];
        merge_llm_preferences(&mut prefs, findings);
        assert_eq!(prefs.len(), 2);
        assert_eq!(prefs[0].source, PreferenceSource::Rule);
        let added = &prefs[1];
        assert_eq!(added.source, PreferenceSource::Llm);
        assert_eq!(added.confidence, LLM_CONFIDENCE);
        assert_eq!(
            added.sample_evidence,
            vec!["repeatedly asks for short answers"]
        );
    }

    #[test]
    fn markdown_export_groups_and_filters() {
        let prefs = vec![
            pref(
                PreferenceCategory::Communication,
                "language",
                "chinese",
                0.9,
                PreferenceStatus::Active,
            ),
            // Below the export threshold: hidden.
            pref(
                PreferenceCategory::Workflow,
                "workflow_style",
                "plan_first",
                0.3,
                PreferenceStatus::Active,
            ),
            // Conflicting: hidden.
            pref(
                PreferenceCategory::Communication,
                "language",
                "english",
                0.9,
                PreferenceStatus::Conflicting,
            ),
            pref(
                PreferenceCategory::Technical,
                "testing",
                "require_tests",
                0.7,
                PreferenceStatus::Active,
            ),
        ];
        let md = render_markdown(&prefs);
        assert!(md.starts_with("## User Preferences\n"));
        assert!(md.contains("### Communication"));
        assert!(md.contains("- **language**: chinese"));
        assert!(md.contains("### Technical"));
        // Snake_case values pick up the underscore escape.
        assert!(md.contains("- **testing**: require\\_tests"));
        assert!(!md.contains("plan_first"));
        assert!(!md.contains("english"));
        assert!(!md.contains("### Workflow"));
    }

    #[test]
    fn markdown_escapes_special_characters() {
        let prefs = vec![pref(
            PreferenceCategory::Technical,
            "preferred_tool",
            "evil*[tool](x) #1 `rm`!",
            0.9,
            PreferenceStatus::Active,
        )];
        let md = render_markdown(&prefs);
        assert!(md.contains("- **preferred\\_tool**: evil\\*\\[tool\\]\\(x\\) \\#1 \\`rm\\`\\!"));
        // The raw link/emphasis syntax must not survive.
        assert!(!md.contains("[tool](x)"));
    }

    #[test]
    fn markdown_export_handles_empty_window() {
        let md = render_markdown(&[]);
        assert!(md.starts_with("## User Preferences\n"));
        assert!(md.contains("_No high-confidence preferences detected"));
    }

    #[test]
    fn merge_caps_overlong_llm_fields() {
        let mut prefs = Vec::new();
        let findings = vec![LlmPreference {
            category: "technical".to_string(),
            key: "k".repeat(MAX_LLM_FIELD_CHARS * 2),
            value: "值".repeat(MAX_LLM_FIELD_CHARS * 2),
            rationale: "r".repeat(MAX_LLM_FIELD_CHARS * 2),
        }];
        merge_llm_preferences(&mut prefs, findings);
        assert_eq!(prefs.len(), 1);
        let p = &prefs[0];
        // Capped at the limit plus the ellipsis, on char boundaries.
        assert_eq!(p.key.chars().count(), MAX_LLM_FIELD_CHARS + 1);
        assert!(p.key.ends_with('…'));
        assert_eq!(p.value.chars().count(), MAX_LLM_FIELD_CHARS + 1);
        assert!(p.value.ends_with('…'));
        assert_eq!(
            p.sample_evidence[0].chars().count(),
            MAX_LLM_FIELD_CHARS + 1
        );

        // Short fields pass through untouched.
        assert_eq!(cap_llm_field("short"), "short");
    }

    #[test]
    fn llm_exclusive_value_conflicts_with_rule() {
        use super::super::aggregator;

        // Rule layer saw Chinese (5 observations); the LLM reports English.
        let mut prefs = vec![pref(
            PreferenceCategory::Communication,
            "language",
            "chinese",
            0.8,
            PreferenceStatus::Active,
        )];
        let findings = vec![LlmPreference {
            category: "communication".to_string(),
            key: "language".to_string(),
            value: "english".to_string(),
            rationale: "several turns are written in English".to_string(),
        }];
        merge_llm_preferences(&mut prefs, findings);
        aggregator::mark_conflicts(&mut prefs);
        assert_eq!(prefs.len(), 2);
        assert!(
            prefs
                .iter()
                .all(|p| p.status == PreferenceStatus::Conflicting),
            "both sources of an exclusive key must be flagged"
        );
    }

    #[test]
    fn cache_round_trips_within_ttl_and_keys_on_source() {
        let key = (29, true, PreferenceSourceParam::Trajectory);
        assert!(cache_get(key).is_none());
        cache_put(key, serde_json::json!({"analyzed_events": 1}));
        assert_eq!(
            cache_get(key),
            Some(serde_json::json!({"analyzed_events": 1}))
        );
        // The same window/llm pair under a different source is a miss.
        assert!(cache_get((29, true, PreferenceSourceParam::Genai)).is_none());
    }
}
