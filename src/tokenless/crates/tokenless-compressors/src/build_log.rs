//! Build and test log compression for PostTool command output.
//!
//! The compressor keeps diagnostics, summaries, phase boundaries, stack
//! traces, and unknown lines. Only recognized runs of routine progress are
//! reduced, with every omission backed by an in-place retrieval marker.

mod classify;
mod template;
mod trace;

use std::collections::{HashMap, HashSet};

use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, compute_key, recovery_instruction};
use tokenless_protocol::estimate_tokens;

use crate::terminal_cleanup::clean_terminal;
use classify::{BuildLogFormat, Evidence, LineRole, RoutineFamily, classify, format_evidence};
use template::generic_progress_template;
use trace::trace_regions;

const SAMPLE_LINES_PER_END: usize = 100;
const SAMPLE_BYTES_PER_END: usize = 32 * 1024;
const MIN_ROUTINE_RUN_LINES: usize = 9;
const ROUTINE_EDGE_LINES: usize = 2;
const MAX_OMISSION_BLOCKS: usize = 8;
const GENERIC_TEMPLATE_MIN_LINES: usize = 9;

/// Stable operations performed inside the build-log domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildLogOperation {
    /// ANSI SGR presentation sequences were removed.
    TerminalCleanup,
    /// Repeated routine progress was replaced by retrievable markers.
    ProgressReduction,
}

impl BuildLogOperation {
    /// Returns the stable internal operation identifier.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::TerminalCleanup => "terminal-cleanup",
            Self::ProgressReduction => "build-log-progress-reduction",
        }
    }
}

/// Observability produced during one build-log compression attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildLogMetrics {
    /// Failed Stash writes while producing the retrievable candidate.
    pub stash_errors: usize,
    /// Omission blocks in the selected candidate.
    pub omitted_blocks: usize,
    /// Routine lines omitted from the selected candidate.
    pub omitted_lines: usize,
}

/// Complete result of one build-log compression attempt.
#[derive(Debug)]
pub struct BuildLogOutcome {
    /// Candidate selected inside the build-log domain.
    pub output: String,
    /// Operations that shaped `output`, in execution order.
    pub operations: Vec<BuildLogOperation>,
    /// Recovery state of `output` relative to the input.
    pub recoverability: crate::Recoverability,
    /// Every tentative write performed while producing candidates.
    pub stash_writes: Vec<StashWrite>,
    /// Metrics associated with the attempt and selected candidate.
    pub metrics: BuildLogMetrics,
}

/// Compresses compiler, package-manager, and test-runner output.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildLogCompressor;

impl BuildLogCompressor {
    /// Detects a supported build/test log using a bounded head-and-tail sample.
    #[must_use]
    pub fn detect(input: &str) -> bool {
        detect_format(input).is_some()
    }

    /// Builds the smallest valid build-log candidate under the recovery policy.
    #[must_use]
    pub fn compress(&self, input: &str, stash: Option<&dyn StashStore>) -> BuildLogOutcome {
        self.compress_with_recovery(input, stash, &RecoveryMethod::Shell)
    }

