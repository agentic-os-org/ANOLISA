use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cosh_platform::audit::{evaluate, LoadedPolicy, PolicySource};
use cosh_types::audit::{Action, ActionSubsystem, Decision, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicTerminalReason {
    ApprovalRequired,
    UserInputRequired,
    MaxToolCalls,
    CleanupFailed,
    Other(&'static str),
}

impl PublicTerminalReason {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval_required",
            Self::UserInputRequired => "user_input_required",
            Self::MaxToolCalls => "max_tool_calls",
            Self::CleanupFailed => "cleanup_failed",
            Self::Other(code) => code,
        }
    }

    pub(super) fn from_shared_code(code: &'static str) -> Self {
        match code {
            "approval_required" => Self::ApprovalRequired,
            "user_input_required" => Self::UserInputRequired,
            "max_tool_calls" => Self::MaxToolCalls,
            "cleanup_failed" => Self::CleanupFailed,
            other => Self::Other(other),
        }
    }
}

pub(crate) struct PublicHeadlessRuntime {
    pub(super) tool_calls: u32,
    pub(super) terminal: Option<PublicTerminalReason>,
    pub(super) cleanup_failure: Arc<AtomicBool>,
    policies: Vec<LoadedPolicy>,
}

impl PublicHeadlessRuntime {
    pub(crate) fn new(policies: Vec<LoadedPolicy>) -> Self {
        Self {
            tool_calls: 0,
            terminal: None,
            cleanup_failure: Arc::new(AtomicBool::new(false)),
            policies,
        }
    }

    pub(super) fn terminal_code(&self) -> Option<&'static str> {
        self.terminal.map(PublicTerminalReason::code)
    }

    pub(super) fn cleanup_failed(&self) -> bool {
        self.cleanup_failure.load(Ordering::SeqCst)
    }

    pub(super) fn cleanup_status(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cleanup_failure)
    }

    pub(super) fn outcome(decisions: &[Decision]) -> Outcome {
        decisions
            .iter()
            .map(|decision| decision.outcome)
            .fold(Outcome::Allow, narrow_outcome)
    }

    pub(super) fn decisions(&self, tool_name: &str, params: &serde_json::Value) -> Vec<Decision> {
        let mut actions = vec![public_policy_action(tool_name, params)];
        if tool_name == "run_command" {
            if let (Some(program), Ok(args)) = (
                params.get("program").and_then(serde_json::Value::as_str),
                serde_json::from_value::<Vec<String>>(
                    params
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                ),
            ) {
                actions.push(cosh_platform::audit::action::shell_action_from_argv(
                    program, &args,
                ));
                if let Some(name) = std::path::Path::new(program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| *name != program)
                {
                    actions.push(cosh_platform::audit::action::shell_action_from_argv(
                        name, &args,
                    ));
                }
            }
        }
        self.policies
            .iter()
            .filter(|policy| matches!(policy.source, PolicySource::UserFile(_)))
            .flat_map(|policy| actions.iter().map(move |action| evaluate(action, policy)))
            .collect()
    }
}

fn public_policy_action(tool_name: &str, params: &serde_json::Value) -> Action {
    let target_key = if tool_name == "run_command" {
        "program"
    } else {
        "path"
    };
    let target = params
        .get(target_key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let mut args = Vec::new();
    if let Some(object) = params.as_object() {
        for (key, value) in object {
            if key != target_key {
                append_policy_args(&mut args, key, value);
            }
        }
    }
    Action {
        subsystem: ActionSubsystem::Cosh,
        operation: tool_name.to_string(),
        target,
        args,
        raw: None,
    }
}

fn append_policy_args(args: &mut Vec<(String, String)>, key: &str, value: &serde_json::Value) {
    if let serde_json::Value::Array(values) = value {
        for value in values {
            append_policy_args(args, key, value);
        }
        return;
    }
    let value = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    args.push((key.to_string(), value));
}

fn narrow_outcome(current: Outcome, candidate: Outcome) -> Outcome {
    match (current, candidate) {
        (Outcome::Deny, _) | (_, Outcome::Deny) => Outcome::Deny,
        (Outcome::RequireApproval, _) | (_, Outcome::RequireApproval) => Outcome::RequireApproval,
        _ => Outcome::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_reasons_keep_stable_public_codes() {
        assert_eq!(
            PublicTerminalReason::ApprovalRequired.code(),
            "approval_required"
        );
        assert_eq!(
            PublicTerminalReason::UserInputRequired.code(),
            "user_input_required"
        );
        assert_eq!(PublicTerminalReason::MaxToolCalls.code(), "max_tool_calls");
        assert_eq!(PublicTerminalReason::CleanupFailed.code(), "cleanup_failed");
        assert_eq!(
            PublicTerminalReason::from_shared_code("cleanup_failed"),
            PublicTerminalReason::CleanupFailed
        );
    }

    #[test]
    fn policy_action_preserves_argv_and_other_arguments() {
        let action = public_policy_action(
            "run_command",
            &serde_json::json!({
                "program": "/bin/sh",
                "args": ["-c", "exit 7"],
                "timeout_ms": 25,
            }),
        );

        assert_eq!(action.target.as_deref(), Some("/bin/sh"));
        assert_eq!(
            action.args,
            vec![
                ("args".to_string(), "-c".to_string()),
                ("args".to_string(), "exit 7".to_string()),
                ("timeout_ms".to_string(), "25".to_string()),
            ]
        );
    }
}
