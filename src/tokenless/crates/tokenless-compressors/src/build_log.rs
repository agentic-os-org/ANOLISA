//! Build and test log compression for PostTool command output.
//!
//! Preserve diagnostics and unknown content while compacting recognized layout and retrievable progress.

mod build_steps;
mod classify;
mod ctest;
mod gradle;
mod maven;
mod pnpm;
mod scope;
mod template;
mod trace;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, compute_key, recovery_instruction};
use tokenless_protocol::estimate_tokens;

use crate::terminal_cleanup::clean_terminal;
use classify::{
    BuildLogFormat, Evidence, LineRole, RoutineFamily, classify, format_evidence,
    is_failure_mode_anchor,
};
use template::generic_progress_template;
use trace::trace_regions;

const SAMPLE_LINES_PER_END: usize = 100;
const SAMPLE_BYTES_PER_END: usize = 32 * 1024;
const MIN_ROUTINE_RUN_LINES: usize = 9;
const ROUTINE_EDGE_LINES: usize = 2;
// Allow up to 24 omission blocks sorted by net savings rather than aborting at 8;
// keeping top-24 bounds worst-case stash writes and recovery markers in one output.
const MAX_OMISSION_BLOCKS: usize = 24;
const GENERIC_TEMPLATE_MIN_LINES: usize = 9;

/// Execution outcome context guiding build-log omission strictness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BuildLogMode {
    /// Normal successful build or test output.
    #[default]
    Success,
    /// Non-zero exit output carrying failure diagnostics; restricts omissions to
    /// whitelisted routine families and preserves error, assertion, and caret anchors.
    DiagnosticFailure,
}

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
    ///
    /// Note that detection is mode-unaware (evaluates as `BuildLogMode::Success`);
    /// generic-only logs may return `true` here yet yield a lossless-only or unchanged
    /// outcome under `BuildLogMode::DiagnosticFailure`, where generic templates are disabled.
    #[must_use]
    pub fn detect(input: &str) -> bool {
        detect_log(input).route.is_some()
    }

    /// Builds the smallest valid build-log candidate under the recovery policy.
    #[must_use]
    pub fn compress(&self, input: &str, stash: Option<&dyn StashStore>) -> BuildLogOutcome {
        self.compress_with_options(input, stash, &RecoveryMethod::Shell, BuildLogMode::Success)
    }

    /// Measures and renders omissions using the caller's actual recovery entry point.
    #[must_use]
    pub fn compress_with_recovery(
        &self,
        input: &str,
        stash: Option<&dyn StashStore>,
        recovery: &RecoveryMethod,
    ) -> BuildLogOutcome {
        self.compress_with_options(input, stash, recovery, BuildLogMode::Success)
    }

    /// Measures and renders omissions using the caller's recovery entry point and execution mode.
    #[must_use]
    pub fn compress_with_options(
        &self,
        input: &str,
        stash: Option<&dyn StashStore>,
        recovery: &RecoveryMethod,
        mode: BuildLogMode,
    ) -> BuildLogOutcome {
        let cleaned = clean_terminal(input);
        let cleanup_changed = cleaned != input;
        let mut lossless_operations = cleanup_changed
            .then_some(BuildLogOperation::TerminalCleanup)
            .into_iter()
            .collect::<Vec<_>>();
        let store = stash.filter(|_| recovery.is_available());
        let PreparedBuildLog::Planned(plan) = prepare_build_log(&cleaned, store, false, mode)
        else {
            return lossless_outcome(cleaned, lossless_operations, Vec::new(), 0);
        };
        if plan.layout_saved {
            lossless_operations.push(BuildLogOperation::Reduction);
        }
        let Some(store) = store else {
            return lossless_outcome(plan.lossless, lossless_operations, Vec::new(), 0);
        };
        let plans = omission_plans(
            &plan.lines,
            &plan.roles,
            &plan.run_starts,
            MarkerStyle::PerGap(recovery),
        );
        if plans.is_empty() {
            return lossless_outcome(plan.lossless, lossless_operations, Vec::new(), 0);
        }

        let mut writes = Vec::with_capacity(plans.len());
        let mut keys = Vec::with_capacity(plans.len());
        let mut stash_errors = 0;
        for omission in &plans {
            match store.stash(&omission.payload) {
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
            return lossless_outcome(plan.lossless, lossless_operations, writes, stash_errors);
        }

        let reduced = render_reduced(&plan.lines, &plans, &keys, recovery, &plan.layouts);
        if !saves_both(&plan.lossless, &reduced) {
            return lossless_outcome(plan.lossless, lossless_operations, writes, 0);
        }

        let mut operations = lossless_operations;
        if !operations.contains(&BuildLogOperation::Reduction) {
            operations.push(BuildLogOperation::Reduction);
        }
        let omitted_lines = plans
            .iter()
            .map(|omission| omission.end - omission.start)
            .sum();
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

    /// Renders a compact, Stash-free view of an embedded build/test log for
    /// callers that stash the complete original value once at the outer slot.
    ///
    /// Shares Stage 0–Stage 2 detection, failure-mode anchor protection, layout
    /// compaction, and top-24 routine omission selection with
    /// [`Self::compress_with_options`], diverging only by emitting inline count
    /// notices instead of per-gap Stash markers.
    #[must_use]
    pub(crate) fn render_embedded_view(&self, input: &str, mode: BuildLogMode) -> Option<String> {
        if !input.contains('\n') {
            return None;
        }
        let cleaned = clean_terminal(input);
        match prepare_build_log(&cleaned, None, true, mode) {
            PreparedBuildLog::Unrecognized => None,
            PreparedBuildLog::TerminalCleaned => saves_both(input, &cleaned).then_some(cleaned),
            PreparedBuildLog::Planned(plan) => {
                let plans = omission_plans(
                    &plan.lines,
                    &plan.roles,
                    &plan.run_starts,
                    MarkerStyle::Embedded,
                );
                let best = if plans.is_empty() {
                    plan.lossless
                } else {
                    let reduced = render_reduced(
                        &plan.lines,
                        &plans,
                        &[],
                        &RecoveryMethod::None,
                        &plan.layouts,
                    );
                    if saves_both(&plan.lossless, &reduced) {
                        reduced
                    } else {
                        plan.lossless
                    }
                };
                saves_both(input, &best).then_some(best)
            }
        }
    }
}

enum PreparedBuildLog<'a> {
    Unrecognized,
    TerminalCleaned,
    Planned(PlannedBuildLog<'a>),
}

struct PlannedBuildLog<'a> {
    lines: Vec<&'a str>,
    layouts: Vec<LayoutEdit>,
    roles: Vec<LineRole>,
    run_starts: HashSet<usize>,
    lossless: String,
    layout_saved: bool,
}