    /// Measures and renders omissions using the caller's actual recovery entry point.
    #[must_use]
    pub fn compress_with_recovery(
        &self,
        input: &str,
        stash: Option<&dyn StashStore>,
        recovery: &RecoveryMethod,
    ) -> BuildLogOutcome {
        let cleaned = clean_terminal(input);
        let cleanup_changed = cleaned != input;
        let cleanup_operations = cleanup_changed
            .then_some(BuildLogOperation::TerminalCleanup)
            .into_iter()
            .collect::<Vec<_>>();

        let Some(store) = stash.filter(|_| recovery.is_available()) else {
            return lossless_outcome(cleaned, cleanup_operations, Vec::new(), 0);
        };
        let Some(format) = detect_format(&cleaned) else {
            return lossless_outcome(cleaned, cleanup_operations, Vec::new(), 0);
        };

        let lines = cleaned.split_inclusive('\n').collect::<Vec<_>>();
        let stripped = lines
            .iter()
            .map(|line| line.trim_end_matches('\n').to_owned())
            .collect::<Vec<_>>();
        let generic_templates = dominant_generic_templates(format, &stripped);
        let mut roles = stripped
            .iter()
            .map(|line| classify(line, format, &generic_templates))
            .collect::<Vec<_>>();
        for region in trace_regions(&stripped) {
            for role in &mut roles[region] {
                *role = LineRole::Diagnostic;
            }
        }

        let Some(plans) = omission_plans(&lines, &roles, recovery) else {
            return lossless_outcome(cleaned, cleanup_operations, Vec::new(), 0);
        };
        if plans.is_empty() {
            return lossless_outcome(cleaned, cleanup_operations, Vec::new(), 0);
        }

        let mut writes = Vec::with_capacity(plans.len());
        let mut keys = Vec::with_capacity(plans.len());
        let mut stash_errors = 0;
        for plan in &plans {
            match store.stash(&plan.payload) {
                Ok(write) => {
                    keys.push(write.key.clone());
                    writes.push(write);
                }
                Err(_) => {
                    stash_errors += 1;
                    break;
                }
            }
        }
        if stash_errors > 0 {
            return lossless_outcome(cleaned, cleanup_operations, writes, stash_errors);
        }

        let reduced = render_reduced(&lines, &plans, &keys, recovery);
        if !saves_both(&cleaned, &reduced) {
            return lossless_outcome(cleaned, cleanup_operations, writes, 0);
        }

        let mut operations = cleanup_operations;
        operations.push(BuildLogOperation::ProgressReduction);
        let omitted_lines = plans.iter().map(|plan| plan.end - plan.start).sum();
        BuildLogOutcome {
            output: reduced,
            operations,
            recoverability: crate::Recoverability::Retrievable,
            stash_writes: writes,
            metrics: BuildLogMetrics {
                stash_errors: 0,
                omitted_blocks: plans.len(),
                omitted_lines,
            },
        }
    }
}

struct OmissionPlan {
    start: usize,
    end: usize,
    family: RoutineFamily,
    payload: String,
}

fn lossless_outcome(
    output: String,
    operations: Vec<BuildLogOperation>,
    stash_writes: Vec<StashWrite>,
    stash_errors: usize,
) -> BuildLogOutcome {
    BuildLogOutcome {
        output,
        operations,
        recoverability: crate::Recoverability::Lossless,
        stash_writes,
        metrics: BuildLogMetrics {
            stash_errors,
            ..BuildLogMetrics::default()
        },
    }
}

fn saves_both(original: &str, candidate: &str) -> bool {
    candidate.chars().count() < original.chars().count()
        && estimate_tokens(candidate) < estimate_tokens(original)
}

fn omission_plans(
    lines: &[&str],
    roles: &[LineRole],
    recovery: &RecoveryMethod,
) -> Option<Vec<OmissionPlan>> {
    let mut plans = Vec::new();
    let mut index = 0;
    while index < roles.len() {
        let LineRole::Routine(family) = roles[index] else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < roles.len() && roles[end] == LineRole::Routine(family) {
            end += 1;
        }
        if end - index >= MIN_ROUTINE_RUN_LINES {
            let start = index + ROUTINE_EDGE_LINES;
            let omitted_end = end - ROUTINE_EDGE_LINES;
            let payload = lines[start..omitted_end].concat();
            let marker = render_marker(
                family,
                omitted_end - start,
                &compute_key(payload.as_bytes()),
                recovery,
            );
            if saves_both(&payload, &marker) {
                plans.push(OmissionPlan {
                    start,
                    end: omitted_end,
                    family,
                    payload,
                });
                if plans.len() > MAX_OMISSION_BLOCKS {
                    return None;
                }
            }
        }
        index = end;
    }
    Some(plans)
}

