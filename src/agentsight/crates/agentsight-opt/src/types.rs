use serde::{Deserialize, Serialize};

// ── Accuracy enums (oracle-layer detector engine) ──

/// Defect type — the symptom category of an accuracy issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefectType {
    Knowledge,
    #[serde(rename = "Tool-use")]
    ToolUse,
    Workflow,
    Style,
    Context,
    Reasoning,
    Unclassified,
}

impl DefectType {
    /// Parse from free-form string (LLM output), no silent fallback to Reasoning.
    pub fn from_loose(s: &str) -> Self {
        match s.to_lowercase().replace(['_', ' '], "-").as_str() {
            "knowledge" => Self::Knowledge,
            "tool-use" | "tooluse" | "tool" => Self::ToolUse,
            "workflow" => Self::Workflow,
            "style" => Self::Style,
            "context" => Self::Context,
            "reasoning" => Self::Reasoning,
            _ => Self::Unclassified,
        }
    }
}

impl std::fmt::Display for DefectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Knowledge => write!(f, "Knowledge"),
            Self::ToolUse => write!(f, "Tool-use"),
            Self::Workflow => write!(f, "Workflow"),
            Self::Style => write!(f, "Style"),
            Self::Context => write!(f, "Context"),
            Self::Reasoning => write!(f, "Reasoning"),
            Self::Unclassified => write!(f, "Unclassified"),
        }
    }
}

/// Root-cause object — what component is to blame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootObject {
    Skill,
    Tool,
    Model,
    Context,
    Env,
    Input,
    Orchestration,
    Unclassified,
}

impl RootObject {
    /// Parse from free-form string (LLM output), no silent fallback to Model.
    pub fn from_loose(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "skill" => Self::Skill,
            "tool" => Self::Tool,
            "model" => Self::Model,
            "context" => Self::Context,
            "env" | "environment" => Self::Env,
            "input" => Self::Input,
            "orchestration" | "orchestrator" => Self::Orchestration,
            _ => Self::Unclassified,
        }
    }
}

impl std::fmt::Display for RootObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skill => write!(f, "Skill"),
            Self::Tool => write!(f, "Tool"),
            Self::Model => write!(f, "Model"),
            Self::Context => write!(f, "Context"),
            Self::Env => write!(f, "Env"),
            Self::Input => write!(f, "Input"),
            Self::Orchestration => write!(f, "Orchestration"),
            Self::Unclassified => write!(f, "Unclassified"),
        }
    }
}

/// Fix locus — where the fix should be applied (derived from RootObject).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FixLocus {
    Skill,
    #[serde(rename = "Context-policy")]
    ContextPolicy,
    #[serde(rename = "Model-routing")]
    ModelRouting,
    Tool,
    #[serde(rename = "无")]
    None,
}

impl FixLocus {
    /// Rule-derived from primary RootObject.
    pub fn from_primary(obj: &RootObject) -> Self {
        match obj {
            RootObject::Skill => Self::Skill,
            RootObject::Context => Self::ContextPolicy,
            RootObject::Model => Self::ModelRouting,
            RootObject::Tool => Self::Tool,
            _ => Self::None,
        }
    }
}

impl std::fmt::Display for FixLocus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skill => write!(f, "Skill"),
            Self::ContextPolicy => write!(f, "Context-policy"),
            Self::ModelRouting => write!(f, "Model-routing"),
            Self::Tool => write!(f, "Tool"),
            Self::None => write!(f, "无"),
        }
    }
}

/// Evidence tier — how objective the detection oracle is.
/// L1 = most objective (rule-based tool error), L5 = most subjective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvidenceTier {
    L1,
    L2,
    L3,
    L4,
    L5,
}

impl EvidenceTier {
    /// Derive confidence label from tier. L4/L5 capped at 中.
    pub fn confidence(&self) -> &'static str {
        match self {
            Self::L1 | Self::L2 => "高",
            Self::L3 | Self::L4 => "中",
            Self::L5 => "低",
        }
    }

    /// Whether this tier allows automatic patching.
    pub fn allows_auto_patch(&self) -> bool {
        matches!(self, Self::L1 | Self::L2 | Self::L3)
    }
}

impl std::fmt::Display for EvidenceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::L1 => write!(f, "L1"),
            Self::L2 => write!(f, "L2"),
            Self::L3 => write!(f, "L3"),
            Self::L4 => write!(f, "L4"),
            Self::L5 => write!(f, "L5"),
        }
    }
}

// ── Input types ──

// ── Failure analysis ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureType {
    ToolError,
    ReasoningError,
    Timeout,
    InvalidUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub failure_type: FailureType,
    pub description: String,
    pub context: String,
    #[serde(default)]
    pub recovery: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureAnalysisOutput {
    pub failures: Vec<Failure>,
}