fn prepare_build_log<'a>(
    cleaned: &'a str,
    store: Option<&dyn StashStore>,
    allow_omissions: bool,
    mode: BuildLogMode,
) -> PreparedBuildLog<'a> {
    let omissions_allowed = store.is_some() || allow_omissions;
    let detected = detect_log_with_mode(cleaned, mode);
    let Some(route) = detected.route else {
        return PreparedBuildLog::Unrecognized;
    };
    if matches!(route, DetectedRoute::Scored(_)) && !omissions_allowed {
        return PreparedBuildLog::TerminalCleaned;
    }

    let lines = cleaned.split_inclusive('\n').collect::<Vec<_>>();
    // Stage 0: preserve already-compacted scoped results as terminal on scored routes even
    // when an oversized preamble pushes the template header out of the sample window.
    // Stage 1 routes enforce their own same-dialect compacted guards without cross-dialect vetoes.
    if matches!(route, DetectedRoute::Scored(_)) && has_scoped_compacted_header(&lines) {
        return PreparedBuildLog::TerminalCleaned;
    }

    let stripped = lines
        .iter()
        .map(|line| line.trim_end_matches('\n').to_owned())
        .collect::<Vec<_>>();
    let mut diagnostic_mask = vec![false; lines.len()];
    for region in trace_regions(&stripped) {
        diagnostic_mask[region].fill(true);
    }
    if mode == BuildLogMode::DiagnosticFailure {
        for (mask, line) in diagnostic_mask.iter_mut().zip(&stripped) {
            if is_failure_mode_anchor(line.trim()) {
                *mask = true;
            }
        }
    }

    // Stage 1: lossless structural layout planning (independent of Stash availability).
    let stage1 = plan_stage1(route, &lines, &diagnostic_mask);
    let mut layouts = stage1.layouts;

    // Stage 2: residual line-role classification for optional omission.
    let (mut roles, run_starts) = build_stage2_roles(
        stage1.residual,
        &detected.sample,
        &lines,
        &stripped,
        store,
        allow_omissions,
        mode,
    );
    for (role, &is_diag) in roles.iter_mut().zip(&diagnostic_mask) {
        if is_diag {
            *role = LineRole::Diagnostic;
        }
    }
    debug_assert!(
        layouts
            .iter()
            .all(|edit| !roles[edit.start..edit.end].contains(&LineRole::Diagnostic))
    );

    let layout_output = render_reduced(&lines, &[], &[], &RecoveryMethod::None, &layouts);
    let (lossless, layout_saved) = if saves_both(cleaned, &layout_output) {
        (layout_output, true)
    } else {
        layouts.clear();
        (cleaned.to_owned(), false)
    };
    PreparedBuildLog::Planned(PlannedBuildLog {
        lines,
        layouts,
        roles,
        run_starts,
        lossless,
        layout_saved,
    })
}

