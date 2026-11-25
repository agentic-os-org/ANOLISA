//! PostTool lifecycle service and its Runtime-owned compression pipeline.

mod arbitration;
mod content;
mod diff;
mod fact_guard;
pub(crate) mod file_read;
mod mixed;
mod pipeline;
mod stash_ledger;

#[cfg(test)]
pub(crate) use fact_guard::FactViolation;
pub(crate) use pipeline::{DEFAULT_ERROR_MIN_CHARS, PostToolPipeline, PostToolPipelineConfig};
