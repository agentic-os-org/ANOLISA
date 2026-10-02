use std::collections::{HashMap, HashSet};

use super::aggregate::{
    entity_key, finding_confidence, finding_topic, has_memory_pressure_with_process, severity_rank,
    AggregatedHookFinding,
};
use super::policy::{command_intent_key, should_downgrade_success_finding};
use super::prelude::{
    load_hook_feedback_preference_details, CommandBlock, CommandOrigin, FindingSeverity,
    HookFeedbackPreference,
};
use super::queue::interruption_budget_exhausted_for_budget;
use super::state::{hook_feedback_group_key, HookFeedback, HookRuntimeState, RuntimeHookDisplay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeHookDecision {
    pub(crate) display: RuntimeHookDisplay,
    pub(crate) reason: &'static str,
}

#[derive(Default)]
pub(crate) struct LoadedHookFeedbackPreferences {
    pub(crate) feedback: HashMap<String, HookFeedback>,
    pub(crate) noisy_groups: HashSet<String>,
}

pub(crate) fn load_hook_feedback_preferences() -> LoadedHookFeedbackPreferences {
    let preferences = load_hook_feedback_preference_details();
    let mut group_scores = HashMap::<String, i32>::new();
    let mut loaded = LoadedHookFeedbackPreferences::default();
    for preference in preferences {
        let Some(feedback) = hook_feedback_from_label(&preference.label) else {
            continue;
        };
        loaded
            .feedback
            .insert(preference.suppression_key.clone(), feedback);
        let Some(group_key) = hook_feedback_group_key_from_preference(&preference) else {
            continue;
        };
        let score = group_scores.entry(group_key).or_insert(0);
        match feedback {
            HookFeedback::Noisy => *score += 1,
            HookFeedback::Useful => *score -= 1,
        }
    }
    for (group_key, score) in group_scores {
        if score >= 2 {
            loaded.noisy_groups.insert(group_key);
        }
    }
    loaded
}

pub(crate) fn display_for_aggregate(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    manual_mode: bool,
) -> RuntimeHookDisplay {
    if manual_mode || block.exit_code != 0 {
        return RuntimeHookDisplay::Silent;
    }

    match aggregate.primary.severity {
        FindingSeverity::Critical => RuntimeHookDisplay::Consultation,
        FindingSeverity::Warning if has_memory_pressure_with_process(aggregate) => {
            RuntimeHookDisplay::Consultation
        }
        FindingSeverity::Warning => RuntimeHookDisplay::Hint,
        FindingSeverity::Info => RuntimeHookDisplay::Silent,
    }
}

#[cfg(test)]
pub(crate) fn apply_session_interruption_policy(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    display: RuntimeHookDisplay,
    suppression_key: &str,
    hooks: &HookRuntimeState,
    active_agent_run: bool,
) -> RuntimeHookDisplay {
    decide_session_interruption_policy(
        block,
        aggregate,
        display,
        suppression_key,
        hooks,
        active_agent_run,
    )
    .display
}

pub(crate) fn decide_session_interruption_policy(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    display: RuntimeHookDisplay,
    suppression_key: &str,
    hooks: &HookRuntimeState,
    active_agent_run: bool,
) -> RuntimeHookDecision {
    decide_session_interruption_policy_with_origin(
        block,
        aggregate,
        display,
        suppression_key,
        CommandOrigin::UserInteractive,
        hooks,
        active_agent_run,
    )
}

pub(crate) fn decide_session_interruption_policy_with_origin(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    display: RuntimeHookDisplay,
    suppression_key: &str,
    origin: CommandOrigin,
    hooks: &HookRuntimeState,
    active_agent_run: bool,
) -> RuntimeHookDecision {
    decide_session_interruption_policy_with_context_and_origin(
        block,
        aggregate,
        display,
        suppression_key,
        hooks,
        active_agent_run,
        hooks.block_followed_by_user_input(&block.id),
        origin,
    )
}

#[cfg(test)]
pub(crate) fn decide_session_interruption_policy_with_context(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    display: RuntimeHookDisplay,
    suppression_key: &str,
    hooks: &HookRuntimeState,
    active_agent_run: bool,
    user_continued_input: bool,
) -> RuntimeHookDecision {
    decide_session_interruption_policy_with_context_and_origin(
        block,
        aggregate,
        display,
        suppression_key,
        hooks,
        active_agent_run,
        user_continued_input,
        CommandOrigin::UserInteractive,
    )
}

pub(crate) fn decide_session_interruption_policy_with_context_and_origin(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    display: RuntimeHookDisplay,
    suppression_key: &str,
    hooks: &HookRuntimeState,
    active_agent_run: bool,
    user_continued_input: bool,
    origin: CommandOrigin,
) -> RuntimeHookDecision {
    if display == RuntimeHookDisplay::Silent {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Silent,
            reason: "base-silent",
        };
    }
    if internal_origin(origin) {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Silent,
            reason: "origin-internal",
        };
    }
    if origin == CommandOrigin::Unknown {
        return RuntimeHookDecision {
            display: unknown_origin_display(aggregate),
            reason: "origin-unknown",
        };
    }
    if is_muted_hook_target(aggregate, &hooks.muted_targets) {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Silent,
            reason: "muted",
        };
    }
    if hooks.feedback.get(suppression_key) == Some(&HookFeedback::Noisy) {
        return match display {
            RuntimeHookDisplay::Consultation => RuntimeHookDecision {
                display: RuntimeHookDisplay::Hint,
                reason: "feedback-noisy",
            },
            RuntimeHookDisplay::Hint => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "feedback-noisy",
            },
            RuntimeHookDisplay::Silent => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "base-silent",
            },
        };
    }
    if hooks.feedback.get(suppression_key) != Some(&HookFeedback::Useful)
        && hooks
            .noisy_groups
            .contains(&aggregate_feedback_group_key(block, aggregate))
    {
        return match display {
            RuntimeHookDisplay::Consultation => RuntimeHookDecision {
                display: RuntimeHookDisplay::Hint,
                reason: "feedback-group-noisy",
            },
            RuntimeHookDisplay::Hint => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "feedback-group-noisy",
            },
            RuntimeHookDisplay::Silent => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "base-silent",
            },
        };
    }
    if block.exit_code == 0 && should_downgrade_success_finding(&block.command) {
        return match display {
            RuntimeHookDisplay::Consultation => RuntimeHookDecision {
                display: RuntimeHookDisplay::Hint,
                reason: "non-diagnostic-success-command",
            },
            other => RuntimeHookDecision {
                display: other,
                reason: "allowed",
            },
        };
    }
    if display == RuntimeHookDisplay::Consultation && block.exit_code == 0 && active_agent_run {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Consultation,
            reason: "active-agent-run-deferred",
        };
    }
    if display == RuntimeHookDisplay::Consultation && block.exit_code == 0 && user_continued_input {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Silent,
            reason: "user-continued-input",
        };
    }
    if display == RuntimeHookDisplay::Consultation
        && block.exit_code == 0
        && finding_confidence(block, aggregate) == "low"
    {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Hint,
            reason: "low-confidence",
        };
    }
    if hooks.ignored_cards.contains(suppression_key) {
        return match display {
            RuntimeHookDisplay::Consultation => RuntimeHookDecision {
                display: RuntimeHookDisplay::Hint,
                reason: "ignored-same-finding",
            },
            RuntimeHookDisplay::Hint => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "ignored-same-finding",
            },
            RuntimeHookDisplay::Silent => RuntimeHookDecision {
                display: RuntimeHookDisplay::Silent,
                reason: "base-silent",
            },
        };
    }
    if display == RuntimeHookDisplay::Consultation {
        if let Some(record) = hooks.rendered_cards.get(suppression_key) {
            if severity_rank(record.severity) >= severity_rank(aggregate.primary.severity) {
                return RuntimeHookDecision {
                    display: RuntimeHookDisplay::Hint,
                    reason: "same-card-already-rendered",
                };
            }
        }
    }
    if display == RuntimeHookDisplay::Consultation
        && block.exit_code == 0
        && interruption_budget_exhausted_for_budget(block, aggregate, &hooks.interruption_budget)
    {
        return RuntimeHookDecision {
            display: RuntimeHookDisplay::Hint,
            reason: "interruption-budget",
        };
    }
    RuntimeHookDecision {
        display,
        reason: "allowed",
    }
}

