//! Safe Agent-run grouping and compaction split-point selection.
//!
//! Compaction may only cut the transcript at complete semantic boundaries: a
//! full Agent run spans one user prompt through its final assistant response,
//! and an assistant tool call is indivisible from all of its tool results.
//! Any malformed tool-protocol structure fails closed.

use std::collections::HashSet;
use std::fmt;

use crate::provider::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Malformed tool-protocol structure detected while grouping a transcript.
pub enum BoundaryError {
    /// A tool result appeared without a matching pending tool call.
    OrphanToolResult {
        /// Transcript index of the offending message.
        index: usize,
    },
    /// A non-tool message arrived while tool results were still pending.
    MissingToolResults {
        /// Transcript index of the offending message.
        index: usize,
    },
    /// An assistant tool call carries an empty or duplicate call ID.
    MalformedToolCall {
        /// Transcript index of the offending message.
        index: usize,
    },
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OrphanToolResult { index } => {
                write!(formatter, "orphan tool result at transcript index {index}")
            }
            Self::MissingToolResults { index } => write!(
                formatter,
                "unresolved tool calls before transcript index {index}"
            ),
            Self::MalformedToolCall { index } => {
                write!(formatter, "malformed tool call at transcript index {index}")
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Half-open transcript span `[start, end)` covering one Agent run.
pub struct RunSpan {
    /// Index of the first message in the run.
    pub start: usize,
    /// Index one past the last message in the run.
    pub end: usize,
}

/// Groups a transcript into Agent runs while validating tool protocol.
///
/// A run starts at each top-level `user` message observed while no tool
/// results are pending; leading `system` messages attach to the first run.
///
/// # Errors
///
/// Fails closed with [`BoundaryError`] on orphan tool results, unresolved
/// tool calls, or malformed tool-call IDs.
pub(crate) fn group_agent_runs(messages: &[Message]) -> Result<Vec<RunSpan>, BoundaryError> {
    let mut runs: Vec<RunSpan> = Vec::new();
    let mut pending_tool_ids: HashSet<String> = HashSet::new();

    for (index, message) in messages.iter().enumerate() {
        match message.role.as_str() {
            "tool" => {
                let id = message.tool_call_id.as_deref().unwrap_or_default();
                if id.is_empty() || !pending_tool_ids.remove(id) {
                    return Err(BoundaryError::OrphanToolResult { index });
                }
            }
            "assistant" => {
                if !pending_tool_ids.is_empty() {
                    return Err(BoundaryError::MissingToolResults { index });
                }
                for call in message.tool_calls.iter().flatten() {
                    if call.id.is_empty() || !pending_tool_ids.insert(call.id.clone()) {
                        return Err(BoundaryError::MalformedToolCall { index });
                    }
                }
            }
            _ => {
                // "user", "system", and any unknown roles close no tool
                // exchange; arriving mid-exchange is a protocol violation.
                if !pending_tool_ids.is_empty() {
                    return Err(BoundaryError::MissingToolResults { index });
                }
                if message.role == "user" {
                    if let Some(current) = runs.last_mut() {
                        current.end = index;
                    }
                    runs.push(RunSpan {
                        start: runs.last().map(|span| span.end).unwrap_or(index),
                        end: messages.len(),
                    });
                }
            }
        }
    }

    // A transcript that ends while tool results are still pending is
    // malformed; accepting it would contradict the fail-closed contract.
    if !pending_tool_ids.is_empty() {
        return Err(BoundaryError::MissingToolResults {
            index: messages.len(),
        });
    }

    // Attach any leading system prelude to the first run.
    let mut normalized: Vec<RunSpan> = Vec::with_capacity(runs.len());
    let mut previous_end = 0usize;
    for span in runs {
        normalized.push(RunSpan {
            start: previous_end,
            end: span.end,
        });
        previous_end = span.end;
    }
    Ok(normalized)
}

/// Reports whether cutting the transcript at `index` splits no tool exchange
/// and lands exactly on an Agent-run boundary.
pub(crate) fn is_safe_split_point(messages: &[Message], index: usize) -> bool {
    if index == 0 || index > messages.len() {
        return false;
    }
    match group_agent_runs(messages) {
        Ok(runs) => runs
            .iter()
            .any(|span| span.start == index || span.end == index),
        Err(_) => false,
    }
}

fn last_compactable_run_index(runs: &[RunSpan], preserve_recent_runs: usize) -> Option<usize> {
    let preserve = preserve_recent_runs.max(1);
    (runs.len() > preserve).then(|| runs.len() - preserve)
}

fn choose_target_cut(
    messages: &[Message],
    candidates: impl IntoIterator<Item = usize>,
    target_tokens: u64,
) -> Option<usize> {
    let mut fallback = None;
    for cut in candidates {
        fallback = Some(cut);
        let retained = super::budget::estimate_messages_tokens(&messages[cut..]);
        if retained <= target_tokens {
            return Some(cut);
        }
    }
    fallback
}

fn transcript_ends_with_complete_agent_run(messages: &[Message], run: RunSpan) -> bool {
    messages[run.start..run.end]
        .iter()
        .rev()
        .find(|message| matches!(message.role.as_str(), "user" | "assistant" | "tool"))
        .is_some_and(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_none_or(|calls| calls.is_empty())
        })
}

/// Reports whether the transcript's last Agent run is still in flight.
///
/// An exhausted emergency ladder means different things depending on this: with
/// an active run the uncompactable remainder *is* that run, while without one
/// every run is complete and already covered by the projection. A malformed
/// transcript reports no active run; its real failure surfaces through the
/// selection path as a [`BoundaryError`].
pub(crate) fn has_active_run(messages: &[Message]) -> bool {
    match group_agent_runs(messages) {
        Ok(runs) => runs
            .last()
            .is_some_and(|run| !transcript_ends_with_complete_agent_run(messages, *run)),
        Err(_) => false,
    }
}

/// Reports whether automatic compaction can advance beyond the active
/// projection while preserving the configured number of recent runs.
///
/// This deliberately checks only boundary feasibility. The compactor still
/// owns target-aware cut selection, but an automatic recommendation must not
/// launch it when every eligible prefix is already compacted or protected.
///
/// # Errors
///
/// Propagates [`BoundaryError`] so malformed transcripts fail closed.
pub(crate) fn has_new_compactable_prefix(
    messages: &[Message],
    preserve_recent_runs: usize,
    compacted_through: usize,
) -> Result<bool, BoundaryError> {
    let runs = group_agent_runs(messages)?;
    Ok(last_compactable_run_index(&runs, preserve_recent_runs)
        .is_some_and(|index| runs[index].start > compacted_through))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How much trailing verbatim history a compaction attempt must keep.
///
/// This is the only difference between the manual, automatic, and emergency
/// paths: all three select their cut through
/// [`select_compacted_through_after`], so the boundary arithmetic and the
/// safety invariants cannot drift between triggers.
///
/// Neither variant may cut the active (incomplete) Agent run, and neither may
/// split a tool-call/tool-result exchange.
pub(crate) enum PreservationPolicy {
    /// Keep at least this many trailing complete Agent runs verbatim.
    ///
    /// Values below one are raised to one: normal automatic compaction never
    /// summarizes away the run the model is still reasoning about.
    RecentRuns(usize),
    /// Keep no complete run unconditionally; the latest *completed* run may be
    /// summarized.
    ///
    /// Used by explicit manual compaction and as the last emergency fallback
    /// before failing closed.
    ThroughLatestCompletedRun,
}

/// Selects a safe cut under `policy`, strictly after an already compacted
/// transcript prefix, or `None` when no safe cut frees a complete Agent run.
///
/// Requiring a cut past `compacted_through` prevents a later compaction from
/// re-selecting the active projection's boundary merely because its retained
/// suffix still fits the target.
///
/// # Errors
///
/// Propagates [`BoundaryError`] so malformed transcripts are never compacted.
pub(crate) fn select_compacted_through_after(
    messages: &[Message],
    policy: PreservationPolicy,
    target_tokens: u64,
    compacted_through: usize,
) -> Result<Option<usize>, BoundaryError> {
    match policy {
        PreservationPolicy::RecentRuns(runs) => {
            select_preserving_recent_runs(messages, runs, target_tokens, compacted_through)
        }
        PreservationPolicy::ThroughLatestCompletedRun => {
            select_through_latest_completed_run(messages, target_tokens, compacted_through)
        }
    }
}

/// Selects a cut that keeps at least `preserve_recent_runs` trailing runs.
///
/// Within that constraint the cut preserving the most history whose retained
/// suffix still fits `target_tokens` wins; when nothing fits, semantic
/// integrity wins and exactly `preserve_recent_runs` runs are preserved.
fn select_preserving_recent_runs(
    messages: &[Message],
    preserve_recent_runs: usize,
    target_tokens: u64,
    compacted_through: usize,
) -> Result<Option<usize>, BoundaryError> {
    let runs = group_agent_runs(messages)?;
    let Some(last_candidate) = last_compactable_run_index(&runs, preserve_recent_runs) else {
        return Ok(None);
    };
    // Candidate cuts are run starts from runs[1] (compact at least one run)
    // through runs[len - preserve] (keep the required recent runs).
    let candidates = runs[1..=last_candidate]
        .iter()
        .map(|span| span.start)
        .filter(|cut| *cut > compacted_through);
    Ok(choose_target_cut(messages, candidates, target_tokens))
}

/// Selects a cut that may reach the end of the latest *completed* Agent run,
/// including the transcript end when the transcript has no active run.
///
/// No recent run is reserved unconditionally. As much verbatim history as the
/// target permits is still preserved, and the latest completed run is only
/// summarized when no earlier cut satisfies the target. An incomplete trailing
/// run stops candidate generation, so the active run is never cut.
fn select_through_latest_completed_run(
    messages: &[Message],
    target_tokens: u64,
    compacted_through: usize,
) -> Result<Option<usize>, BoundaryError> {
    let runs = group_agent_runs(messages)?;
    if runs.is_empty() {
        return Ok(None);
    }
    let mut candidates = Vec::with_capacity(runs.len());
    let mut prefix_is_complete = true;
    for (index, run) in runs.iter().copied().enumerate() {
        prefix_is_complete &= transcript_ends_with_complete_agent_run(messages, run);
        if !prefix_is_complete {
            continue;
        }
        let cut = runs
            .get(index + 1)
            .map(|next| next.start)
            .unwrap_or(messages.len());
        if cut > compacted_through {
            candidates.push(cut);
        }
    }
    Ok(choose_target_cut(messages, candidates, target_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolCallFunction, ToolCallInfo};

    fn tool_call(id: &str) -> ToolCallInfo {
        ToolCallInfo {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "shell".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    fn run_with_tools(prompt: &str, call_id: &str) -> Vec<Message> {
        vec![
            Message::user(prompt),
            Message::assistant_with_tool_calls("", vec![tool_call(call_id)]),
            Message::tool_result(call_id, "output", false),
            Message::assistant("done"),
        ]
    }

    /// Selects from the transcript start while preserving recent runs.
    fn select_preserving(
        messages: &[Message],
        preserve_recent_runs: usize,
        target_tokens: u64,
    ) -> Result<Option<usize>, BoundaryError> {
        select_compacted_through_after(
            messages,
            PreservationPolicy::RecentRuns(preserve_recent_runs),
            target_tokens,
            0,
        )
    }

    /// Selects from the transcript start with no unconditionally reserved run.
    fn select_latest_completed(
        messages: &[Message],
        target_tokens: u64,
        compacted_through: usize,
    ) -> Result<Option<usize>, BoundaryError> {
        select_compacted_through_after(
            messages,
            PreservationPolicy::ThroughLatestCompletedRun,
            target_tokens,
            compacted_through,
        )
    }

    #[test]
    fn groups_simple_runs() {
        let mut messages = run_with_tools("first", "call-1");
        messages.extend(run_with_tools("second", "call-2"));
        let runs = group_agent_runs(&messages).expect("valid transcript");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], RunSpan { start: 0, end: 4 });
        assert_eq!(runs[1], RunSpan { start: 4, end: 8 });
    }

    #[test]
    fn automatic_preflight_requires_a_new_eligible_boundary() {
        let mut messages = run_with_tools("first", "call-1");
        messages.extend(run_with_tools("second", "call-2"));

        assert!(!has_new_compactable_prefix(&messages, 2, 0).unwrap());
        assert!(has_new_compactable_prefix(&messages, 1, 0).unwrap());

        let first_cut = group_agent_runs(&messages).unwrap()[1].start;
        assert!(!has_new_compactable_prefix(&messages, 1, first_cut).unwrap());
    }

    #[test]
    fn target_selection_advances_past_the_active_projection() {
        let mut messages = run_with_tools("first", "call-1");
        messages.extend(run_with_tools("second", "call-2"));
        messages.extend(run_with_tools("third", "call-3"));
        let runs = group_agent_runs(&messages).unwrap();

        assert_eq!(
            select_compacted_through_after(
                &messages,
                PreservationPolicy::RecentRuns(1),
                u64::MAX,
                runs[1].start,
            )
            .unwrap(),
            Some(runs[2].start)
        );
    }

    #[test]
    fn manual_selection_can_compact_one_complete_run() {
        let messages = run_with_tools("first", "call-1");

        assert_eq!(
            select_latest_completed(&messages, u64::MAX, 0).unwrap(),
            Some(messages.len())
        );
    }

    #[test]
    fn manual_selection_only_cuts_complete_run_prefixes() {
        let user_only = vec![Message::user("unfinished")];
        assert_eq!(
            select_latest_completed(&user_only, u64::MAX, 0).unwrap(),
            None
        );

        let tool_tail = vec![
            Message::user("unfinished tool run"),
            Message::assistant_with_tool_calls("", vec![tool_call("call-1")]),
            Message::tool_result("call-1", "output", false),
        ];
        assert_eq!(
            select_latest_completed(&tool_tail, u64::MAX, 0).unwrap(),
            None
        );

        let mut older_complete = run_with_tools("first", "call-1");
        let first_run_end = older_complete.len();
        older_complete.push(Message::user("unfinished second run"));
        assert_eq!(
            select_latest_completed(&older_complete, u64::MAX, 0).unwrap(),
            Some(first_run_end)
        );
    }

    #[test]
    fn leading_system_messages_attach_to_first_run() {
        let mut messages = vec![Message::system("[Hook context] init")];
        messages.extend(run_with_tools("first", "call-1"));
        let runs = group_agent_runs(&messages).expect("valid transcript");
        assert_eq!(runs[0].start, 0);
    }

    #[test]
    fn orphan_tool_result_fails_closed() {
        let messages = vec![
            Message::user("hi"),
            Message::tool_result("missing-call", "output", false),
        ];
        assert_eq!(
            group_agent_runs(&messages),
            Err(BoundaryError::OrphanToolResult { index: 1 })
        );
    }

    #[test]
    fn missing_tool_results_fail_closed() {
        let messages = vec![
            Message::user("hi"),
            Message::assistant_with_tool_calls("", vec![tool_call("call-1")]),
            Message::user("next prompt"),
        ];
        assert_eq!(
            group_agent_runs(&messages),
            Err(BoundaryError::MissingToolResults { index: 2 })
        );
    }

    #[test]
    fn pending_tool_calls_at_eof_fail_closed() {
        let messages = vec![
            Message::user("hi"),
            Message::assistant_with_tool_calls("", vec![tool_call("call-1")]),
        ];
        assert_eq!(
            group_agent_runs(&messages),
            Err(BoundaryError::MissingToolResults { index: 2 })
        );
        assert!(select_preserving(&messages, 1, 0).is_err());
    }

    #[test]
    fn duplicate_or_empty_tool_call_ids_fail_closed() {
        let duplicated = vec![
            Message::user("hi"),
            Message::assistant_with_tool_calls("", vec![tool_call("dup"), tool_call("dup")]),
        ];
        assert_eq!(
            group_agent_runs(&duplicated),
            Err(BoundaryError::MalformedToolCall { index: 1 })
        );
        let empty_id = vec![
            Message::user("hi"),
            Message::assistant_with_tool_calls("", vec![tool_call("")]),
        ];
        assert_eq!(
            group_agent_runs(&empty_id),
            Err(BoundaryError::MalformedToolCall { index: 1 })
        );
    }

    #[test]
    fn split_point_never_lands_inside_tool_exchange() {
        let mut messages = run_with_tools("first", "call-1");
        messages.extend(run_with_tools("second", "call-2"));
        // Indices 1..=3 fall inside the first run's tool exchange.
        for index in 1..4 {
            assert!(!is_safe_split_point(&messages, index), "index {index}");
        }
        assert!(is_safe_split_point(&messages, 4));
    }

    #[test]
    fn preserves_requested_recent_runs() {
        let mut messages = Vec::new();
        for run in 0..5 {
            messages.extend(run_with_tools(&format!("prompt {run}"), &format!("c{run}")));
        }
        let cut = select_preserving(&messages, 2, 0)
            .expect("valid transcript")
            .expect("cut selected");
        // Five runs of four messages: preserving 2 runs cuts at index 12.
        assert_eq!(cut, 12);
    }

    #[test]
    fn generous_target_preserves_more_history() {
        let mut messages = Vec::new();
        for run in 0..5 {
            messages.extend(run_with_tools(&format!("prompt {run}"), &format!("c{run}")));
        }
        let cut = select_preserving(&messages, 2, u64::MAX)
            .expect("valid transcript")
            .expect("cut selected");
        // A huge budget keeps everything except the first run.
        assert_eq!(cut, 4);
    }

    #[test]
    fn refuses_to_cut_when_too_few_runs() {
        let messages = run_with_tools("only", "call-1");
        assert_eq!(
            select_preserving(&messages, 2, 0).expect("valid transcript"),
            None
        );
    }

    #[test]
    fn malformed_transcript_never_selects_a_cut() {
        let messages = vec![
            Message::user("hi"),
            Message::tool_result("orphan", "x", false),
            Message::user("again"),
        ];
        assert!(select_preserving(&messages, 1, 0).is_err());
    }

    #[test]
    fn multibyte_transcripts_group_without_panic() {
        let mut messages = Vec::new();
        for run in 0..3 {
            messages.extend(run_with_tools(
                &format!("排查内存问题 第{run}轮 🎯"),
                &format!("调用-{run}"),
            ));
        }
        let runs = group_agent_runs(&messages).expect("valid multibyte transcript");
        assert_eq!(runs.len(), 3);
    }
}
