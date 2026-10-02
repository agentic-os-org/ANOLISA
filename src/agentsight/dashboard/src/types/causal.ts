//! Types for the causal attribution offline analysis pipeline.
//!
//! Mirrors the JSON emitted by `POST /api/causal-attribution`.

/**
 * Node role in the causal chain.
 *
 * Either a problem is asserted or none is, so there is no kind for "suspected
 * but unproven": on a round with nothing alleged the backend neutralises flagged
 * steps to `ok` instead of drawing them as suspicions.
 *
 * `failed` records that a call on that step errored. That is a fact rather than
 * an accusation, so it survives that neutralisation.
 */
export type CausalNodeKind =
  | 'ok'
  | 'seed'
  | 'root'
  | 'sym'
  | 'good'
  | 'env'
  | 'shipped'
  | 'failed'
  | 'user';

export type CausalEdgeType = 'n' | 'bad';

export type CausalAttrib = 'model' | 'skill' | 'prompt' | 'agent';

export type CausalOutcome = 'success' | 'fail';

/**
 * How strong the evidence behind the verdict is.
 *
 * `L2` is a failure tied to a fabricated value, `L3` an ungrounded claim, `L4` an
 * unsupported model opinion. Only findings allowed to drive a verdict score a
 * tier, so a finding that merely describes the round leaves it at `L4`.
 */
export type EvidenceTier = 'L2' | 'L3' | 'L4';

/** One candidate attribution the evaluator considers plausible. */
export interface AlternativeAttrib {
  attrib: CausalAttrib;
  /** 0..1 — evaluator's self-reported confidence in this candidate. */
  confidence: number;
  rationale: string;
  fix: string;
}

/** A finding the deterministic pass established, verifiable without the model. */
export interface CausalFinding {
  kind: 'ungrounded_onset' | 'failure_then_fabrication' | 'repeated_identical_failure';
  step: number;
  detail: string;
  /** Verbatim excerpt proving the finding. */
  quote?: string;
}

export interface CausalNode {
  id: string;
  step?: number;
  kind: CausalNodeKind;
  tag: string;
  foot?: string;
  plain: string;
  raw?: string;
}

export interface CausalEdge {
  a: string;
  b: string;
  type: CausalEdgeType;
}

export interface CausalContra {
  saw: string;
  said: string;
}

export interface CausalCase {
  id: string;
  title: string;
  task: string;
  session: string;
  trigger?: string;
  verdict: string;
  root_one: string;
  outcome: CausalOutcome;
  outcome_note?: string;
  turn_issue?: string;
  attrib: CausalAttrib;
  fix: string;
  alternative_attribs?: AlternativeAttrib[];
  timeline?: string[];
  nodes: CausalNode[];
  edges: CausalEdge[];
  contra?: CausalContra;
  concl?: string;
  evidence_tier: EvidenceTier;
  /** False means the verdict is a suspicion, not an established defect. */
  verdict_supported: boolean;
  /** Too many calls were unclassifiable for a root cause to be claimed. */
  needs_human_review: boolean;
  findings?: CausalFinding[];
  /** Verifiable claims examined; zero means silence proves nothing. */
  claims_checked: number;
  claims_unresolved: number;
}

export interface CausalAttributionRequest {
  session_id: string;
  round_index?: number;
  complaint: string;
  /** When true, bypass the server-side cache and rerun the LLM pipeline. */
  force?: boolean;
  /**
   * Scope hint: "conversation" means `session_id` actually carries a
   * conversation_id and the backend should attribute only that sub-conversation.
   */
  id_kind?: 'session' | 'conversation';
}

export interface CausalAttributionResponse {
  case: CausalCase;
  /** True when the case came from the server's in-memory cache. */
  cached: boolean;
}