fn is_muted_hook_target(
    aggregate: &AggregatedHookFinding,
    muted_targets: &HashSet<String>,
) -> bool {
    muted_targets.contains(&aggregate.topic)
        || muted_targets.contains(&aggregate.entity_key)
        || muted_targets.contains(&aggregate.primary.hook_id)
        || aggregate
            .related
            .iter()
            .any(|finding| muted_targets.contains(&finding.hook_id))
}

fn internal_origin(origin: CommandOrigin) -> bool {
    matches!(
        origin,
        CommandOrigin::UserAnalysisAction
            | CommandOrigin::AgentHandoff
            | CommandOrigin::ProviderTool
            | CommandOrigin::ShellInternal
    )
}

fn unknown_origin_display(aggregate: &AggregatedHookFinding) -> RuntimeHookDisplay {
    if aggregate.effective_severity == FindingSeverity::Critical {
        RuntimeHookDisplay::Hint
    } else {
        RuntimeHookDisplay::Silent
    }
}

fn aggregate_feedback_group_key(block: &CommandBlock, aggregate: &AggregatedHookFinding) -> String {
    hook_feedback_group_key(
        finding_topic(aggregate),
        &entity_key(block, aggregate),
        command_intent_key(&block.command),
    )
}

fn hook_feedback_group_key_from_preference(preference: &HookFeedbackPreference) -> Option<String> {
    if preference.topic.is_empty()
        || preference.entity_key.is_empty()
        || preference.command_intent.is_empty()
    {
        return None;
    }
    Some(hook_feedback_group_key(
        &preference.topic,
        &preference.entity_key,
        &preference.command_intent,
    ))
}

