//! Serializable diagnosis report and the fixed text attached to each cause.

use serde::{Deserialize, Serialize};

use crate::storage::sqlite::genai::MetricPercentiles;

/// Tunables of one diagnosis run.
#[derive(Debug, Clone, Copy)]
pub struct CacheDiagnosisOptions {
    /// Conservative hit rate below which a sufficiently sampled group is low.
    pub low_hit_rate: f64,
    /// Gap after which a provider cache entry is assumed expired.
    pub cache_ttl_secs: u64,
    /// Unit prices; without them only token counts are reported.
    pub prices: Option<CachePrices>,
}

impl Default for CacheDiagnosisOptions {
    fn default() -> Self {
        Self {
            low_hit_rate: 0.5,
            cache_ttl_secs: 300,
            prices: None,
        }
    }
}

/// Prices per million tokens; they depend on provider, model, region and contract.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CachePrices {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// Diagnosis of one time window.
#[derive(Debug, Serialize)]
pub struct CacheDiagnosis {
    pub start_ns: i64,
    pub end_ns: i64,
    /// The window exceeded the row cap and only the newest calls were read.
    pub truncated: bool,
    pub groups: Vec<CacheGroupDiagnosis>,
}

/// Overall judgement of one agent/provider/model group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    /// No call reported token usage.
    NoUsage,
    /// Usage exists but no call reported a cache-read counter.
    Undeterminable,
    /// Too few records or tokens to judge the rate.
    InsufficientSamples,
    /// The rate is below the threshold or dropped sharply against the baseline.
    NeedsReview,
    Healthy,
}

/// Cache diagnosis of one agent, provider and model.
#[derive(Debug, Serialize)]
pub struct CacheGroupDiagnosis {
    pub agent_name: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: CacheStatus,
    pub coverage: CacheCoverage,
    pub tokens: CacheTokens,
    /// Reads over all known input; missing read counters count as zero.
    pub conservative_hit_rate: Option<f64>,
    /// Reads over input of calls that reported both counters.
    pub complete_hit_rate: Option<f64>,
    pub latency: CacheLatency,
    pub cost: Option<CacheCost>,
    /// Causes of uncached input, most actionable first.
    pub findings: Vec<CacheFinding>,
    pub baseline: Option<CacheComparison>,
}

/// Record and field coverage of a group.
#[derive(Debug, Default, Serialize)]
pub struct CacheCoverage {
    pub calls: usize,
    pub duplicate_calls: usize,
    pub failed_calls: usize,
    pub usage_calls: usize,
    pub cache_read_field_calls: usize,
    pub cache_creation_field_calls: usize,
    pub explicit_zero_read_calls: usize,
}

/// Token totals over calls with usage.
#[derive(Debug, Default, Serialize)]
pub struct CacheTokens {
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
    /// Input neither read from nor written to the cache.
    pub uncached_input: i64,
}

/// Latency of calls that mostly hit the cache against calls that mostly missed.
#[derive(Debug, Default, Serialize)]
pub struct CacheLatency {
    pub warm: CacheLatencyBucket,
    pub cold: CacheLatencyBucket,
}

/// Latency of one hit-rate bucket; input size is reported because it also drives TTFT.
#[derive(Debug, Default, Serialize)]
pub struct CacheLatencyBucket {
    pub calls: usize,
    pub median_input_tokens: Option<i64>,
    pub ttft_ms: Option<MetricPercentiles>,
    pub e2e_latency_ms: Option<MetricPercentiles>,
}

/// Input cost under the supplied prices.
#[derive(Debug, Default, Serialize)]
pub struct CacheCost {
    pub input_cost: f64,
    pub output_cost: f64,
    /// Input cost had nothing been cached.
    pub no_cache_input_cost: f64,
    /// Saving against no caching, net of the cache-write premium.
    pub cache_savings: f64,
    /// Further saving had missed reusable input been read from the cache.
    pub missed_savings: f64,
    /// Input cost per million input tokens, comparable across windows of different load.
    pub input_cost_per_mtok: Option<f64>,
    /// Calls whose read plus write exceeds input; their counters use incompatible semantics.
    pub incompatible_calls: usize,
}

/// Cause attached to a call with uncached input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheReason {
    PrefixNotReused,
    FixedPrefixPlateau,
    SystemPromptChanged,
    ToolsChanged,
    ModelSwitched,
    HistoryRewritten,
    CacheExpired,
    FailedCalls,
    CacheFieldsMissing,
    NoSessionContext,
    ColdStart,
    NewInputDominant,
}

