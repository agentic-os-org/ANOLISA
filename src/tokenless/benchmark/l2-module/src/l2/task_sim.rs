// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Scripted task simulations: replay fixed sample sequences on both sides.
//!
//! Per-category rates hide how savings compose over a real session, so three
//! scripted tasks (bug-hunt, api-debug, review) chain samples the way an
//! agent would encounter them. Replay reuses per-sample statistics already
//! collected during category runs — no new executions — so task numbers are
//! consistent with the category matrix by construction.

use crate::l2::Category;
use std::collections::HashMap;

/// One interaction in a scripted task.
#[derive(Debug, Clone, Copy)]
pub struct TaskStep {
    pub category: Category,
    /// Sample id from the category runs this step replays.
    pub sample_id: &'static str,
}

/// A scripted task: a fixed, ordered sample sequence.
#[derive(Debug, Clone, Copy)]
pub struct TaskDef {
    pub name: &'static str,
    pub steps: &'static [TaskStep],
    /// Methodology caveats (proxy steps etc.) surfaced in the report.
    pub notes: &'static str,
}

/// The three scripted tasks.
///
/// The repository has no deterministic test/log commands to capture, so
/// bug-hunt's "run tests" step is proxied by `git show --stat` and
/// api-debug's "read logs" step by re-reading the main tool response (its
/// `logs` array plays the log payload) — both proxies are declared in
/// `notes` so report readers can discount them.
pub fn tasks() -> [TaskDef; 3] {
    const BUG_HUNT: &[TaskStep] = &[
        TaskStep {
            category: Category::Grep,
            sample_id: "rg_fn_main",
        },
        TaskStep {
            category: Category::Code,
            sample_id: "rust_marker_impl",
        },
        TaskStep {
            category: Category::Diff,
            sample_id: "git_diff_tokenless",
        },
        TaskStep {
            category: Category::Command,
            sample_id: "git_show_stat",
        },
    ];
    const API_DEBUG: &[TaskStep] = &[
        TaskStep {
            category: Category::Json,
            sample_id: "tool_response_main",
        },
        TaskStep {
            category: Category::Json,
            sample_id: "deep_nested",
        },
        TaskStep {
            category: Category::Json,
            sample_id: "null_dense",
        },
        TaskStep {
            category: Category::Json,
            sample_id: "tool_response_main",
        },
    ];
    const REVIEW: &[TaskStep] = &[
        TaskStep {
            category: Category::Diff,
            sample_id: "git_diff_tokenless",
        },
        TaskStep {
            category: Category::Code,
            sample_id: "rust_key",
        },
        TaskStep {
            category: Category::Code,
            sample_id: "py_gen_fixtures_data",
        },
    ];
    [
        TaskDef {
            name: "bug-hunt",
            steps: BUG_HUNT,
            notes: "final step proxies a test run with `git show --stat` (no deterministic test command exists)",
        },
        TaskDef {
            name: "api-debug",
            steps: API_DEBUG,
            notes: "final step proxies log reading by revisiting tool_response_main (its logs array is the payload)",
        },
        TaskDef {
            name: "review",
            steps: REVIEW,
            notes: "",
        },
    ]
}

/// Mean per-sample statistics for one side, from the category runs.
#[derive(Debug, Clone, Copy)]
pub struct SampleSideStats {
    /// Mean o200k tokens of the original payload.
    pub tokens_before: f64,
    /// Mean o200k tokens of the compressed payload.
    pub tokens_after: f64,
    /// Mean latency in seconds (basis inherited from the side).
    pub latency_s: f64,
}

/// Lookup table: `(category name, sample id) → stats` for one side.
pub type SideLookup = HashMap<(String, String), SampleSideStats>;

/// One side's replay totals for one task.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskSideReplay {
    /// Steps in the script.
    pub interactions: usize,
    /// Steps with collected stats; fewer than `interactions` means the side
    /// was degraded for some category and the totals under-count it.
    pub covered_steps: usize,
    /// Sum of mean original tokens over covered steps.
    pub tokens_before_total: f64,
    /// Sum of mean compressed tokens over covered steps.
    pub tokens_after_total: f64,
    /// Absolute token savings.
    pub tokens_saved: f64,
    /// `1 - after/before` over the totals; 0.0 when nothing was covered.
    pub saving_rate: f64,
    /// Total compression time in seconds over covered steps.
    pub compression_time_s: f64,
}

/// Replays `task` against one side's per-sample statistics.
///
/// Steps missing from `lookup` (degraded side, skipped category) are
/// counted in `interactions` but not in the token totals; `covered_steps`
/// makes the gap explicit instead of silently deflating totals.
pub fn replay(task: &TaskDef, lookup: &SideLookup) -> TaskSideReplay {
    let mut covered = 0usize;
    let mut before = 0.0f64;
    let mut after = 0.0f64;
    let mut time_s = 0.0f64;
    for step in task.steps {
        let key = (step.category.name().to_string(), step.sample_id.to_string());
        if let Some(stats) = lookup.get(&key) {
            covered += 1;
            before += stats.tokens_before;
            after += stats.tokens_after;
            time_s += stats.latency_s;
        }
    }
    let saving_rate = if before > 0.0 {
        1.0 - after / before
    } else {
        0.0
    };
    TaskSideReplay {
        interactions: task.steps.len(),
        covered_steps: covered,
        tokens_before_total: before,
        tokens_after_total: after,
        tokens_saved: before - after,
        saving_rate,
        compression_time_s: time_s,
    }
}