fn hook_feedback_from_label(label: &str) -> Option<HookFeedback> {
    match label {
        "noisy" => Some(HookFeedback::Noisy),
        "useful" => Some(HookFeedback::Useful),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct EnvLock {
        path: std::path::PathBuf,
    }

    impl Drop for EnvLock {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn env_lock() -> EnvLock {
        let path =
            std::env::temp_dir().join(format!("cosh-shell-test-env-{}.lock", std::process::id()));
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return EnvLock { path },
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(err) => panic!("create env test lock failed: {err}"),
            }
        }
    }

    #[test]
    fn hook_feedback_preferences_load_from_store() {
        let _env_lock = env_lock();
        let store = std::env::temp_dir().join(format!(
            "cosh-shell-main-feedback-store-{}.txt",
            std::process::id()
        ));
        let _ = fs::remove_file(&store);
        fs::write(
            &store,
            "noisy\tmemory:pressure:free\nuseful\tmemory:process:pid:1234\nnoisy\tmemory:pressure:free-a\ttopic=memory\tentity=system-memory\tintent=free\taction=noisy\nnoisy\tmemory:pressure:free-b\ttopic=memory\tentity=system-memory\tintent=free\taction=noisy\n",
        )
        .expect("write feedback store");
        std::env::set_var("COSH_SHELL_HOOK_FEEDBACK_STORE", &store);
        let loaded = load_hook_feedback_preferences();

        assert_eq!(
            loaded.feedback.get("memory:pressure:free").copied(),
            Some(HookFeedback::Noisy)
        );
        assert_eq!(
            loaded.feedback.get("memory:process:pid:1234").copied(),
            Some(HookFeedback::Useful)
        );
        assert!(loaded.noisy_groups.contains("memory:system-memory:free"));
        std::env::remove_var("COSH_SHELL_HOOK_FEEDBACK_STORE");
        let _ = fs::remove_file(&store);
    }
}