// ── Attribution ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttributionType {
    SkillDefect,
    ModelError,
    Environment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribution {
    pub failure_index: usize,
    pub attribution: AttributionType,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionOutput {
    pub attribution: AttributionType,
    pub reasoning: String,
}

// ── Accuracy attribution (五字段正交归因, 见 docs/准确性优化.md) ──

/// One root-cause object with its role, as returned by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccRootCauseRaw {
    pub object: String, // Skill / Tool / Model / Context / Env / Input / Orchestration
    pub role: String,   // 主因（当前仅输出唯一主因）
}

/// A single accuracy issue as produced by the LLM analysis layer.
/// The LLM only produces symptom / defect_type / root_cause / detail / verify /
/// fix / evidence_tier / tool_call_id; all gate fields are rule-derived in Rust.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccIssueRaw {
    pub symptom: String,
    pub defect_type: String, // Knowledge / Tool-use / Workflow / Style / Context / Reasoning
    #[serde(default)]
    pub root_cause: Vec<AccRootCauseRaw>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub verify: String,
    #[serde(default)]
    pub fix: String,
    #[serde(default)]
    pub evidence_tier: String, // L1 / L2 / L3 / L4 — drives confidence
    #[serde(default)]
    pub tool_call_id: Option<String>, // anchor for evidence/at backfill
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccIssueOutput {
    pub issues: Vec<AccIssueRaw>,
}

/// Root-cause object + role, serialized to the frontend `RootCause` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccRootCause {
    pub object: RootObject, // Skill / Tool / Model / Context / Env / Input / Orchestration
    pub role: String,       // 主因（当前仅输出唯一主因）
}

/// A finalized accuracy issue = one row of the frontend attribution table.
/// Serialized as camelCase to match `web/src/types.ts` `AccIssue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccIssue {
    pub symptom: String,               // 现象（用户视角一句话）
    pub defect_type: DefectType,       // 缺陷类型（症状）
    pub root_cause: Vec<AccRootCause>, // 归因对象（病灶），主因在前
    pub fix_locus: FixLocus,           // 修复落点（处方）— 规则派生
    pub confidence: String,            // 置信度 高/中/低 — 由 evidence_tier 派生
    pub optimizable: bool,             // 可优化 — 规则派生闸门
    pub tier: String,                  // user-failure / internal-lead — 规则派生
    pub recovered: bool,               // 是否已自我恢复 — 规则派生
    pub evidence_tier: EvidenceTier,   // 证据层级 — detector 按 oracle 客观程度设定
    pub auto_patch: bool,              // 是否允许自动补丁 — L4/L5 为 false
    pub evidence: String,              // 真实命令/报错（回填自解析层）
    pub at: String,                    // 发生时刻，如 "@929s"（回填）
    pub detail: String,                // 完整证据/说明（LLM）
    pub verify: String,                // 反事实验证（LLM）
    pub fix: String,                   // 修复补丁（LLM）
}

// ── Extraction ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub final_answer: String,
}

