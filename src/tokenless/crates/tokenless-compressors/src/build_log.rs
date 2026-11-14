//! Build and test log compression for PostTool command output.
//!
//! Preserve diagnostics and unknown content while compacting recognized layout and retrievable progress.

mod classify;
mod maven;
mod pnpm;
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

struct LayoutEdit {
    start: usize,
    end: usize,
    replacement: String,
}

/// Stable operations performed inside the build-log domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildLogOperation {
    /// ANSI SGR presentation sequences were removed.
    TerminalCleanup,
    /// Recognized layout or routine progress was reduced under the recovery policy.
    Reduction,
}

impl BuildLogOperation {
    /// Returns the stable internal operation identifier.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::TerminalCleanup => "terminal-cleanup",
            Self::Reduction => "build-log-reduction",
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
        detect_format(input).0.is_some()
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
        let mut lossless_operations = cleanup_changed
            .then_some(BuildLogOperation::TerminalCleanup)
            .into_iter()
            .collect::<Vec<_>>();
        let store = stash.filter(|_| recovery.is_available());
        let (Some(mut format), sample) = detect_format(&cleaned) else {
            return lossless_outcome(cleaned, lossless_operations, Vec::new(), 0);
        };
        if !matches!(format, BuildLogFormat::Pnpm | BuildLogFormat::Maven) && store.is_none() {
            return lossless_outcome(cleaned, lossless_operations, Vec::new(), 0);
        }

        let lines = cleaned.split_inclusive('\n').collect::<Vec<_>>();
        let maven_layouts = if format == BuildLogFormat::Maven {
            maven::plan(&lines)
        } else {
            Vec::new()
        };
        let mut maven_transfers = Vec::new();
        if format == BuildLogFormat::Maven
            && maven_layouts.is_empty()
            && !maven::is_compacted(&lines)
            && store.is_some()
        {
            // No-edit Maven results retain anchored Generic recovery, plus verified
            // transfers that remain inside an active Maven scope.
            if is_anchored(&sample) {
                maven_transfers = maven::scoped_transfers(&lines);
            }
            if let Some(generic) = detect_generic(&sample) {
                format = generic;
            } else if maven_transfers.iter().filter(|&&ok| ok).count() >= GENERIC_TEMPLATE_MIN_LINES
            {
                format = BuildLogFormat::Generic;
            }
        }
        let stripped = lines
            .iter()
            .map(|line| line.trim_end_matches('\n').to_owned())
            .collect::<Vec<_>>();
        let (mut roles, run_starts, mut layouts) = if format == BuildLogFormat::Pnpm {
            let plan = pnpm::plan(&lines);
            (plan.roles, plan.run_starts, plan.layouts)
        } else if format == BuildLogFormat::Maven {
            (
                vec![LineRole::Summary; lines.len()],
                HashSet::new(),
                maven_layouts,
            )
        } else {
            let templates = dominant_generic_templates(format, &stripped);
            (
                stripped
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let role = classify(line, format, &templates);
                        if role == LineRole::Unknown
                            && maven_transfers.get(index).copied().unwrap_or(false)
                        {
                            LineRole::Routine(RoutineFamily::Generic)
                        } else {
                            role
                        }
                    })
                    .collect::<Vec<_>>(),
                HashSet::new(),
                Vec::new(),
            )
        };
        for region in trace_regions(&stripped) {
            for role in &mut roles[region] {
                *role = LineRole::Diagnostic;
            }
        }
        layouts.retain(|edit| !roles[edit.start..edit.end].contains(&LineRole::Diagnostic));
        let layout_output = render_reduced(&lines, &[], &[], recovery, &layouts);
        let lossless = if saves_both(&cleaned, &layout_output) {
            lossless_operations.push(BuildLogOperation::Reduction);
            layout_output
        } else {
            layouts.clear();
            cleaned.clone()
        };
        let Some(store) = store else {
            return lossless_outcome(lossless, lossless_operations, Vec::new(), 0);
        };
        let Some(plans) = omission_plans(&lines, &roles, &run_starts, recovery) else {
            return lossless_outcome(lossless, lossless_operations, Vec::new(), 0);
        };
        if plans.is_empty() {
            return lossless_outcome(lossless, lossless_operations, Vec::new(), 0);
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
            return lossless_outcome(lossless, lossless_operations, writes, stash_errors);
        }

        let reduced = render_reduced(&lines, &plans, &keys, recovery, &layouts);
        if !saves_both(&lossless, &reduced) {
            return lossless_outcome(lossless, lossless_operations, writes, 0);
        }

        let mut operations = lossless_operations;
        if !operations.contains(&BuildLogOperation::Reduction) {
            operations.push(BuildLogOperation::Reduction);
        }
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
    run_starts: &HashSet<usize>,
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
        while end < roles.len()
            && roles[end] == LineRole::Routine(family)
            && !run_starts.contains(&end)
        {
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
    layouts: &[LayoutEdit],
) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    let mut omissions = plans.iter().zip(keys).peekable();
    let mut layouts = layouts.iter().peekable();
    while cursor < lines.len() {
        if let Some((plan, key)) = omissions.peek()
            && plan.start == cursor
        {
            output.push_str(&render_marker(
                plan.family,
                plan.end - plan.start,
                key,
                recovery,
            ));
            cursor = plan.end;
            omissions.next();
        } else if let Some(layout) = layouts.peek()
            && layout.start == cursor
        {
            output.push_str(&layout.replacement);
            cursor = layout.end;
            layouts.next();
        } else {
            output.push_str(lines[cursor]);
            cursor += 1;
        }
    }
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

