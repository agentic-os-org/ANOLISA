//! Brokered approval, governed execution, and non-replayable Runtime dispatch.

use super::*;

// Brokered phases share private fencing helpers; keep file ownership separate
// without widening those helpers across sibling module boundaries.
include!("brokered/model.rs");
include!("brokered/approval.rs");
include!("brokered/execution.rs");
include!("brokered/dispatch.rs");
include!("brokered/recovery.rs");
include!("brokered/support.rs");

#[cfg(test)]
mod tests;
