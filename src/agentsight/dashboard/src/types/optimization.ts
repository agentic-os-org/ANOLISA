// Optimization analysis types — ported from agentopt web/src/types.ts.
// Field names must stay 1:1 with the backend JSON (Rust agentopt_core::types).

// ── Accuracy dimension ──────────────────────────────────────────────────

export type FailureType = 'tool_error' | 'reasoning_error' | 'timeout' | 'invalid_usage';

export interface Failure {
  failure_type: FailureType;
  description: string;
  context: string;
  recovery: string | null;
}

export interface ExtractionResult {
  final_answer: string;
}

// 归因模型五字段
export type DefectType = 'Knowledge' | 'Tool-use' | 'Workflow' | 'Style' | 'Context' | 'Reasoning';
export type RootObject = 'Skill' | 'Tool' | 'Model' | 'Context' | 'Env' | 'Input' | 'Orchestration';
export type FixLocus = 'Skill' | 'Context-policy' | 'Model-routing' | 'Tool' | '无';

export interface RootCause {
  object: RootObject;
  role: '主因';
}

export interface AccIssue {
  symptom: string;           // 现象（一句话说清出了啥问题）
  defectType: DefectType;    // 缺陷类型（症状）
  rootCause: RootCause[];    // 归因对象（病灶），主因在前
  fixLocus: FixLocus;        // 修复落点（处方）
  confidence: '高' | '中' | '低'; // 置信度（把握）
  optimizable: boolean;      // 可优化：false → 不进优化队列
  tier: 'user-failure' | 'internal-lead'; // 用户已感知失败 / 内部优化线索
  recovered: boolean;        // 是否已自我恢复（走弯路但结果对）
  evidenceTier: 'L1' | 'L2' | 'L3' | 'L4' | 'L5'; // 证据层级
  autoPatch: boolean;        // 是否允许自动补丁（L4/L5 为 false）
  // 展开区
  evidence: string;          // 证据：真实命令 / 报错
  at: string;                // 发生位置，如 '@929s'
  detail: string;            // 完整证据 / 说明（允许 <code>）
  verify: string;            // 反事实验证
  fix: string;               // 修复补丁（允许 <code>）
}

/** Response of POST /api/optimize/sessions/{id}/accuracy */
export interface AccuracyResult {
  extraction: ExtractionResult;
  failures: Failure[];
  issues: AccIssue[];
}

// ── Perf dimension ──────────────────────────────────────────────────────

/** Element type of PerfStats.tool_calls / top_slow. */
export interface ToolCallRecord {
  name: string;
  call_id: string;
  start: number;
  dur: number;
  cmd: string;
  err: boolean;
}

export interface IdleGap {
  start: number;
  end: number;
  dur: number;
}

export interface PerfStats {
  wall_secs: number;
  tool_secs: number;
  model_secs: number;
  idle_secs: number;
  tool_count: number;
  tool_calls: ToolCallRecord[];
  top_slow: ToolCallRecord[];
  idle_gaps: IdleGap[];
}

// LLM-selected optimization strategy row (Rust supplies data → LLM selects strategies).
export interface PerfIssueItem {
  strategy_id: string;   // 策略 ID，如 "timeout_cap"
  symptom: string;       // 现象
  category: string;      // 大类：模型推理慢 / 工具执行慢 / 用户空闲
  subtype: string;       // 策略名称
  root_cause: string;    // 根因
  optimization: string;  // 具体化优化建议
  evidence: string;      // 证据
  at: string;            // 位置，如 "@929s"
  impact_secs: number;   // 预估可节省时长（秒）
  pct: number;           // 占总耗时比例
  confidence: string;    // 高 / 中 / 低
}

export interface PerfReport {
  items: PerfIssueItem[];
  considered: number;
  dismissed: number;
  wall_secs: number;
}

// ── Cost dimension ──────────────────────────────────────────────────────

export interface CostSegment {
  label: string;
  chars: number;
  pct: number;
}

export interface RedundantCallGroup {
  name: string;
  cmd_sig: string;
  count: number;
  wasted_chars: number;
}

export interface CostFinding {
  severity: string;
  html: string;
}

// One LLM call (= one assistant turn). Fields map 1:1 to Rust `LlmCall`.
// Every value is a token count; the context categories sum to that step's prompt_tokens.
export interface LLMCall {
  step_id: number;
  time: string;                 // relative "MM:SS"
  label: string;                // first-action summary (tool name / "text-only")