fn render_reduced(
    lines: &[&str],
    plans: &[OmissionPlan],
    keys: &[String],
    recovery: &RecoveryMethod,
) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    for (plan, key) in plans.iter().zip(keys) {
        output.push_str(&lines[cursor..plan.start].concat());
        output.push_str(&render_marker(
            plan.family,
            plan.end - plan.start,
            key,
            recovery,
        ));
        cursor = plan.end;
    }
    output.push_str(&lines[cursor..].concat());
    output
}

fn render_marker(
    family: RoutineFamily,
    omitted_lines: usize,
    key: &str,
    recovery: &RecoveryMethod,
) -> String {
    format!(
        "{omitted_lines} {} lines omitted. {}\n",
        family.label(),
        recovery_instruction(key, recovery),
    )
}

fn detect_format(input: &str) -> Option<BuildLogFormat> {
    let sample = sample_lines(input);
    let formats = [
        BuildLogFormat::Cargo,
        BuildLogFormat::Pytest,
        BuildLogFormat::Npm,
        BuildLogFormat::Jest,
        BuildLogFormat::Go,
        BuildLogFormat::Make,
    ];
    let mut winner = None;
    let mut winner_score = 0;
    for format in formats {
        let Evidence { strong, weak } = sample.iter().fold(Evidence::default(), |mut sum, line| {
            let evidence = format_evidence(format, line);
            sum.strong += evidence.strong;
            sum.weak += evidence.weak;
            sum
        });
        if strong == 0 && weak < 3 {
            continue;
        }
        let score = strong * 1_000 + weak;
        if score > winner_score {
            winner = Some(format);
            winner_score = score;
        }
    }
    winner.or_else(|| detect_generic(&sample))
}

fn detect_generic(sample: &[String]) -> Option<BuildLogFormat> {
    let anchored = sample.iter().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("$ ")
            || trimmed.to_ascii_lowercase().starts_with("exit code:")
            || trimmed.to_ascii_lowercase().starts_with("exit status:")
    });
    if !anchored {
        return None;
    }
    let mut counts = HashMap::new();
    for line in sample {
        if let Some(template) = generic_progress_template(line) {
            *counts.entry(template).or_insert(0usize) += 1;
        }
    }
    counts
        .values()
        .any(|count| *count >= GENERIC_TEMPLATE_MIN_LINES)
        .then_some(BuildLogFormat::Generic)
}

fn dominant_generic_templates(format: BuildLogFormat, lines: &[String]) -> HashSet<String> {
    if format != BuildLogFormat::Generic {
        return HashSet::new();
    }
    let mut counts = HashMap::new();
    for line in lines {
        if let Some(template) = generic_progress_template(line) {
            *counts.entry(template).or_insert(0usize) += 1;
        }
    }
    counts
        .into_iter()
        .filter_map(|(template, count)| (count >= GENERIC_TEMPLATE_MIN_LINES).then_some(template))
        .collect()
}

fn sample_lines(input: &str) -> Vec<String> {
    let mut sampled = Vec::new();
    let mut seen = HashSet::new();
    let mut head_bytes = 0;
    for line in input.lines().take(SAMPLE_LINES_PER_END) {
        if head_bytes + line.len() > SAMPLE_BYTES_PER_END {
            break;
        }
        head_bytes += line.len();
        seen.insert((line.as_ptr() as usize, line.len()));
        sampled.push(clean_terminal(line));
    }

    let mut tail = Vec::new();
    let mut tail_bytes = 0;
    for line in input.lines().rev().take(SAMPLE_LINES_PER_END) {
        if tail_bytes + line.len() > SAMPLE_BYTES_PER_END {
            break;
        }
        tail_bytes += line.len();
        if !seen.contains(&(line.as_ptr() as usize, line.len())) {
            tail.push(clean_terminal(line));
        }
    }
    tail.reverse();
    sampled.extend(tail);
    sampled
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/build_log_tests.rs");
}