#[derive(Clone, Copy)]
enum MarkerStyle<'a> {
    PerGap(&'a RecoveryMethod),
    Embedded,
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

struct ScoredOmissionPlan {
    plan: OmissionPlan,
    saved_tokens: usize,
    saved_bytes: usize,
}

fn omission_plans(
    lines: &[&str],
    roles: &[LineRole],
    run_starts: &HashSet<usize>,
    marker_style: MarkerStyle<'_>,
) -> Vec<OmissionPlan> {
    let mut scored = Vec::new();
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
            let marker = match marker_style {
                MarkerStyle::PerGap(recovery) => render_marker(
                    family,
                    omitted_end - start,
                    &compute_key(payload.as_bytes()),
                    recovery,
                ),
                MarkerStyle::Embedded => render_embedded_marker(family, omitted_end - start),
            };
            if saves_both(&payload, &marker) {
                let saved_tokens =
                    estimate_tokens(&payload).saturating_sub(estimate_tokens(&marker));
                let saved_bytes = payload.len().saturating_sub(marker.len());
                scored.push(ScoredOmissionPlan {
                    plan: OmissionPlan {
                        start,
                        end: omitted_end,
                        family,
                        payload,
                    },
                    saved_tokens,
                    saved_bytes,
                });
            }
        }
        index = end;
    }
    if scored.len() > MAX_OMISSION_BLOCKS {
        // Keep the highest-net-savings omission intervals and restore ascending
        // source order so render_reduced's single-pass cursor remains valid.
        scored.sort_by_key(|entry| {
            (
                Reverse((entry.saved_tokens, entry.saved_bytes)),
                entry.plan.start,
            )
        });
        scored.truncate(MAX_OMISSION_BLOCKS);
        scored.sort_by_key(|entry| entry.plan.start);
    }
    scored.into_iter().map(|entry| entry.plan).collect()
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
    let mut omissions = plans.iter().enumerate().peekable();
    let mut layouts = layouts.iter().peekable();
    while cursor < lines.len() {
        if let Some(&(idx, plan)) = omissions.peek()
            && plan.start == cursor
        {
            let omitted_count = plan.end - plan.start;
            match keys.get(idx) {
                Some(key) => {
                    output.push_str(&render_marker(plan.family, omitted_count, key, recovery));
                }
                None => {
                    output.push_str(&render_embedded_marker(plan.family, omitted_count));
                }
            }
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

fn render_embedded_marker(family: RoutineFamily, omitted_lines: usize) -> String {
    format!("{omitted_lines} {} lines omitted.\n", family.label())
}

#[derive(Debug, Clone, Copy)]
enum Stage1Selection {
    Pnpm,
    Maven,
    Gradle,
    Ctest,
    BuildSteps,
}

#[derive(Debug, Clone, Copy)]
enum DetectedRoute {
    Stage1(Stage1Selection),
    Scored(BuildLogFormat),
}

struct DetectedLog {
    route: Option<DetectedRoute>,
    sample: Vec<String>,
}

enum ResidualPolicy {
    Provided {
        roles: Vec<LineRole>,
        run_starts: HashSet<usize>,
    },
    KeepResidual,
    MavenTransfersOrGeneric,
    AnchoredGeneric,
    ScopedMixed {
        is_protected_step: fn(&str) -> bool,
        protected_ranges: Vec<Range<usize>>,
    },
    Scored(BuildLogFormat),
}

struct Stage1Plan {
    layouts: Vec<LayoutEdit>,
    residual: ResidualPolicy,
}

fn has_scoped_compacted_header(lines: &[&str]) -> bool {
    scope::is_any_compacted(
        lines,
        &[
            gradle::HEADER,
            ctest::HEADER,
            build_steps::CMAKE_HEADER,
            build_steps::COMPILE_HEADER,
        ],
    )
}

fn overlaps_diagnostic(diagnostic_mask: &[bool], range: Range<usize>) -> bool {
    diagnostic_mask[range].iter().any(|&is_diag| is_diag)
}

fn plan_stage1(route: DetectedRoute, lines: &[&str], diagnostic_mask: &[bool]) -> Stage1Plan {
    match route {
        DetectedRoute::Stage1(Stage1Selection::Pnpm) => {
            let plan = pnpm::plan(lines);
            let mut layouts = plan.layouts;
            layouts.retain(|edit| !overlaps_diagnostic(diagnostic_mask, edit.start..edit.end));
            Stage1Plan {
                layouts,
                residual: ResidualPolicy::Provided {
                    roles: plan.roles,
                    run_starts: plan.run_starts,
                },
            }
        }
        DetectedRoute::Stage1(Stage1Selection::Maven) => {
            let mut layouts = maven::plan(lines);
            // Preserve pre-trace empty-fallback parity before filtering diagnostic overlaps.
            let residual = if layouts.is_empty() {
                ResidualPolicy::MavenTransfersOrGeneric
            } else {
                ResidualPolicy::KeepResidual
            };
            layouts.retain(|edit| !overlaps_diagnostic(diagnostic_mask, edit.start..edit.end));
            Stage1Plan { layouts, residual }
        }
        DetectedRoute::Stage1(Stage1Selection::Gradle | Stage1Selection::Ctest) => {
            let is_gradle = matches!(route, DetectedRoute::Stage1(Stage1Selection::Gradle));
            let mut layouts = if is_gradle {
                gradle::plan(lines)
            } else {
                ctest::plan(lines)
            };
            // Preserve pre-trace empty-fallback parity before filtering diagnostic overlaps,
            // keeping same-dialect headers conservative so clipped blocks stay out of Stash.
            let is_same_dialect_compacted = layouts.is_empty()
                && if is_gradle {
                    gradle::is_compacted(lines)
                } else {
                    ctest::is_compacted(lines)
                };
            let residual = if !layouts.is_empty() || is_same_dialect_compacted {
                ResidualPolicy::KeepResidual
            } else {
                ResidualPolicy::AnchoredGeneric
            };
            layouts.retain(|edit| !overlaps_diagnostic(diagnostic_mask, edit.start..edit.end));
            Stage1Plan { layouts, residual }
        }
        DetectedRoute::Stage1(Stage1Selection::BuildSteps) => {
            let raw_layouts = build_steps::plan(lines);
            let protected_ranges = raw_layouts
                .iter()
                .map(|edit| edit.start..edit.end)
                .collect::<Vec<_>>();
            let layouts = raw_layouts
                .into_iter()
                .filter(|edit| !overlaps_diagnostic(diagnostic_mask, edit.start..edit.end))
                .collect::<Vec<_>>();
            let residual = if layouts.is_empty() && build_steps::is_compacted(lines) {
                ResidualPolicy::KeepResidual
            } else {
                ResidualPolicy::ScopedMixed {
                    is_protected_step: build_steps::is_protected_step,
                    protected_ranges,
                }
            };
            Stage1Plan { layouts, residual }
        }
        DetectedRoute::Scored(format) => Stage1Plan {
            layouts: Vec::new(),
            residual: ResidualPolicy::Scored(format),
        },
    }
}

fn classify_scored_lines(
    format: BuildLogFormat,
    stripped: &[String],
    maven_transfers: &[bool],
    mode: BuildLogMode,
) -> Vec<LineRole> {
    let templates = dominant_generic_templates(format, stripped);
    stripped
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let role = classify(line, format, &templates, mode);
            if role == LineRole::Unknown && maven_transfers.get(index).copied().unwrap_or(false) {
                LineRole::Routine(RoutineFamily::Generic)
            } else {
                role
            }
        })
        .collect()
}