  // Context window sources (breakdown of this step's prompt_tokens, incl. replay)
  system_prompt: number;        // static region — constant, replayed
  skill_definitions: number;    // static region; 0 if unavailable
  tool_definitions: number;     // static region; 0 if no tool schema
  user_messages: number;        // user prompts, accumulated
  assistant_messages: number;   // assistant output (thinking+text+tool args), O(n) accumulation
  tool_results: number;         // tool outputs, accumulated
  injected_context: number;     // others (RAG/injection); 0 when absent

  // Output (this turn's completion, not counted into later input)
  output_tokens: number;

  // Payload-layer optimizable amounts (threshold rules, unit: token)
  cacheable: number;            // static region cacheable (saves unit price, not deleted)
  history_prunable: number;     // prunable/summarizable old assistant output
  trimmable: number;            // truncatable oversized tool output
  prunable: number;             // prunable low-relevance injected content

  // Orchestration-layer suggestion (orthogonal to the 4 payload fields)
  removable_turn: boolean;      // whole turn mergeable/eliminable

  // Real usage from message.usage (absent when trajectory lacks usage).
  real_prompt_tokens?: number;
  real_completion_tokens?: number;
  real_cached_tokens?: number;
}

export interface CostHeadroom {
  payload_deletable_tok: number;
  payload_cacheable_tok: number;
  orch_savable_tok: number;
  total_input_tok: number;
  total_output_tok: number;
  pct: number;
  headroom_compressed_tok?: number;
  headroom_save_pct?: number;
}

export interface CostStats {
  total_events: number;
  total_chars: number;
  breakdown: CostSegment[];
  redundant_calls: RedundantCallGroup[];
  findings: CostFinding[];
  // Per-step replay model (drives the token flame chart). Empty for legacy sessions.
  calls?: LLMCall[];
  model?: string;
  token_ratio_version?: string;
  // Real usage aggregates (0 / absent when the trajectory carries no usage)
  usage_steps?: number;
  total_real_input_tok?: number;
  total_real_output_tok?: number;
  total_real_cached_tok?: number;
  headroom?: CostHeadroom;
}

/** 可复用经验：试错型填 applicability/pitfall/effective_path，返工型填 rule/example/scope。 */
export interface WasteExperience {
  applicability?: string;
  pitfall?: string;
  effective_path?: string;
  rule?: string;
  good_example?: string;
  bad_example?: string;
  scope?: string;
  defect_type?: string;
  root_cause?: string;
  fix_locus?: string;
}

// LLM-judged waste row (Rust supplies candidates → LLM judges worth).
export interface WasteItem {
  symptom: string;        // 现象
  category: string;       // 大类
  subtype: string;        // 子类型
  optimization: string;   // 优化手段
  evidence: string;       // 证据
  save_tokens: number;    // 预计可省 token
  discount: boolean;      // 缓存类（省单价）
  confidence: string;     // 高 / 中 / 低
  needs_confirm: boolean; // 编排层建议标记（后端仍下发，前端仅呈现、不再展示确认提示）
  savings_kind?: string;  // 节省口径：折价 / 可省 / 预防
  steps?: number[];       // 涉及轮号（减轮次浪费类）
  experience?: WasteExperience | null; // 可沉淀经验（减轮次浪费类）
}

export interface WasteReport {
  items: WasteItem[];
  considered: number;
  dismissed: number;
  model: string;
}

// ── Trajectory summary（LLM 叙事摘要）─────────────────────────────────────

/** 一次 LLM 调用产出的会话叙事摘要：目标 / 过程 / 结果 */
export interface TrajectorySummary {
  goal: string;
  process: string[];
  outcome: string;
}

// ── Aggregated report ───────────────────────────────────────────────────

export interface AnalysisReport {
  extraction: ExtractionResult;
  failures: Failure[];
  issues?: AccIssue[];        // 五字段正交归因（主渲染）
  perf: PerfStats | null;
  perf_issues?: PerfReport | null;
  cost: CostStats | null;
  cost_waste?: WasteReport | null;
  summary?: TrajectorySummary | null;
}

/** Response of GET /api/optimize/sessions/{id}/results */
export interface OptimizeSessionResults {
  session_id: string;
  perf: PerfStats | null;
  perf_issues: PerfReport | null;
  cost: CostStats | null;
  cost_waste: WasteReport | null;
  accuracy: AccuracyResult | null;
  summary: TrajectorySummary | null;
  created_at_ns?: number;
  updated_at_ns?: number;
}

/** Response of GET/POST /api/optimize/config (api_key is masked by the backend). */
export interface OptimizeLlmConfig {
  api_key: string | null;
  base_url: string;
  model: string;
  semantic_search_timeout_secs: number;
  configured: boolean;
}
