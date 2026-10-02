//! Trajectory-level reuse labelling: decide whether a captured trajectory is
//! worth putting in front of a future agent at all.
//!
//! Three parts, kept apart on purpose:
//!
//! - [`triage`] turns a trajectory's structure plus the deterministic grounding
//!   verdicts into an automatic label. No LLM, no I/O.
//! - [`label`] holds the automatic verdict alongside the human's and decides
//!   which of the two the retrieval layer must obey.
//! - [`store`] persists both in `reuse.db`.
//!
//! [`summarize`] bridges the first of those to [`crate::grounding`], folding a
//! whole trajectory's per-round verdicts into the single summary the rules read.
//! [`api`] holds the mechanics both servers' handlers share, so a label cannot
//! come to mean one thing on Linux and another on macOS.
//!
//! Cross-platform, like [`crate::preferences`]: the rules read collected ATIF
//! trajectories, which exist on every OS, so the Linux and macOS servers share
//! one implementation. Nothing here touches eBPF.
//!
//! [`triage`] itself never reaches into the grounding engine — it takes
//! [`triage::GroundingSummary`] as an argument. That keeps the rules testable
//! without replaying a trajectory, and it is why the architecture check allows
//! this module no dependency on `server`, where causal attribution lives.

pub mod api;
pub mod judge;
pub mod label;
pub mod store;
pub mod summarize;
pub mod triage;

pub use api::{ReuseApiError, SessionLabelView, SessionsQuery, TriageQuery, TriageReport};
pub use judge::{JudgeError, JudgeResponse, JudgeVerdict};
pub use label::{ConfirmState, LabelAction, LabelEventKind, SessionLabel, TrajectoryLabel};
pub use store::{LabelEvent, LabelFilter, ReuseStore, ReuseStoreError, RuleOverrideStat};
pub use summarize::summarize_trajectory;
pub use triage::{GroundingSummary, TriageConfig, TriageMetrics, TriageOutcome};
