use crate::runtime::prelude::{ApprovalDecision, ApprovalResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalOutcome {
    ProviderNativeShellFallback,
    ForegroundShellHandoff,
    ProviderApprovalResponse,
    LocalOnly,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ApprovalOutcomeInput {
    pub(crate) approved: bool,
    pub(crate) shell_tool: bool,
    pub(crate) provider_request: bool,
}

pub(crate) fn classify_approval_outcome(input: ApprovalOutcomeInput) -> ApprovalOutcome {
    if !input.approved {
        return if input.provider_request {
            ApprovalOutcome::ProviderApprovalResponse
        } else {
            ApprovalOutcome::LocalOnly
        };
    }

    if !input.shell_tool {
        return if input.provider_request {
            ApprovalOutcome::ProviderApprovalResponse
        } else {
            ApprovalOutcome::LocalOnly
        };
    }

    ApprovalOutcome::ForegroundShellHandoff
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderApprovalStatus {
    Approved,
    Blocked,
    Denied,
    Cancelled,
    Pending,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderResponseInput<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) tool_use_id: Option<&'a str>,
    pub(crate) tool_input: Option<&'a serde_json::Value>,
}

pub(crate) fn provider_status_response(
    input: ProviderResponseInput<'_>,
    status: ProviderApprovalStatus,
) -> ApprovalResponse {
    let decision = match status {
        ProviderApprovalStatus::Approved => ApprovalDecision::Allow,
        ProviderApprovalStatus::Blocked => ApprovalDecision::Deny {
            message: "cosh-shell blocked this Bash tool request before execution".to_string(),
        },
        ProviderApprovalStatus::Denied => ApprovalDecision::Deny {
            message: "User denied this operation".to_string(),
        },
        ProviderApprovalStatus::Cancelled => ApprovalDecision::Deny {
            message: "User cancelled this operation".to_string(),
        },
        ProviderApprovalStatus::Pending => ApprovalDecision::Deny {
            message: "Approval request is still pending".to_string(),
        },
    };
    provider_response(input, decision)
}

pub(crate) fn provider_allow_response(input: ProviderResponseInput<'_>) -> ApprovalResponse {
    provider_response(input, ApprovalDecision::Allow)
}

pub(crate) fn provider_deny_response(
    input: ProviderResponseInput<'_>,
    message: String,
) -> ApprovalResponse {
    provider_response(input, ApprovalDecision::Deny { message })
}

fn provider_response(
    input: ProviderResponseInput<'_>,
    decision: ApprovalDecision,
) -> ApprovalResponse {
    ApprovalResponse {
        request_id: input.request_id.to_string(),
        tool_use_id: input.tool_use_id.map(str::to_string),
        tool_input: input.tool_input.cloned(),
        decision,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApprovalExecutionMetadata {
    pub(crate) execution_path: Option<&'static str>,
    pub(crate) redaction_status: Option<&'static str>,
}

pub(crate) fn approval_execution_metadata(
    outcome: ApprovalOutcome,
    status: ProviderApprovalStatus,
    shell_tool: bool,
) -> ApprovalExecutionMetadata {
    if !shell_tool {
        return ApprovalExecutionMetadata {
            execution_path: None,
            redaction_status: None,
        };
    }

    match outcome {
        ApprovalOutcome::ProviderNativeShellFallback => ApprovalExecutionMetadata {
            execution_path: Some("provider_native_shell_tool_execution"),
            redaction_status: None,
        },
        ApprovalOutcome::ForegroundShellHandoff => ApprovalExecutionMetadata {
            execution_path: Some("foreground_shell_pty"),
            redaction_status: Some("ref_only"),
        },
        ApprovalOutcome::ProviderApprovalResponse | ApprovalOutcome::LocalOnly => {
            let execution_path = match status {
                ProviderApprovalStatus::Approved => "foreground_shell_pty",
                ProviderApprovalStatus::Blocked => "blocked",
                ProviderApprovalStatus::Denied => "not_executed_denied",
                ProviderApprovalStatus::Cancelled => "not_executed_cancelled",
                ProviderApprovalStatus::Pending => "pending",
            };
            ApprovalExecutionMetadata {
                execution_path: Some(execution_path),
                redaction_status: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_outcome_table() {
        struct Case {
            name: &'static str,
            input: ApprovalOutcomeInput,
            expected: ApprovalOutcome,
        }

        let cases = [
            Case {
                name: "denied provider request responds to provider",
                input: ApprovalOutcomeInput {
                    approved: false,
                    shell_tool: true,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ProviderApprovalResponse,
            },
            Case {
                name: "denied local request stays local",
                input: ApprovalOutcomeInput {
                    approved: false,
                    shell_tool: true,
                    provider_request: false,
                },
                expected: ApprovalOutcome::LocalOnly,
            },
            Case {
                name: "approved non-shell provider request responds to provider",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: false,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ProviderApprovalResponse,
            },
            Case {
                name: "approved local shell request uses foreground",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: true,
                    provider_request: false,
                },
                expected: ApprovalOutcome::ForegroundShellHandoff,
            },
            Case {
                name: "host-executed deliverable provider request uses foreground",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: true,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ForegroundShellHandoff,
            },
            Case {
                name: "obvious tty command uses foreground",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: true,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ForegroundShellHandoff,
            },
            Case {
                name: "missing command fails closed to foreground",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: true,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ForegroundShellHandoff,
            },
            Case {
                name: "safe provider shell without host-exec still uses foreground",
                input: ApprovalOutcomeInput {
                    approved: true,
                    shell_tool: true,
                    provider_request: true,
                },
                expected: ApprovalOutcome::ForegroundShellHandoff,
            },
        ];

        for case in cases {
            assert_eq!(
                classify_approval_outcome(case.input),
                case.expected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn provider_status_response_table() {
        struct Case {
            status: ProviderApprovalStatus,
            expected: &'static str,
        }

        let cases = [
            Case {
                status: ProviderApprovalStatus::Approved,
                expected: "allow",
            },
            Case {
                status: ProviderApprovalStatus::Blocked,
                expected: "blocked this Bash tool request",
            },
            Case {
                status: ProviderApprovalStatus::Denied,
                expected: "User denied this operation",
            },
            Case {
                status: ProviderApprovalStatus::Cancelled,
                expected: "User cancelled this operation",
            },
            Case {
                status: ProviderApprovalStatus::Pending,
                expected: "Approval request is still pending",
            },
        ];

        for case in cases {
            let tool_input = serde_json::json!({ "command": "echo ok" });
            let response = provider_status_response(
                ProviderResponseInput {
                    request_id: "ctrl-1",
                    tool_use_id: Some("toolu-1"),
                    tool_input: Some(&tool_input),
                },
                case.status,
            );
            assert_eq!(response.request_id, "ctrl-1");
            assert_eq!(response.tool_use_id.as_deref(), Some("toolu-1"));
            assert_eq!(response.tool_input.as_ref(), Some(&tool_input));
            match response.decision {
                ApprovalDecision::Allow => assert_eq!(case.expected, "allow"),
                ApprovalDecision::Deny { message } => assert!(message.contains(case.expected)),
                ApprovalDecision::HostExecutedShell { .. } => {
                    panic!("status response must not build host-executed result")
                }
                ApprovalDecision::Answer { .. } => {
                    panic!("status response must not build question answer response")
                }
                ApprovalDecision::ShellEvidence { .. } => {
                    panic!("status response must not build shell output evidence response")
                }
            }
        }
    }

    #[test]
    fn provider_deny_response_preserves_message_and_missing_tool_use_id() {
        let response = provider_deny_response(
            ProviderResponseInput {
                request_id: "ctrl-1",
                tool_use_id: None,
                tool_input: None,
            },
            "Missing provider tool_use_id".to_string(),
        );

        assert_eq!(response.request_id, "ctrl-1");
        assert!(response.tool_use_id.is_none());
        assert!(matches!(
            response.decision,
            ApprovalDecision::Deny { ref message } if message == "Missing provider tool_use_id"
        ));
    }

    #[test]
    fn approval_outcome_never_selects_provider_response_and_foreground_handoff() {
        for outcome in [
            ApprovalOutcome::ProviderNativeShellFallback,
            ApprovalOutcome::ForegroundShellHandoff,
            ApprovalOutcome::ProviderApprovalResponse,
            ApprovalOutcome::LocalOnly,
        ] {
            let sends_provider_response = matches!(
                outcome,
                ApprovalOutcome::ProviderNativeShellFallback
                    | ApprovalOutcome::ProviderApprovalResponse
            );
            let queues_foreground_handoff = outcome == ApprovalOutcome::ForegroundShellHandoff;

            assert!(
                !(sends_provider_response && queues_foreground_handoff),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn approval_execution_metadata_derives_path_from_outcome() {
        let cases = [
            (
                ApprovalOutcome::ProviderNativeShellFallback,
                ProviderApprovalStatus::Approved,
                true,
                Some("provider_native_shell_tool_execution"),
                None,
            ),
            (
                ApprovalOutcome::ForegroundShellHandoff,
                ProviderApprovalStatus::Approved,
                true,
                Some("foreground_shell_pty"),
                Some("ref_only"),
            ),
            (
                ApprovalOutcome::ProviderApprovalResponse,
                ProviderApprovalStatus::Blocked,
                true,
                Some("blocked"),
                None,
            ),
            (
                ApprovalOutcome::LocalOnly,
                ProviderApprovalStatus::Denied,
                true,
                Some("not_executed_denied"),
                None,
            ),
            (
                ApprovalOutcome::ProviderApprovalResponse,
                ProviderApprovalStatus::Approved,
                false,
                None,
                None,
            ),
        ];

        for (outcome, status, shell_tool, execution_path, redaction_status) in cases {
            assert_eq!(
                approval_execution_metadata(outcome, status, shell_tool),
                ApprovalExecutionMetadata {
                    execution_path,
                    redaction_status,
                }
            );
        }
    }
}