// ── Flow match (Phase 2 placeholder) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStep {
    pub id: String,
    pub name: String,
    pub step_type: String,
    #[serde(default)]
    pub is_optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchRecord {
    pub expected_step_id: Option<String>,
    pub actual_step_name: Option<String>,
    pub match_type: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchSummary {
    pub total_expected: usize,
    pub matched: usize,
    pub partial: usize,
    pub skipped: usize,
    pub unexpected: usize,
    pub overall_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemStep {
    pub step_id: String,
    pub problem_type: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowMatch {
    pub expected_steps: Vec<FlowStep>,
    pub actual_steps: Vec<FlowStep>,
    pub matches: Vec<MatchRecord>,
    pub summary: MatchSummary,
    pub problem_steps: Vec<ProblemStep>,
}

// ── Evaluation (Phase 2 placeholder) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalItem {
    pub id: String,
    pub match_score: f32,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluation {
    pub evaluations: Vec<EvalItem>,
}

// ── Skill issue ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIssue {
    pub id: String,
    pub is_skill_issue: bool,
    pub reasoning: String,
    #[serde(default)]
    pub improvement_suggestion: Option<String>,
}

// ── Benchmark ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Benchmark {
    pub query: String,
    pub coverage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingBenchmarkOutput {
    pub benchmarks: Vec<Benchmark>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeBenchmark {
    pub source_scenario: String,
    pub standard_answer: String,
}

// ── Eval criteria ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedItem {
    pub id: String,
    pub content: String,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootCausesOutput {
    pub root_causes: Vec<WeightedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyActionsOutput {
    pub key_actions: Vec<WeightedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCriteria {
    pub root_causes: Vec<WeightedItem>,
    pub key_actions: Vec<WeightedItem>,
}

// ── Flow parse intermediate ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedFlowOutput {
    pub steps: Vec<FlowStep>,
    #[serde(default)]
    pub branches: Vec<serde_json::Value>,
    #[serde(default)]
    pub conditional_groups: Vec<serde_json::Value>,
    #[serde(default)]
    pub loop_groups: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActualStepRaw {
    pub id: String,
    pub name: String,
    pub step_type: String,
    #[serde(default)]
    pub dialog_start_index: Option<usize>,
    #[serde(default)]
    pub dialog_end_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActualFlowOutput {
    pub steps: Vec<ActualStepRaw>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowMatchOutput {
    pub matches: Vec<MatchRecord>,
    #[serde(default)]
    pub skipped_expected_steps: Vec<String>,
    pub summary: MatchSummary,
    #[serde(default)]
    pub problem_steps: Vec<ProblemStep>,
}

// ── Edit operations (Phase 3 placeholder) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditAction {
    Append,
    InsertAfter,
    Replace,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edit {
    pub action: EditAction,
    #[serde(default)]
    pub target: Option<String>,
    pub content: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub edits: Vec<Edit>,
    pub reasoning: String,
}

// ── Performance analysis ──

/// A single tool call record with timing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,    // Bash / Read / WebFetch ...
    pub call_id: String, // tool_use_id
    pub start: f64,      // offset in seconds from trajectory start
    pub dur: f64,        // duration in seconds
    pub cmd: String,     // command summary (truncated)
    pub err: bool,       // whether the call returned an error
    #[serde(default)]
    pub result_tokens: Option<u64>, // token count from cost side (future use)
}

/// Idle gap: a period with no tool calls (model thinking / reading context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleGap {
    pub start: f64, // gap start offset (seconds)
    pub end: f64,   // gap end offset (seconds)
    pub dur: f64,   // gap duration (seconds)
}

/// Performance statistics computed from raw trajectory events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfStats {
    pub wall_secs: f64,    // total wall-clock time
    pub tool_secs: f64,    // total tool execution time
    pub model_secs: f64,   // actual model inference time (trigger → assistant response)
    pub idle_secs: f64,    // user idle / inter-turn gaps (= wall - tool - model)
    pub tool_count: usize, // total number of tool calls
    pub tool_calls: Vec<ToolCallRecord>,
    pub top_slow: Vec<ToolCallRecord>, // top N slowest calls
    pub idle_gaps: Vec<IdleGap>,       // user idle gaps > 60s threshold
    pub frag_idle_secs: f64,           // user idle gaps in 3–60s range (fragmented)
}

// ── Performance issue identification (Rust supplies candidates → LLM analyzes) ──

/// A structured performance-problem candidate. Rust does the deterministic
/// extraction (wall-clock three-way split, slow calls, idle gaps, error retries);
/// the LLM only analyzes root cause and gives fitted optimization advice.
/// One candidate per applicable sub-type of the taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfCandidate {
    pub id: String,           // stable id, e.g. "slow_call" / "idle_long"
    pub category: String,     // 模型推理慢 / 工具执行慢 / 用户空闲
    pub subtype: String,      // 单次超慢调用 / 失败重试浪费 …
    pub optimization: String, // default optimization hint (from taxonomy)
    pub impact_secs: f64,     // wall-clock seconds attributable to this candidate
    pub pct: f64,             // impact_secs / wall_secs * 100
    pub at: String,           // "@929s" anchor (representative occurrence)
    pub facts: String,        // compact numeric facts (step/count/duration)
    pub snippet: String,      // short evidence excerpt (command, Rust-truncated)
}

/// The full perf data set fed to the LLM for strategy selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfCandidateSet {
    pub wall_secs: f64,
    pub tool_secs: f64,
    pub model_secs: f64, // actual model inference time
    pub idle_secs: f64,  // user idle / inter-turn gaps
    pub tool_count: usize,
    /// Per-turn model inference durations in seconds (for prefix_cache strategy).
    #[serde(default)]
    pub model_turn_secs: Vec<f64>,
    /// Per-turn cache stats: (prompt_tokens, cached_tokens) (for prefix_cache strategy).
    #[serde(default)]
    pub cache_turns: Vec<CacheTurn>,
    /// Top 5 slowest tool calls (for fast_tool strategy).
    pub top_tools: Vec<ToolCallRecord>,
    /// Per-tool-name aggregated stats (for fast_tool strategy).
    #[serde(default)]
    pub tool_agg: Vec<ToolAggStats>,
}

/// Per-turn cache token statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheTurn {
    /// Total prompt tokens (including cached portion).
    pub prompt_tokens: u64,
    /// Cache-read tokens (hit portion).
    pub cached_tokens: u64,
}

/// Aggregated tool usage statistics by tool name (for fast_tool strategy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAggStats {
    /// Tool name (e.g. "Bash", "Read", "WebFetch").
    pub name: String,
    /// Number of calls.
    pub count: usize,
    /// Total duration in seconds.
    pub total_secs: f64,
    /// Average duration per call.
    pub avg_secs: f64,
    /// Max single-call duration.
    pub max_secs: f64,
}