fn scoped_fallback_roles(
    stripped: &[String],
    sample: &[String],
    protected_ranges: &[Range<usize>],
    store: Option<&dyn StashStore>,
    allow_omissions: bool,
    is_protected_step: fn(&str) -> bool,
    mode: BuildLogMode,
) -> Vec<LineRole> {
    let omissions_allowed = store.is_some() || allow_omissions;
    let sub_format = if omissions_allowed {
        detect_scored_format(sample).or_else(|| {
            (mode == BuildLogMode::Success)
                .then(|| detect_generic(sample))
                .flatten()
        })
    } else {
        None
    };
    let Some(sub_format) = sub_format else {
        return vec![LineRole::Summary; stripped.len()];
    };
    let templates = dominant_generic_templates(sub_format, stripped);
    let mut open_fence = None;
    // Mirror scope::enter_line's fence and non-TAB control boundaries (including
    // mixed-ending '\r') plus protected counter steps that Stage 1 cannot compact.
    let mut roles = stripped
        .iter()
        .map(|line| {
            if scope::step_fence(&mut open_fence, line)
                || line.chars().any(|ch| ch != '\t' && ch.is_control())
                || is_protected_step(line)
            {
                LineRole::Summary
            } else {
                classify(line, sub_format, &templates, mode)
            }
        })
        .collect::<Vec<_>>();
    for range in protected_ranges {
        roles[range.clone()].fill(LineRole::Summary);
    }
    roles
}

