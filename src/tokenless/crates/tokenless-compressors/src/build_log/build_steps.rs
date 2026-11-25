//! Unified lossless structural compaction for CMake, Ninja, and C/C++ compile logs.
//!
//! Replaces separate `cmake_build` and `cc_compile` passes with a single Stage 1
//! planner (`detect`, `is_compacted`, `plan`, and `is_protected_step`) that keeps
//! diagnostics, failures, handwritten short Makefiles, and unknown output verbatim.

mod grammar;
mod render;
mod segment;

pub(super) const CMAKE_HEADER: &str =
    "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";
pub(super) const COMPILE_HEADER: &str =
    "C/C++ compile templates (id<TAB>fields; interleave with literals): ";

use super::LayoutEdit;
use super::scope;

/// Detects a supported CMake, Ninja, or verbose C/C++ build-step stream from a bounded sample.
pub(super) fn detect(sample: &[String]) -> bool {
    let head = match sample.iter().position(|line| line == "\0") {
        Some(gap) => &sample[..gap],
        None => sample,
    };
    let head_start = scope::skip_leading_preambles(head, grammar::is_static_preamble);
    let allow_compacted_only = head[head_start..]
        .iter()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| grammar::is_strict_log_head(line));
    scope::detect_sampled(
        sample,
        |head| scope::skip_leading_preambles(head, grammar::is_static_preamble),
        grammar::is_log_head,
        |run| grammar::has_strong_evidence(run, allow_compacted_only),
    )
}

/// Keeps already-compacted CMake/Ninja or C/C++ compile outputs terminal outside fences.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    scope::is_any_compacted(lines, &[CMAKE_HEADER, COMPILE_HEADER])
}

/// Returns whether a residual line is a counter step outside unified template coverage.
pub(super) fn is_protected_step(line: &str) -> bool {
    grammar::is_protected_step(line)
}

/// Plans atomic lossless template edits across contiguous build-progress and compile blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    segment::plan(lines)
}

#[cfg(test)]
mod tests;
