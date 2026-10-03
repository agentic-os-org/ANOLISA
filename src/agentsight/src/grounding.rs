//! Deterministic grounding engine: what can be established about a trajectory
//! without asking a model.
//!
//! Runs before any LLM call and answers the questions that do not need
//! semantic understanding: did a tool call fail, and can a factual claim be
//! traced to an upstream observation. The LLM is then asked only about the
//! residue, which keeps a trivially-correct trajectory from being flagged at
//! all.
//!
//! Rules come from `attribution_decision_spec.md`, derived from a full scan of
//! 594 collected trajectories (14,852 steps, 12,368 tool results) rather than
//! from guesswork.
//!
//! Cross-platform and free of `crate::` dependencies: it reads ATIF documents
//! and nothing else. Causal attribution was its first consumer and
//! [`crate::reuse`] is the second, so it sits here rather than under `server`,
//! where its location alone would have made it Linux-only.
//!
//! Terminology: a *claim* here is a factual assertion an agent made, unrelated
//! to LLM tokens.

pub mod claims;
pub mod evidence;
pub mod outcome;