/// LLM per-strategy evaluation output (one call per strategy, parallel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfStrategyEval {
    /// Whether this strategy applies to the current trajectory.
    pub applies: bool,
    /// 现象（一句话，引用具体数据）
    #[serde(default)]
    pub symptom: String,
    /// 根因
    #[serde(default)]
    pub root_cause: String,
    /// 具体化落地优化建议
    #[serde(default)]
    pub action: String,
    /// 证据（引用时刻/时长/数量/工具名）
    #[serde(default)]
    pub evidence: String,
    /// 预估可节省秒数
    #[serde(default)]
    pub estimated_saving_secs: f64,
    /// 高 / 中 / 低
    #[serde(default)]
    pub confidence: String,
}

// ── Perf Causal Graph (性能分析因果图) ──

/// Node type in the perf causal graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerfNodeKind {
    /// Data signal observed from trajectory (e.g. "12次grep累计45s").
    Signal,
    /// Root cause identified (e.g. "单线程grep遍历大仓库").
    Cause,
    /// Optimization strategy recommended (e.g. "替换为ripgrep").
    Strategy,
}

/// A node in the perf causal graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfCausalNode {
    pub id: String,
    /// Node kind: signal / cause / strategy.
    pub kind: PerfNodeKind,
    /// Short label (<=30 chars).
    pub label: String,
    /// Optional detail text.
    #[serde(default)]
    pub detail: String,
    /// Strategy ID (only for strategy nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    /// Estimated saving in seconds (only for strategy nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_saving_secs: Option<f64>,
    /// Confidence level: 高/中/低 (only for strategy nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
}

/// An edge in the perf causal graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfCausalEdge {
    pub from: String,
    pub to: String,
    /// Relation label: "导致" (signal→cause) / "推荐" (cause→strategy).
    pub label: String,
}

/// The causal graph for perf strategy analysis visualization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfCausalGraph {
    pub nodes: Vec<PerfCausalNode>,
    pub edges: Vec<PerfCausalEdge>,
}

/// A finalized perf issue row for the frontend table (strategy selection → issue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfIssue {
    pub strategy_id: String,  // 策略 ID，如 "timeout_cap"
    pub symptom: String,      // 现象
    pub category: String,     // 大类
    pub subtype: String,      // 策略名称
    pub root_cause: String,   // 根因
    pub optimization: String, // 具体化优化建议 (action)
    pub evidence: String,     // 证据
    pub at: String,           // 位置，如 "@929s"
    pub impact_secs: f64,     // 预估可节省时长（秒）
    pub pct: f64,             // 占总耗时比例
    pub confidence: String,   // 高 / 中 / 低
}

/// Result of LLM perf strategy selection (→ frontend).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfReport {
    pub items: Vec<PerfIssue>, // selected strategy rows
    pub considered: usize,     // data signals evaluated
    pub dismissed: usize,      // signals not matched by any strategy
    pub wall_secs: f64,
    /// Causal graph: signal → cause → strategy analysis chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_graph: Option<PerfCausalGraph>,
}

// ── Cost analysis ──

/// A single segment in the content size breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSegment {
    pub label: String, // "工具返回", "工具入参", "思考", "回复正文" etc.
    pub chars: usize,  // character count
    pub pct: f64,      // percentage of total
}

/// A group of redundant tool calls detected by similar command signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedundantCallGroup {
    pub name: String,        // tool name
    pub cmd_sig: String,     // command signature (truncated)
    pub count: usize,        // how many times called
    pub wasted_chars: usize, // estimated wasted chars from replay
}

/// A textual finding from cost analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostFinding {
    pub severity: String, // "high" / "mid" / "low"
    pub html: String,     // finding text (may contain <code> <b>)
}