/// How much a finding says about a defect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Reusable input was not read and nothing expected explains it.
    Suspected,
    /// The counters needed to judge are missing.
    Undeterminable,
    /// Normal cache behaviour, such as a cold start or new content.
    Expected,
}

impl CacheReason {
    /// Defect evidence this cause carries.
    pub fn kind(self) -> FindingKind {
        match self {
            Self::PrefixNotReused
            | Self::FixedPrefixPlateau
            | Self::SystemPromptChanged
            | Self::ToolsChanged
            | Self::ModelSwitched => FindingKind::Suspected,
            Self::CacheFieldsMissing | Self::FailedCalls | Self::NoSessionContext => {
                FindingKind::Undeterminable
            }
            Self::HistoryRewritten
            | Self::CacheExpired
            | Self::ColdStart
            | Self::NewInputDominant => FindingKind::Expected,
        }
    }

    /// Remedy shown next to the cause.
    pub fn recommendation(self) -> &'static str {
        match self {
            Self::PrefixNotReused => {
                "The unchanged previous input was not read from the cache. Check that explicit cache markers cover the conversation history and tool results, sit on non-empty content, and survive SDK, proxy and protocol conversion."
            }
            Self::FixedPrefixPlateau => {
                "Cache reads stay at one fixed size while the input grows, so only a static prefix is cached. Move the cache breakpoint forward with the history, for example onto the latest tool result."
            }
            Self::SystemPromptChanged => {
                "The system prompt changed between adjacent calls. Keep timestamps, random IDs and retrieved content out of the prompt prefix."
            }
            Self::ToolsChanged => {
                "The tool definitions changed between adjacent calls. Keep the tool set, its order and its schema serialization stable."
            }
            Self::ModelSwitched => {
                "The model changed within the session and caches are per model and route. Confirm the switch is intended and check the cache strategy of the new route."
            }
            Self::HistoryRewritten => {
                "Earlier messages were dropped or rewritten, as compaction or truncation does; the cached prefix ends at the first changed message."
            }
            Self::CacheExpired => {
                "The previous call ended longer ago than the cache TTL, so the entry had expired."
            }
            Self::FailedCalls => {
                "Failed calls rarely read the cache; investigate the errors before the cache."
            }
            Self::CacheFieldsMissing => {
                "The response carried no cache-read counter, which is not a confirmed miss. Check the raw usage fields and capture coverage."
            }
            Self::NoSessionContext => {
                "The call has no session or conversation ID, so it cannot be compared with a previous call."
            }
            Self::ColdStart => {
                "The first call of a session writes the cache. Longer sessions or a shared static prefix amortize it."
            }
            Self::NewInputDominant => {
                "The previous input was reused; the low ratio comes from new content in this call."
            }
        }
    }
}

/// Calls of a group sharing one cause.
#[derive(Debug, Serialize)]
pub struct CacheFinding {
    pub reason: CacheReason,
    pub kind: FindingKind,
    pub calls: usize,
    /// Input of the affected calls that was not read from the cache, cache writes included.
    pub unread_tokens: i64,
    /// Previous-call input that could have been read but was not.
    pub missed_tokens: i64,
    pub ttft_ms_p50: Option<f64>,
    pub e2e_latency_ms_p50: Option<f64>,
    pub recommendation: &'static str,
    pub examples: Vec<CacheExample>,
}

/// One affected call, enough to open it in the trace view.
#[derive(Debug, Clone, Serialize)]
pub struct CacheExample {
    pub trace_id: Option<String>,
    pub call_id: Option<String>,
    pub session_id: Option<String>,
    pub start_ns: i64,
    pub input_tokens: i64,
    pub cache_read_tokens: Option<i64>,
    pub previous_input_tokens: Option<i64>,
}

/// Direction of a change against the baseline window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTrend {
    Improved,
    Regressed,
    Unchanged,
    /// One side lacks usage or cache-read counters.
    Incomparable,
}

/// The same group in the baseline window.
#[derive(Debug, Serialize)]
pub struct CacheComparison {
    pub status: CacheStatus,
    pub conservative_hit_rate: Option<f64>,
    /// Current minus baseline conservative rate, in percentage points.
    pub hit_rate_change_pp: Option<f64>,
    pub input_cost_per_mtok: Option<f64>,
    pub trend: CacheTrend,
    /// Both windows meet the sample thresholds.
    pub sufficient_samples: bool,
}
