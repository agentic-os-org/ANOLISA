//! Performance analyzer — pure computation + LLM bottleneck identification.
//!
//! Pure computation: walks ATIF agent steps, derives the wall-clock split
//! (model / tool / idle) from the step timing model (see [`crate::atif`]),
//! top-N slowest calls, and idle gaps. Also extracts structured perf-issue
//! candidates for the LLM to judge.
//!
//! LLM layer (`llm` module): sends candidates to the LLM for bottleneck
//! judgment, then joins verdicts back to produce the final PerfReport.

pub mod llm;
mod prompts;

use anyhow::Result;

use crate::atif::AtifTrajectory;
use crate::trace::{collect_tool_calls_with, tool_window_secs};
use crate::types::{CacheTurn, IdleGap, PerfCandidateSet, PerfStats, ToolAggStats, ToolCallRecord};

/// Default threshold (seconds) for detecting idle gaps between turns.
const IDLE_GAP_THRESHOLD_SECS: f64 = 60.0;

/// Number of top slowest calls to include in the report.
const TOP_SLOW_COUNT: usize = 10;

/// Maximum characters for command summary truncation (UTF-8 safe).
const CMD_TRUNCATE_CHARS: usize = 200;

// ── Idle detection thresholds ──
/// Fragmented-idle window: gaps between these bounds (below the >60s idle line).
const FRAG_GAP_MIN_SECS: f64 = 3.0;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute performance statistics from an ATIF trajectory.
pub fn compute_stats(trajectory: &AtifTrajectory) -> Result<PerfStats> {
    let Some(origin) = trajectory.origin_ts() else {
        return Ok(PerfStats {
            wall_secs: 0.0,
            tool_secs: 0.0,
            model_secs: 0.0,
            idle_secs: 0.0,
            tool_count: 0,
            tool_calls: vec![],
            top_slow: vec![],
            idle_gaps: vec![],
            frag_idle_secs: 0.0,
        });
    };

    let tool_calls = collect_tool_calls_with(trajectory, CMD_TRUNCATE_CHARS);

    let wall_secs = trajectory
        .last_ts()
        .map(|t| (t - origin).as_seconds_f64())
        .unwrap_or(0.0);

    let tool_secs: f64 = tool_calls.iter().map(|c| c.dur).sum();
    let model_secs = compute_model_turn_durations(trajectory).0;
    let idle_secs = (wall_secs - tool_secs - model_secs).max(0.0);
    let tool_count = tool_calls.len();

    // Top N slowest.
    let mut sorted_by_dur = tool_calls.clone();
    sorted_by_dur.sort_by(|a, b| {
        b.dur
            .partial_cmp(&a.dur)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_slow: Vec<ToolCallRecord> = sorted_by_dur.into_iter().take(TOP_SLOW_COUNT).collect();

    // Idle gaps: true user idle periods (agent text output → next user step).
    // Detect all gaps >= FRAG_GAP_MIN_SECS, then split into long (>60s) and fragmented (3-60s).
    let all_user_idle = detect_idle_gaps(trajectory, FRAG_GAP_MIN_SECS);
    let idle_gaps: Vec<IdleGap> = all_user_idle
        .iter()
        .filter(|g| g.dur > IDLE_GAP_THRESHOLD_SECS)
        .cloned()
        .collect();
    let frag_idle_secs: f64 = all_user_idle
        .iter()
        .filter(|g| g.dur >= FRAG_GAP_MIN_SECS && g.dur <= IDLE_GAP_THRESHOLD_SECS)
        .map(|g| g.dur)
        .sum();

    Ok(PerfStats {
        wall_secs,
        tool_secs,
        model_secs,
        idle_secs,
        tool_count,
        tool_calls,
        top_slow,
        idle_gaps,
        frag_idle_secs,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Per-step model inference durations.
///
/// Preferred: `end − start` per agent step (`extra.start_timestamp`).
/// Fallback when start is unrecorded: interval from the previous step's
/// timestamp to this step's end (mirrors the trigger → response measurement).
/// Returns (total_secs, per_step_vec).
fn compute_model_turn_durations(traj: &AtifTrajectory) -> (f64, Vec<f64>) {
    let mut turns: Vec<f64> = Vec::new();
    let mut prev_ts = None;

    for step in &traj.steps {
        let step_end = step.end_ts();
        if step.is_agent() {
            let start = step.start_ts().or(prev_ts);
            if let (Some(s), Some(e)) = (start, step_end) {
                let dur = (e - s).as_seconds_f64();
                if dur > 0.0 {
                    turns.push(dur);
                }
            }
        }
        if let Some(e) = step_end {
            prev_ts = Some(e);
        }
    }

    let total: f64 = turns.iter().sum();
    (total, turns)
}

/// Detect true idle gaps: periods where the agent has finished responding
/// (produced text output) and is waiting for the user's next message.
///
/// Gap = next user step's timestamp − the agent step's end. ATIF user steps
/// are genuine user turns (tool results live in agent step observations),
/// so no tool_result filtering is needed.
fn detect_idle_gaps(traj: &AtifTrajectory, threshold: f64) -> Vec<IdleGap> {
    let Some(origin) = traj.origin_ts() else {
        return vec![];
    };
    let mut gaps = Vec::new();
    let steps = &traj.steps;

    for (i, step) in steps.iter().enumerate() {
        if !step.is_agent() || !step.has_text_output() {
            continue;
        }
        let Some(end) = step.end_ts() else { continue };

        for next in &steps[i + 1..] {
            if next.is_agent() {
                break; // interaction continued without user input
            }
            if next.is_user() {
                // Next-turn user step: gap measured to its timestamp when
                // recorded, else to the following agent step's request start.
                let user_ts = next.end_ts();
                if let Some(u) = user_ts {
                    let dur = (u - end).as_seconds_f64();
                    if dur > threshold {
                        gaps.push(IdleGap {
                            start: (end - origin).as_seconds_f64(),
                            end: (u - origin).as_seconds_f64(),
                            dur,
                        });
                    }
                }
                break;
            }
        }
    }

    // Sort by duration descending — biggest gaps first.
    gaps.sort_by(|a, b| {
        b.dur
            .partial_cmp(&a.dur)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    gaps
}

// ---------------------------------------------------------------------------
// Perf data extraction for LLM strategy selection
// ---------------------------------------------------------------------------

/// Build the perf data set for LLM strategy selection.
///
/// Pure computation: wall-clock three-way split + top 5 slowest tool calls.
/// The LLM receives raw structured data and freely selects optimization strategies.
pub fn extract_perf_candidates(trajectory: &AtifTrajectory) -> Result<PerfCandidateSet> {
    if trajectory.steps.is_empty() {
        return Ok(PerfCandidateSet::default());
    }

    let stats = compute_stats(trajectory)?;
    if stats.tool_count == 0 && stats.wall_secs <= 0.0 {
        return Ok(PerfCandidateSet::default());
    }

    // Per-turn model inference durations (for prefix_cache strategy).
    let (_, model_turn_secs) = compute_model_turn_durations(trajectory);

    // Per-turn cache token stats (for prefix_cache strategy).
    let cache_turns = extract_cache_turns(trajectory);

    // Top 5 slowest tool calls (for fast_tool strategy).
    let top_tools: Vec<ToolCallRecord> = stats.top_slow.iter().take(5).cloned().collect();

    // Per-tool-name aggregation (for fast_tool strategy).
    let tool_agg = compute_tool_agg(&stats.tool_calls);

    Ok(PerfCandidateSet {
        wall_secs: stats.wall_secs,
        tool_secs: stats.tool_secs,
        model_secs: stats.model_secs,
        idle_secs: stats.idle_secs,
        tool_count: stats.tool_count,
        model_turn_secs,
        cache_turns,
        top_tools,
        tool_agg,
    })
}

/// Aggregate tool calls by name: count, total/avg/max duration.
/// Sorted by total_secs descending (most time-consuming tools first).
fn compute_tool_agg(tool_calls: &[ToolCallRecord]) -> Vec<ToolAggStats> {
    use std::collections::HashMap;
    let mut map: HashMap<&str, (usize, f64, f64)> = HashMap::new(); // (count, total, max)
    for tc in tool_calls {
        let entry = map.entry(tc.name.as_str()).or_insert((0, 0.0, 0.0));
        entry.0 += 1;
        entry.1 += tc.dur;
        if tc.dur > entry.2 {
            entry.2 = tc.dur;
        }
    }
    let mut agg: Vec<ToolAggStats> = map
        .into_iter()
        .map(|(name, (count, total, max))| ToolAggStats {
            name: name.to_string(),
            count,
            total_secs: total,
            avg_secs: if count > 0 { total / count as f64 } else { 0.0 },
            max_secs: max,
        })
        .collect();
    agg.sort_by(|a, b| {
        b.total_secs
            .partial_cmp(&a.total_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    agg
}

/// Per-step cache token stats from ATIF step metrics.
///
/// `prompt_tokens` semantics differ across providers (Anthropic excludes the
/// cached prefix, OpenAI includes it). When cached > prompt we assume
/// exclusive semantics and add the cached portion back.
fn extract_cache_turns(traj: &AtifTrajectory) -> Vec<CacheTurn> {
    let mut turns = Vec::new();
    for step in traj.steps.iter().filter(|s| s.is_agent()) {
        let Some(m) = step.metrics else { continue };
        let Some(prompt) = m.prompt_tokens else {
            continue;
        };
        let cached = m.cached_tokens.unwrap_or(0) as u64;
        let prompt = prompt as u64;
        let prompt_total = if cached > prompt {
            prompt + cached
        } else {
            prompt
        };
        turns.push(CacheTurn {
            prompt_tokens: prompt_total,
            cached_tokens: cached,
        });
    }
    turns
}

/// Sum of user idle gaps in the fragmented range. Kept for tool window use
/// by sibling modules via `crate::trace::tool_window_secs`.
#[allow(dead_code)]
pub(crate) fn step_tool_window(traj: &AtifTrajectory, idx: usize) -> f64 {
    tool_window_secs(traj, idx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn traj(steps_json: &str) -> AtifTrajectory {
        AtifTrajectory::from_json(&format!(
            r#"{{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{{"name":"a","version":"1","model_name":"m1"}},"steps":{steps_json}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn test_empty_trajectory() {
        let stats = compute_stats(&traj("[]")).unwrap();
        assert_eq!(stats.tool_count, 0);
        assert_eq!(stats.wall_secs, 0.0);
    }

    #[test]
    fn test_tool_window_basic() {
        // Step1 issues a Bash call, ends at t=1; step2's request starts at t=3.5
        // → tool window 2.5s.
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2026-07-02T06:30:01.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:30:00.000Z"},
             "tool_calls":[{"tool_call_id":"call_001","function_name":"Bash","arguments":{"command":"echo hello"}}],
             "observation":{"results":[{"source_call_id":"call_001","content":"hello"}]}},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:05.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:30:03.500Z"},
             "message":"done"}
        ]"#,
        ))
        .unwrap();

        assert_eq!(stats.tool_count, 1);
        assert_eq!(stats.tool_calls[0].name, "Bash");
        assert_eq!(stats.tool_calls[0].call_id, "call_001");
        assert!((stats.tool_calls[0].dur - 2.5).abs() < 0.01);
        assert!(!stats.tool_calls[0].err);
    }

    #[test]
    fn test_idle_gap_detection() {
        // Agent responds with text at t=1s, user replies at t=120s → 119s idle.
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2026-07-02T06:30:00.000Z","message":"do something"},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:01.000Z","message":"done"},
            {"step_id":3,"source":"user","timestamp":"2026-07-02T06:32:00.000Z","message":"next task"},
            {"step_id":4,"source":"agent","timestamp":"2026-07-02T06:32:01.000Z","message":"ok"}
        ]"#,
        ))
        .unwrap();

        assert_eq!(stats.idle_gaps.len(), 1);
        assert!(stats.idle_gaps[0].dur > 100.0);
    }

    #[test]
    fn test_model_inference_not_counted_as_idle() {
        // Two consecutive tool steps: step2 spans t=61→t=120 (59s model time),
        // tool window between step1 end (t=1) and step2 start (t=61) is 60s tool.
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2026-07-02T06:30:01.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:30:00.000Z"},
             "tool_calls":[{"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls"}}],
             "observation":{"results":[{"source_call_id":"c1","content":""}]}},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:32:00.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:31:01.000Z"},
             "tool_calls":[{"tool_call_id":"c2","function_name":"Bash","arguments":{"command":"pwd"}}]}
        ]"#,
        ))
        .unwrap();

        assert_eq!(stats.tool_count, 2);
        assert_eq!(stats.idle_gaps.len(), 0);
        // model = 1s (step1) + 59s (step2) = 60s
        assert!((stats.model_secs - 60.0).abs() < 0.01);
        // tool window = 60s (step1); step2's calls have no next step → 0
        assert!((stats.tool_secs - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_wall_clock_split_orthogonal() {
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2026-07-02T06:30:00.000Z","message":"go"},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:02.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:30:00.000Z"},
             "tool_calls":[{"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"ok"}]}},
            {"step_id":3,"source":"agent","timestamp":"2026-07-02T06:30:10.000Z",
             "extra":{"start_timestamp":"2026-07-02T06:30:07.000Z"},
             "message":"done"}
        ]"#,
        ))
        .unwrap();

        // wall = 10s; model = 2 + 3 = 5s; tool = 7 − 2 = 5s; idle = 0
        assert!((stats.wall_secs - 10.0).abs() < 0.01);
        assert!((stats.tool_secs - 5.0).abs() < 0.01);
        assert!((stats.model_secs - 5.0).abs() < 0.01);
        assert!(stats.idle_secs < 0.01);
    }

    #[test]
    fn test_error_call_marked() {
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2026-07-02T06:30:00.000Z",
             "tool_calls":[{"tool_call_id":"c1","function_name":"WebFetch","arguments":{"url":"https://example.com"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"Error: request timed out"}]}},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:30.000Z","message":"failed"}
        ]"#,
        ))
        .unwrap();

        assert_eq!(stats.tool_count, 1);
        assert!(stats.tool_calls[0].err);
        assert_eq!(stats.tool_calls[0].name, "WebFetch");
        // No start_timestamp on step2 → window measured to its end: 30s.
        assert!((stats.tool_calls[0].dur - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_parallel_calls_share_window() {
        // Two parallel calls in one step share the 10s window → 5s each,
        // tool_secs stays 10s.
        let stats = compute_stats(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2026-07-02T06:30:00.000Z",
             "tool_calls":[
                {"tool_call_id":"c1","function_name":"Read","arguments":{"file_path":"/a"}},
                {"tool_call_id":"c2","function_name":"Read","arguments":{"file_path":"/b"}}],
             "observation":{"results":[
                {"source_call_id":"c1","content":"a"},
                {"source_call_id":"c2","content":"b"}]}},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:10.000Z","message":"done"}
        ]"#,
        ))
        .unwrap();

        assert_eq!(stats.tool_count, 2);
        assert!((stats.tool_calls[0].dur - 5.0).abs() < 0.01);
        assert!((stats.tool_secs - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_perf_candidates_top_tools() {
        let set = extract_perf_candidates(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2026-07-02T06:30:00.000Z",
             "metrics":{"prompt_tokens":100,"cached_tokens":80},
             "tool_calls":[{"tool_call_id":"c1","function_name":"WebFetch","arguments":{"url":"https://a.com"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"ok"}]}},
            {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:20.000Z",
             "tool_calls":[{"tool_call_id":"c2","function_name":"WebFetch","arguments":{"url":"https://b.com"}}],
             "observation":{"results":[{"source_call_id":"c2","content":"Error: timeout"}]}},
            {"step_id":3,"source":"agent","timestamp":"2026-07-02T06:30:50.000Z","message":"done"}
        ]"#,
        ))
        .expect("extract");

        assert_eq!(set.top_tools.len(), 2);
        assert_eq!(set.top_tools[0].name, "WebFetch");
        assert!((set.top_tools[0].dur - 30.0).abs() < 0.1); // errored one is slower
        assert!(set.top_tools[0].err);
        assert!((set.top_tools[1].dur - 20.0).abs() < 0.1);
        assert!(!set.top_tools[1].err);
        // cache turn picked up from step metrics
        assert_eq!(set.cache_turns.len(), 1);
        assert_eq!(set.cache_turns[0].cached_tokens, 80);
    }

    #[test]
    fn test_extract_perf_candidates_empty() {
        let set = extract_perf_candidates(&traj("[]")).expect("extract");
        assert!(set.top_tools.is_empty());
    }
}