fn detect_format(input: &str) -> (Option<BuildLogFormat>, Vec<String>) {
    let mut sample = sample_lines(input);
    let maven = maven::detect(&sample);
    // Keep CR visible to Maven, whose layout grammar only handles LF records.
    // Existing detectors retain the CRLF normalization previously done by lines().
    for line in &mut sample {
        if line.ends_with('\r') {
            line.pop();
        }
    }
    if pnpm::detect(&sample) {
        return (Some(BuildLogFormat::Pnpm), sample);
    }
    if maven {
        return (Some(BuildLogFormat::Maven), sample);
    }
    let formats = [
        BuildLogFormat::Cargo,
        BuildLogFormat::Pytest,
        BuildLogFormat::Unittest,
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
    let format = winner.or_else(|| detect_generic(&sample));
    (format, sample)
}

fn is_anchored(sample: &[String]) -> bool {
    sample.iter().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("$ ")
            || trimmed.to_ascii_lowercase().starts_with("exit code:")
            || trimmed.to_ascii_lowercase().starts_with("exit status:")
    })
}

fn detect_generic(sample: &[String]) -> Option<BuildLogFormat> {
    if !is_anchored(sample) {
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
    let mut head_end = 0;
    for line in input.split_terminator('\n').take(SAMPLE_LINES_PER_END) {
        if head_bytes + line.len() > SAMPLE_BYTES_PER_END {
            break;
        }
        head_bytes += line.len();
        head_end = line.as_ptr() as usize - input.as_ptr() as usize + line.len();
        seen.insert((line.as_ptr() as usize, line.len()));
        sampled.push(clean_terminal(line));
    }

    let mut tail = Vec::new();
    let mut tail_bytes = 0;
    let mut tail_start = None;
    for line in input
        .split_terminator('\n')
        .rev()
        .take(SAMPLE_LINES_PER_END)
    {
        if tail_bytes + line.len() > SAMPLE_BYTES_PER_END {
            break;
        }
        tail_bytes += line.len();
        if !seen.contains(&(line.as_ptr() as usize, line.len())) {
            tail_start = Some(line.as_ptr() as usize - input.as_ptr() as usize);
            tail.push(clean_terminal(line));
        }
    }
    tail.reverse();
    if tail_start.is_some_and(|start| !matches!(&input[head_end..start], "\n" | "\r\n")) {
        // Multi-line detectors must not join windows or mistake a tail-only
        // sample for the document's head when its first record exceeds the budget.
        sampled.push("\0".into());
    }
    sampled.extend(tail);
    sampled
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/build_log_tests.rs");
}

#[cfg(test)]
mod pnpm_tests {
    use super::*;
    include!("tests/pnpm_tests.rs");
}