/// A single LLM call (= one assistant turn) with its context-window breakdown.
///
/// Fields map 1:1 to the frontend `LLMCall` interface (camelCase ↔ snake_case).
/// Every category value is a **token count**; the sum of the context categories
/// equals that step's `prompt_tokens` (billing view, includes replayed history).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCall {
    pub step_id: usize,
    pub time: String,  // relative to origin, "MM:SS"
    pub label: String, // first-action summary (tool name / "text-only")

    // ── Context window sources (breakdown of this step's prompt_tokens) ──
    // Static region (three sub-quantities): constant every step, replayed each turn.
    pub system_prompt: usize,
    pub skill_definitions: usize, // SKILL.md/AGENTS.md/MEMORY.md etc.; 0 if unavailable
    pub tool_definitions: usize,  // 0 when trajectory carries no tool schema
    pub user_messages: usize,     // user prompts, accumulated so far
    pub assistant_messages: usize, // assistant output (thinking+text), prior turns — O(n)
    pub tool_results: usize,      // tool outputs, prior returns accumulated
    pub injected_context: usize,  // others (RAG/injection); 0 when absent

    // Output (this turn's completion, not counted into later input)
    pub output_tokens: usize,

    // ── Payload-layer optimizable amounts (threshold rules, unit: token) ──
    pub cacheable: usize, // static region cacheable part (saves unit price, not deleted)
    pub history_prunable: usize, // prunable/summarizable part of old assistant output
    pub trimmable: usize, // truncatable part of oversized tool output
    pub prunable: usize,  // prunable low-relevance injected content

    // ── Orchestration-layer suggestion (orthogonal to the 4 payload fields) ──
    pub removable_turn: bool, // this turn can be merged/eliminated; true = whole turn savable

    // ── Real usage (from message.usage; None when trajectory lacks usage) ──
    // When present, the context categories above are calibrated so their sum
    // equals `real_prompt_tokens` (真值定总量、估算定比例).
    #[serde(default)]
    pub real_prompt_tokens: Option<u64>, // full context incl. cached prefix
    #[serde(default)]
    pub real_completion_tokens: Option<u64>,
    #[serde(default)]
    pub real_cached_tokens: Option<u64>, // cache-read portion of the prompt
}

/// Estimated optimization headroom aggregated from per-step replay model.
///
/// "Headroom" = how many tokens can be saved if all applicable
/// optimizations were applied. Split into payload-layer (deterministic)
/// and orchestration-layer (needs human confirmation).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostHeadroom {
    // ── Payload-layer (deterministic) ──
    /// Tokens deletable: history_prunable + trimmable + prunable (across all calls).
    pub payload_deletable_tok: usize,
    /// Tokens cacheable via Prefix Cache (static region replay).
    pub payload_cacheable_tok: usize,

    // ── Orchestration-layer (suggestion, needs_confirm) ──
    /// Tokens savable by eliminating removable turns (input + output).
    pub orch_savable_tok: usize,

    // ── Totals ──
    /// Total input tokens (billing caliber, incl. replay).
    pub total_input_tok: usize,
    /// Total output tokens.
    pub total_output_tok: usize,
    /// Headroom as percentage of total cost (0..100).
    pub pct: f64,

    // ── Headroom library实测 (optional, populated when headroom-ai available) ──
    /// Token count after headroom-ai compression (0 = not measured).
    #[serde(default)]
    pub headroom_compressed_tok: usize,
    /// Actual savings percentage from headroom-ai compression (0..100, 0 = not measured).
    #[serde(default)]
    pub headroom_save_pct: f64,
}

/// Cost statistics computed from raw trajectory events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostStats {
    pub total_events: usize,
    pub total_chars: usize,
    pub breakdown: Vec<CostSegment>,
    pub redundant_calls: Vec<RedundantCallGroup>,
    pub findings: Vec<CostFinding>,

    // ── Per-step replay model (drives the token flame chart) ──
    #[serde(default)]
    pub calls: Vec<LlmCall>, // one entry per LLM call, time-ordered
    #[serde(default)]
    pub model: String, // model name (from trajectory / default)
    #[serde(default)]
    pub token_ratio_version: String, // char→token ratio version, for auditability

    // ── Real usage aggregates (0 when the trajectory carries no usage) ──
    #[serde(default)]
    pub usage_steps: usize, // steps with real usage / calls.len()
    #[serde(default)]
    pub total_real_input_tok: u64,
    #[serde(default)]
    pub total_real_output_tok: u64,
    #[serde(default)]
    pub total_real_cached_tok: u64,

    /// Optimization headroom summary (aggregated from `calls`).
    #[serde(default)]
    pub headroom: CostHeadroom,
}

// ── Cost waste identification (Rust supplies candidates → LLM judges worth) ──

/// Playbook v1.1 ratio metrics (M-class, Rust-computed). Admission gates use
/// only relative shares/rates — absolute token thresholds don't transfer
/// across trajectories of different scale. All shares are 0..1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostRatioMetrics {
    /// M1 前缀账单占比: static prefix replay / total billed input.
    pub m1_prefix_share: f64,
    /// M3 缓存命中率 (billing truth): cached / prompt tokens. None = no usage data.
    #[serde(default)]
    pub m3_cache_hit_rate: Option<f64>,
    /// M7 历史占比: peak of (replayed history / step input) across steps.
    pub m7_history_peak_share: f64,
    /// M14 thinking / body-text output ratio (char-based proxy).
    pub m14_thinking_ratio: f64,
    /// M15 tool-call step share of all steps.
    pub m15_tool_step_share: f64,
    /// M16 空转账单占比: retry/backtrack churn tokens / total billed.
    pub m16_churn_share: f64,
}

