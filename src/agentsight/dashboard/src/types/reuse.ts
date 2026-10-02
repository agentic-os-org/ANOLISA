/**
 * Trajectory reuse labels.
 *
 * Three sources decide a label and they do not carry equal weight: a person
 * outranks the model, the model outranks the rules. `effectiveLabel` is that
 * decision already made — never re-derive it in a component, or two views will
 * eventually disagree about which label is in force.
 */

/** Verdict on whether a trajectory is worth putting in front of a future agent. */
export type TrajectoryLabel = 'good' | 'bad' | 'useless' | 'unknown';

/** Whether a person has spoken about this label. */
export type ConfirmState = 'unconfirmed' | 'confirmed' | 'overridden';

/** Labels a person may assign. `unknown` is missing on purpose: it means the
 * rules could not tell, which is not a claim someone who read the trajectory
 * would be making. */
export const ASSIGNABLE_LABELS: TrajectoryLabel[] = ['good', 'bad', 'useless'];

/** One row of the label list. */
export interface SessionLabelView {
  session_id: string;
  /** The opening request — the closest thing a captured conversation has to a
   * title. Absent for tool-only transcripts, where the id is shown instead. */
  title: string | null;
  /** Working directory the session ran in. */
  project: string;
  /** Which product wrote it: `qoder`, `claude-code`, `codex`. */
  source: string;
  agent_name: string;
  started_at: string | null;
  is_subagent: boolean;
  /** What the deterministic rules concluded. Kept visible next to the human
   * verdict because comparing the two is how rule misfires get measured. */
  auto_label: TrajectoryLabel;
  auto_reason: string;
  /** Rule identifiers that contributed, e.g. `R3`, `ungrounded_onset`. */
  auto_rules: string[];
  human_label: TrajectoryLabel | null;
  human_reason: string | null;
  /** The model judge's verdict, absent until second-level labelling runs. */
  llm_label?: TrajectoryLabel | null;
  llm_reason?: string | null;
  /** Steps the model's verdict rests on. Required for `bad`. */
  llm_cited_steps?: number[];
  /** The model said `bad` without citing a step and was reduced to `unknown`. */
  llm_downgraded?: boolean;
  confirm_state: ConfirmState;
  decided_by: string | null;
  /** The label downstream consumers obey, precedence already applied. */
  effective_label: TrajectoryLabel;
  /** Whether a person actually signed off, as opposed to merely not objecting. */
  human_backed: boolean;
  /** The rules changed their mind after a person decided. Worth surfacing so a
   * stale decision can be revisited, but it never overrides one. */
  auto_changed_since_decision: boolean;
  n_steps: number;
  n_user_turns: number;
  n_tool_calls: number;
  max_agent_len: number;
  n_findings: number;
  triage_version: string;
  updated_at_ns: number;
}

export interface SessionsResponse {
  count: number;
  sessions: SessionLabelView[];
}

/** Outcome of one deterministic labelling run. */
export interface TriageReport {
  examined: number;
  labelled: number;
  /** Same content under the same rules, so nothing was recomputed. */
  unchanged: number;
  unparsable: number;
  missing: number;
  auto_good: number;
  auto_bad: number;
  auto_useless: number;
  auto_unknown: number;
  /** Rows where a person's verdict differs from the rules' current one. */
  human_overrides_in_force: number;
  triage_version: string;
  truncated: boolean;
}

/** Outcome of one second-level judging run. */
export interface JudgeReport {
  examined: number;
  judged: number;
  skipped: number;
  failed: number;
  /** Verdicts reduced to `unknown` for citing no step. A high number means the
   * model is reaching for `bad` without evidence and being caught at it. */
  downgraded: number;
  judged_good: number;
  judged_bad: number;
  judged_unclear: number;
}

/** How often a person accepted or overturned a verdict each rule contributed to. */
export interface RuleOverrideStat {
  rule: string;
  confirmed: number;
  overridden: number;
}

export interface LabelStatsResponse {
  count: number;
  rules: RuleOverrideStat[];
}
