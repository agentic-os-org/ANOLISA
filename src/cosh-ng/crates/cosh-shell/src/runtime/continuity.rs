use crate::agent::run::ActiveAgentRun;
use crate::runtime::prelude::*;
use crate::runtime::state::{ContinuityFactKind, InlineState};

pub(crate) fn continuity_prompt_hint(state: &InlineState, input: &str) -> Option<String> {
    if !is_follow_up_input(input) || state.continuity.facts.items.is_empty() {
        return None;
    }

    let facts = state
        .continuity
        .facts
        .items
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>();
    if facts.is_empty() {
        return None;
    }

    let lines = facts
        .into_iter()
        .rev()
        .map(|fact| format!("{}: {}", fact_label(fact.kind), truncate(&fact.text, 180)))
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "continuity facts from this cosh-shell session: {lines}. Use provider --resume as the primary conversation memory; use these facts only to disambiguate the current follow-up."
    ))
}

pub(crate) fn record_user_intent(state: &mut InlineState, input: &str) {
    state
        .continuity
        .facts
        .push(ContinuityFactKind::UserIntent, truncate(input, 220));
}

pub(crate) fn record_agent_run_facts(state: &mut InlineState, run: &ActiveAgentRun) {
    let mut last_text = None::<String>;
    let mut terminal = None::<String>;
    for governed in &run.governed_events {
        match &governed.event {
            AgentEvent::TextDelta { text, .. } if !text.trim().is_empty() => {
                last_text = Some(text.trim().to_string());
            }
            AgentEvent::AgentCompleted { summary, .. } => {
                terminal = Some(format!("completed: {summary}"));
            }
            AgentEvent::AgentFailed { error, .. } => {
                terminal = Some(format!("failed: {error}"));
            }
            AgentEvent::AgentCancelled { reason, .. } => {
                terminal = Some(format!("cancelled: {reason}"));
            }
            _ => {}
        }
    }

    if let Some(text) = last_text {
        state
            .continuity
            .facts
            .push(ContinuityFactKind::AgentResult, truncate(&text, 260));
    }
    if let Some(text) = terminal {
        state
            .continuity
            .facts
            .push(ContinuityFactKind::AgentResult, truncate(&text, 220));
    }
}

pub(crate) fn continuity_debug_lines(state: &InlineState) -> Vec<String> {
    if state.continuity.facts.items.is_empty() {
        return vec!["local continuity facts: none".to_string()];
    }
    state
        .continuity
        .facts
        .items
        .iter()
        .rev()
        .take(8)
        .enumerate()
        .map(|(idx, fact)| {
            format!(
                "fact {} [{}] {}",
                idx + 1,
                fact_label(fact.kind),
                truncate(&fact.text, 120)
            )
        })
        .collect()
}

fn is_follow_up_input(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let follow_up_markers = [
        "继续",
        "直接",
        "升级",
        "更新",
        "安装",
        "那",
        "刚才",
        "上面",
        "继续做",
        "do it",
        "continue",
        "then",
    ];
    trimmed.chars().count() <= 12
        || follow_up_markers
            .iter()
            .any(|marker| trimmed.contains(marker) || lower.contains(marker))
}

fn fact_label(kind: ContinuityFactKind) -> &'static str {
    match kind {
        ContinuityFactKind::UserIntent => "user",
        ContinuityFactKind::AgentResult => "agent",
    }
}

fn truncate(input: &str, max_chars: usize) -> String {
    let mut chars = input.trim().chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_hint_only_for_follow_up() {
        let mut state = InlineState::default();
        record_user_intent(&mut state, "帮我更新 git 版本");

        assert!(continuity_prompt_hint(&state, "直接升级").is_some());
        assert!(
            continuity_prompt_hint(&state, "请详细介绍一下这个仓库的模块职责以及测试策略")
                .is_none()
        );
    }
}