/// One agent turn compressed to a single structured row —— the 轮次账本.
///
/// Rust fills every turn without filtering: which turns are wasted is a
/// semantic call left to the LLM, so pre-filtering here would cap recall.
/// `turn` is the canonical turn ordinal (identical to `LlmCall::step_id`), and
/// it is the only step numbering the prevention prompts expose — verdict step
/// references are validated against it before any token math.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnLedgerRow {
    pub turn: usize,
    /// Normalized action signature (tool + target, variable parts stripped) so
    /// that a pitfall recurring many turns apart is visible as the same action.
    pub action_sig: String,
    /// Whether every tool call in this turn errored.
    pub is_error: bool,
    /// Head of the error text (empty when `is_error` is false).
    pub err_head: String,
    /// Files this turn wrote/edited (targets of write-like tools).
    pub files: Vec<String>,
    /// Billing-caliber tokens of this turn (replayed context + output).
    pub tokens: usize,
    /// Whether a user message immediately precedes this turn.
    pub after_user: bool,
    /// Head of that user message (empty when `after_user` is false).
    pub user_head: String,
    /// Turn contains a backtrack-looking command (git reset/revert/…).
    pub backtrack: bool,
    /// Head of the agent's own narration this turn — direction declarations
    /// ("found the root cause", "换个思路") live here, and declare-then-overturn
    /// pairs are the primary signal for direction-level detours.
    pub say_head: String,
}

/// A reusable lesson distilled from a prevention-class waste finding.
///
/// Two shapes share one struct: 试错型 fills applicability/pitfall/effective_path,
/// 返工型 fills rule/good_example/bad_example/scope. Empty fields are omitted so
/// the frontend renders whichever shape came back.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasteExperience {
    // ── 试错型 ──
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub applicability: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pitfall: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effective_path: String,
    // ── 返工型 ──
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rule: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub good_example: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bad_example: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    // ── 共用：五字段归因（本期只进 JSON，不渲染）──
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub defect_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root_cause: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix_locus: String,
}

/// One remediation attached to a detour finding — where to land the lesson
/// and the paste-ready experience fields backing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetourFix {
    /// 一句话可执行修复动作 (→ WasteItem.optimization).
    #[serde(default)]
    pub action: String,
    /// 落点: Skill / 项目规范 / 系统提示词 / 环境配置 / 框架配置.
    #[serde(default)]
    pub locus: String,
    #[serde(default)]
    pub applicability: String,
    #[serde(default)]
    pub pitfall: String,
    #[serde(default)]
    pub effective_path: String,
}

/// One detour segment reported by the detour prompt.
///
/// `turns` carries turn ordinals only — token savings are summed by Rust from
/// the ledger, never taken from the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetourFinding {
    #[serde(default)]
    pub turns: Vec<usize>,
    /// 一句话现象.
    #[serde(default)]
    pub what: String,
    /// 归因: 可预知坑 / 隐性规范 / 方向选错 / 偶发故障 / 无意义重复.
    #[serde(default)]
    pub why: String,
    /// 归因依据 — must cite turn numbers and ledger facts.
    #[serde(default)]
    pub why_detail: String,
    /// Remediation; `None` is mandatory for 偶发故障 (enforced in Rust).
    #[serde(default)]
    pub fix: Option<DetourFix>,
}

/// LLM verdict for the detour candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetourVerdict {
    #[serde(default)]
    pub detected: bool,
    #[serde(default)]
    pub findings: Vec<DetourFinding>,
}

/// A structured waste candidate: Rust does the deterministic extraction
/// (grouping, token math, evidence snippets); the LLM only judges whether it's
/// worth optimizing. One candidate per applicable sub-type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasteCandidate {
    pub id: String,                   // stable id, e.g. "tool_output" / "detour"
    pub category: String,             // 上下文臃肿 / 步骤冗余 / 推理开销
    pub subtype: String,              // 工具输出多 / 反复重试 …
    pub optimization: String,         // 工具输出截断 / 历史消息裁剪 …
    pub potential_save_tokens: usize, // billing-caliber upper bound (incl. replay)
    #[serde(default)]
    pub discount: bool, // true = saves unit price (cache), not deleted tokens
    /// 预计节省的账单占比 (0..1) — drives noise-line gating and ordering.
    #[serde(default)]
    pub save_share: f64,
    /// 节省口径: 折价 / 可省 / 预防 — the three kinds must not be summed.
    #[serde(default)]
    pub savings_kind: String,
    pub steps: Vec<usize>, // involved step ids (evidence)
    pub facts: String,     // compact numeric facts
    pub snippet: String,   // short evidence excerpt (Rust-truncated)
}

/// The full candidate set fed to the LLM.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasteCandidateSet {
    pub model: String,
    pub total_steps: usize,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    /// Trajectory-level ratio metrics shared by every strategy prompt.
    #[serde(default)]
    pub metrics: CostRatioMetrics,
    pub candidates: Vec<WasteCandidate>,
    /// 轮次账本 — per-turn structural rows feeding the prevention prompts and
    /// the token math that backs their verdicts.
    #[serde(default)]
    pub ledger: Vec<TurnLedgerRow>,
}