fn build_stage2_roles(
    residual: ResidualPolicy,
    sample: &[String],
    lines: &[&str],
    stripped: &[String],
    store: Option<&dyn StashStore>,
    allow_omissions: bool,
    mode: BuildLogMode,
) -> (Vec<LineRole>, HashSet<usize>) {
    let omissions_allowed = store.is_some() || allow_omissions;
    match residual {
        ResidualPolicy::Provided { roles, run_starts } => (roles, run_starts),
        ResidualPolicy::KeepResidual => (vec![LineRole::Summary; lines.len()], HashSet::new()),
        ResidualPolicy::MavenTransfersOrGeneric => {
            if maven::is_compacted(lines)
                || !omissions_allowed
                || mode == BuildLogMode::DiagnosticFailure
            {
                return (vec![LineRole::Summary; lines.len()], HashSet::new());
            }
            // No-edit Maven results retain anchored Generic recovery, plus verified
            // transfers that remain inside an active Maven scope.
            let maven_transfers = if is_anchored(sample) {
                maven::scoped_transfers(lines)
            } else {
                Vec::new()
            };
            let generic = detect_generic(sample).or_else(|| {
                (maven_transfers.iter().filter(|&&ok| ok).count() >= GENERIC_TEMPLATE_MIN_LINES)
                    .then_some(BuildLogFormat::Generic)
            });
            let roles = match generic {
                Some(format) => classify_scored_lines(format, stripped, &maven_transfers, mode),
                None => vec![LineRole::Summary; lines.len()],
            };
            (roles, HashSet::new())
        }
        ResidualPolicy::AnchoredGeneric => {
            if mode == BuildLogMode::Success
                && omissions_allowed
                && let Some(generic) = detect_generic(sample)
            {
                return (
                    classify_scored_lines(generic, stripped, &[], mode),
                    HashSet::new(),
                );
            }
            (vec![LineRole::Summary; lines.len()], HashSet::new())
        }
        ResidualPolicy::ScopedMixed {
            is_protected_step,
            protected_ranges,
        } => (
            scoped_fallback_roles(
                stripped,
                sample,
                &protected_ranges,
                store,
                allow_omissions,
                is_protected_step,
                mode,
            ),
            HashSet::new(),
        ),
        ResidualPolicy::Scored(format) => (
            classify_scored_lines(format, stripped, &[], mode),
            HashSet::new(),
        ),
    }
}

fn detect_log(input: &str) -> DetectedLog {
    detect_log_with_mode(input, BuildLogMode::Success)
}

fn detect_log_with_mode(input: &str, mode: BuildLogMode) -> DetectedLog {
    let mut sample = sample_lines(input);
    let maven = maven::detect(&sample);
    let gradle = gradle::detect(&sample);
    let ctest = ctest::detect(&sample);
    let build_steps = build_steps::detect(&sample);
    // Keep CR visible to Maven, Gradle, CTest, and BuildSteps, whose layout grammars only handle LF records.
    // Existing detectors retain the CRLF normalization previously done by lines().
    for line in &mut sample {
        if line.ends_with('\r') {
            line.pop();
        }
    }
    let route = if pnpm::detect(&sample) {
        Some(DetectedRoute::Stage1(Stage1Selection::Pnpm))
    } else if maven {
        Some(DetectedRoute::Stage1(Stage1Selection::Maven))
    } else if gradle {
        Some(DetectedRoute::Stage1(Stage1Selection::Gradle))
    } else if ctest {
        Some(DetectedRoute::Stage1(Stage1Selection::Ctest))
    } else if build_steps {
        Some(DetectedRoute::Stage1(Stage1Selection::BuildSteps))
    } else {
        detect_scored_format(&sample)
            .or_else(|| {
                (mode == BuildLogMode::Success)
                    .then(|| detect_generic(&sample))
                    .flatten()
            })
            .map(DetectedRoute::Scored)
    };
    DetectedLog { route, sample }
}

fn detect_scored_format(sample: &[String]) -> Option<BuildLogFormat> {
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
    winner
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
#[path = "../tests/common/literal_oracle.rs"]
mod oracle;

#[cfg(test)]
mod tests;
