export interface Message {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  tokens: number;
  timestamp: number;
  type?: 'text' | 'code' | 'tool_call' | 'tool_result';
  eventType?: 'request' | 'response' | 'command';
}

export interface Conversation {
  id: string;
  title: string;
  messages: Message[];
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  model: string;
  createdAt: number;
  updatedAt: number;
}

export interface TokenStats {
  total: number;
  byRole: Record<string, number>;
  byType: Record<string, number>;
  byMessage: Array<{ id: string; tokens: number; percentage: number }>;
}

// ==================== ChatML Token Breakdown Types ====================

/** ChatML Token 分解树节点 */
export interface TokenBreakdownNode {
  name: string;
  label: string;
  tokens: number;
  percentage: number;
  char_count: number;
  segment_type?: string;
  children?: TokenBreakdownNode[];
  items?: MessageItem[];
}

/** 对话消息明细 */
export interface MessageItem {
  index: number;
  tokens: number;
  is_history: boolean;
  content: string;
}

/** 完整 ChatML Token 分解结果 */
export interface ChatMLTokenBreakdown {
  file_path: string;
  model_name: string;
  total_tokens: number;
  blocks: TokenBreakdownNode[];
}

// ==================== Execution Trace Types ====================

/** 执行轨迹条目类型 */
export type TraceEntryType = 
  | 'assistant_text' 
  | 'assistant_skill' 
  | 'assistant_bash' 
  | 'assistant_read'
  | 'tool_result' 
  | 'user_message';

/** 执行轨迹条目 */
export interface TraceEntry {
  id: string;
  type: TraceEntryType;
  role: 'assistant' | 'user' | 'tool';
  title: string;
  subtitle?: string;
  content: string;
  tokens: number;
  isHistory: boolean;
  timestamp?: number;
  metadata?: {
    skillName?: string;
    command?: string;
    filePath?: string;
  };
}

/** 执行轨迹分组 */
export interface TraceGroup {
  id: string;
  title: string;
  entries: TraceEntry[];
  totalTokens: number;
}

// ==================== Trace Event Types ====================

/** Trace 事件子项 */
export interface TraceEventChild {
  name: string;
  label: string;
  tokens: number;
  percentage: number;
  char_count: number;
  content?: string;
  is_history?: boolean;
  response_items?: Array<{
    index: number;
    tokens: number;
    char_count: number;
    content: string;
  }>;
}

/** 分类统计项 */
export interface SummaryCategory {
  count: number;
  tokens: number;
  percentage: number;
}

/** Event 内嵌 summary（by_role + by_history 嵌套结构）*/
export interface EventSummary {
  by_role?: Record<string, SummaryCategory>;
  by_history?: Record<string, SummaryCategory>;
}

/** Trace 事件 */
export interface TraceEvent {
  type: 'request' | 'response' | 'toolcall';
  label: string;
  tokens: number;
  percentage: number;
  char_count: number;
  summary?: EventSummary;
  children: TraceEventChild[] | null;
  /** toolcall 类型特有字段 */
  cmdline?: string;
  stdout?: string;
  stderr?: string;
}

/** 根级分组统计（input/output 各含 by_role + by_history）*/
export interface GroupedSummary {
  tokens: number;
  percentage: number;
  by_role?: Record<string, SummaryCategory>;
  by_history?: Record<string, SummaryCategory>;
}

/** Trace 数据根结构 */
export interface TraceData {
  file_path?: string;
  model_name: string;
  total_tokens: number;
  summary?: {
    input?: GroupedSummary;
    output?: GroupedSummary;
    /** 兼容旧格式 */
    by_history?: Record<string, SummaryCategory>;
    by_role?: Record<string, SummaryCategory>;
  };
  events: TraceEvent[];
}

// ==================== Agent Health Types ====================

export interface AgentActivitySummary {
  agent_name: string;
  last_seen_ns: number;
  genai_calls: number;
  genai_tokens: number;
  trajectory_steps: number;
  trajectory_tokens: number;
  source: 'genai_events' | 'trajectories' | 'genai_events+trajectories';
}

export interface AgentHealthResponse {
  agents: AgentActivitySummary[];
  warnings?: string[];
}

export interface AgentHealthStatus {
  pid: number;
  agent_name: string;
  category: string;
  exe_path: string;
  /** Agent 进程当前工作目录，用作安全保护默认范围 */
  workspace_path?: string;
  ports: number[];
  status: 'healthy' | 'unhealthy' | 'hung' | 'unknown' | 'no_port' | 'offline';
  last_check_time: number;
  latency_ms: number | null;
  error_message: string | null;
  restart_cmd?: string[];
  offline_since?: number;
  role: 'gateway' | 'client' | 'worker';
  parent_pid?: number;
  has_crash?: boolean;
}

export interface AgentProcessHealthResponse {
  agents: AgentHealthStatus[];
  last_scan_time: number;
  filtered_count: number;
}


// ─── ATIF (shared v1.7 schema) ───────────────────────────────────────────────
// Mirrors the `agentsight-atif` crate, which both the eBPF export
// (/api/export/atif/*) and the collector store (/api/trajectories/*) emit.

export interface AtifToolCall {
  tool_call_id: string;
  function_name: string;
  arguments: any;
}

export interface AtifObservationResult {
  source_call_id?: string;
  /** Any JSON per the schema — AgentSight's exporter flattens it to a string. */
  content?: unknown;
  subagent_trajectory_ref?: SubagentTrajectoryRef[];
}

export interface SubagentTrajectoryRef {
  trajectory_id?: string;
  trajectory_path?: string;
  session_id?: string;
  extra?: any;
}

export interface AtifObservation {
  results?: AtifObservationResult[];
}

export interface AtifStepMetrics {
  prompt_tokens?: number;
  completion_tokens?: number;
  cached_tokens?: number;
  extra?: any;
}

export interface AtifStep {
  step_id: number;
  timestamp?: string;
  source: 'system' | 'user' | 'agent';
  message?: string;
  model_name?: string;
  reasoning_content?: string;
  tool_calls?: AtifToolCall[];
  observation?: AtifObservation;
  metrics?: AtifStepMetrics;
  extra?: any;
}

export interface AtifAgent {
  name: string;
  version: string;
  model_name?: string;
  tool_definitions?: any[];
  extra?: any;
}

export interface AtifFinalMetrics {
  total_prompt_tokens?: number;
  total_completion_tokens?: number;
  total_cached_tokens?: number;
  total_steps?: number;
  extra?: any;
}

export interface AtifDocument {
  schema_version: string;
  /** Optional in the schema; both AgentSight producers always set it. */
  session_id?: string;
  trajectory_id?: string;
  /** Free-form description, e.g. the opt run root's target session. */
  notes?: string;
  agent?: AtifAgent;
  steps?: AtifStep[];
  final_metrics?: AtifFinalMetrics;
  subagent_trajectories?: AtifDocument[];
  extra?: any;
}