/// LLM verdict for one candidate (one call per candidate/strategy, parallel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasteVerdict {
    pub worth_optimizing: bool,
    #[serde(default)]
    pub save_ratio: f64, // 0..1 — fraction of potential actually worth cutting
    #[serde(default)]
    pub symptom: String, // 现象（一句话）
    #[serde(default)]
    pub evidence: String, // 证据（含步号/数量）
    #[serde(default)]
    pub confidence: String, // 高 / 中 / 低
    #[serde(default)]
    pub reason: String, // 为何值得 / 不值得
}

/// A finalized waste row for the frontend table (candidate × verdict, joined).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasteItem {
    pub symptom: String,      // 现象
    pub category: String,     // 大类
    pub subtype: String,      // 子类型
    pub optimization: String, // 优化手段
    pub evidence: String,     // 证据
    pub save_tokens: usize,   // 预计可省 token
    pub discount: bool,       // 缓存类（省单价）
    /// 节省口径: 折价（省单价）/ 可省（本次可回收）/ 预防（防下次复发）。
    #[serde(default)]
    pub savings_kind: String,
    pub confidence: String,  // 高 / 中 / 低
    pub needs_confirm: bool, // 编排层：建议·需确认
    /// Involved turn ordinals (prevention-class rows only; empty otherwise).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<usize>,
    /// Reusable lesson distilled from a prevention-class finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experience: Option<WasteExperience>,
}

/// Result of LLM cost-waste identification (→ frontend).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasteReport {
    pub items: Vec<WasteItem>, // worth-optimizing rows only
    pub considered: usize,     // candidates evaluated
    pub dismissed: usize,      // judged not worth optimizing
    pub model: String,
}

// ── Analysis report ──

/// Result of accuracy analysis (LLM-dependent: extraction + failures).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccuracyResult {
    pub extraction: ExtractionResult,
    pub failures: Vec<Failure>,
    /// 五字段正交归因结果（主渲染）；`failures` 保留兼容旧渲染。
    #[serde(default)]
    pub issues: Vec<AccIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub extraction: ExtractionResult,
    pub failures: Vec<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf: Option<PerfStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostStats>,
}

// ── Trace Overview (轨迹图) ──

/// A single ATIF step (one assistant turn + its tool results).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceStep {
    /// Step ordinal within the phase (1-based).
    pub seq: usize,
    /// ISO-8601 timestamp of the assistant event that started this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Reasoning/thinking content (if present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Assistant text response (if present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool calls made in this step.
    #[serde(default)]
    pub tool_calls: Vec<TraceToolCall>,
    /// Tool results (from the subsequent user event).
    #[serde(default)]
    pub tool_results: Vec<TraceToolResult>,
}

/// A tool call within a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceToolCall {
    pub id: String,
    pub name: String,
    /// Full tool input as pretty-printed JSON.
    pub input: String,
    /// Brief human-readable summary (e.g. file_path or command).
    pub input_summary: String,
}

/// A tool result paired with its call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceToolResult {
    pub id: String,
    pub content: String,
    pub is_error: bool,
}

/// A phase = one user turn + its ATIF steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePhase {
    /// Phase index (0-based).
    pub index: usize,
    /// User turn ordinal (1-based).
    pub turn: usize,
    /// ISO-8601 timestamp of the user event that started this phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// User message text.
    pub text: String,
    /// ATIF steps in this phase.
    pub steps: Vec<TraceStep>,
    /// Whether any tool result errored.
    pub has_error: bool,
}

/// The complete trace overview for visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceOverview {
    pub phases: Vec<TracePhase>,
    pub total_turns: usize,
    pub total_steps: usize,
}

// ── Intent Evolution Graph (用户意图关联图) ──

/// Lifecycle status of a user intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    /// Currently being pursued.
    Active,
    /// Completed / satisfied.
    Fulfilled,
    /// Abandoned (e.g. approach replaced).
    Abandoned,
    /// Suspended (interrupted, may resume later).
    Suspended,
}

/// Category of a user intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentCategory {
    /// "Do something" (implement feature, fix bug).
    Task,
    /// "Must satisfy X" (use JWT, add validation).
    Constraint,
    /// Asking or confirming ("is this right?").
    Question,
    /// Evaluation / correction ("no", "ok continue").
    Feedback,
}

/// Relation type between two user intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRelation {
    /// B is a more precise/specific version of A (same direction, higher precision).
    Refines,
    /// B adds a new requirement dimension on top of A (same direction, wider scope).
    Extends,
    /// B corrects a misunderstanding or wrong direction in A.
    Corrects,
    /// B switches to a completely new topic/requirement (unrelated to A).
    Pivots,
    /// B is a simple confirmation/urge (no substantive change).
    Continues,
    /// B re-activates a previously suspended intent direction (interrupted then restarted).
    Resumes,
    /// B imposes constraints/cleanup/refactoring on existing implementation (not a new requirement).
    Constrains,
}

/// Trigger source for intent changes (Phase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentTrigger {
    /// User proactively adjusted.
    Proactive,
    /// Forced by model's erroneous output.
    ReactiveError,
    /// Forced by model's ambiguous output.
    ReactiveAmbiguity,
    /// Model missed requirements, user supplemented.
    ReactiveIncomplete,
}

/// A single user intent node extracted from a user turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentNode {
    /// Node ID, format: "intent_{turn_index}".
    pub id: String,
    /// LLM-generated intent summary (<=30 chars).
    pub label: String,
    /// Corresponding user_turn ordinal (1-based).
    pub turn_index: usize,
    /// Mainline this intent belongs to (e.g. "A").
    pub mainline: String,
    /// Lifecycle status.
    pub status: IntentStatus,
    /// Intent category.
    pub category: IntentCategory,
}

/// An edge representing a relation between two intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentEdge {
    /// Source node ID.
    pub from: String,
    /// Target node ID.
    pub to: String,
    /// Relation type.
    pub kind: IntentRelation,
    /// LLM-generated relation explanation.
    pub label: String,
    /// LLM confidence 0..1.
    pub confidence: f64,
    /// Phase 2: trigger source (only for corrects/pivots/resumes edges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<IntentTrigger>,
}

/// A requirement mainline clustering multiple intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mainline {
    /// Mainline ID (e.g. "A").
    pub id: String,
    /// Human-readable mainline label.
    pub label: String,
    /// Intent node IDs belonging to this mainline.
    pub node_ids: Vec<String>,
    /// Overall mainline status.
    pub status: IntentStatus,
}

/// Metadata about the intent graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentGraphMeta {
    /// Total user turns in the trajectory.
    pub total_turns: usize,
    /// Number of intent nodes extracted.
    pub total_intents: usize,
    /// Number of relation edges.
    pub total_edges: usize,
    /// Number of mainlines.
    pub mainline_count: usize,
}

/// The complete intent evolution graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentGraph {
    pub nodes: Vec<IntentNode>,
    pub edges: Vec<IntentEdge>,
    pub mainlines: Vec<Mainline>,
    pub meta: IntentGraphMeta,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loose_defect_types() {
        assert_eq!(DefectType::from_loose("tool_use"), DefectType::ToolUse);
        assert_eq!(DefectType::from_loose("tool"), DefectType::ToolUse);
        assert_eq!(DefectType::from_loose("reasoning"), DefectType::Reasoning);
        assert_eq!(DefectType::from_loose("unknown"), DefectType::Unclassified);
        assert_eq!(DefectType::ToolUse.to_string(), "Tool-use");
        assert_eq!(
            serde_json::to_string(&DefectType::ToolUse).unwrap(),
            "\"Tool-use\""
        );
    }

    #[test]
    fn parses_loose_root_objects_and_fix_locus() {
        assert_eq!(RootObject::from_loose("environment"), RootObject::Env);
        assert_eq!(
            RootObject::from_loose("orchestrator"),
            RootObject::Orchestration
        );
        assert_eq!(RootObject::from_loose("missing"), RootObject::Unclassified);

        assert_eq!(FixLocus::from_primary(&RootObject::Skill), FixLocus::Skill);
        assert_eq!(
            FixLocus::from_primary(&RootObject::Context),
            FixLocus::ContextPolicy
        );
        assert_eq!(
            FixLocus::from_primary(&RootObject::Model),
            FixLocus::ModelRouting
        );
        assert_eq!(FixLocus::from_primary(&RootObject::Tool), FixLocus::Tool);
        assert_eq!(FixLocus::from_primary(&RootObject::Input), FixLocus::None);
        assert_eq!(serde_json::to_string(&FixLocus::None).unwrap(), "\"无\"");
    }

    #[test]
    fn derives_evidence_confidence_and_patch_gate() {
        assert_eq!(EvidenceTier::L1.confidence(), "高");
        assert_eq!(EvidenceTier::L3.confidence(), "中");
        assert_eq!(EvidenceTier::L4.confidence(), "中");
        assert_eq!(EvidenceTier::L5.confidence(), "低");
        assert!(EvidenceTier::L1.allows_auto_patch());
        assert!(EvidenceTier::L3.allows_auto_patch());
        assert!(!EvidenceTier::L4.allows_auto_patch());
        assert!(!EvidenceTier::L5.allows_auto_patch());
        assert_eq!(EvidenceTier::L2.to_string(), "L2");
    }
}
