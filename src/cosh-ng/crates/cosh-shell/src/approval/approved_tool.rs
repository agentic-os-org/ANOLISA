use crate::runtime::prelude::*;

pub(crate) fn request_is_executable_bash_tool(request: &RuntimeApprovalRequest) -> bool {
    request.kind == ApprovalRequestKind::Tool && is_shell_tool_name(&request.subject)
}

pub(crate) fn request_is_readonly_builtin_tool(request: &RuntimeApprovalRequest) -> bool {
    if request.kind != ApprovalRequestKind::Tool {
        return false;
    }

    is_readonly_builtin_tool_name(&request.subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_request(subject: &str) -> RuntimeApprovalRequest {
        RuntimeApprovalRequest {
            id: "req-1".to_string(),
            audit_ref: None,
            run_id: "run-1".to_string(),
            origin: AgentRunOrigin::Standard,
            session_id: "sess-1".to_string(),
            cwd: "/tmp".to_string(),
            source: "agent",
            provider_shell_request_kind: ProviderShellRequestKind::StreamedToolCallFallback,
            kind: ApprovalRequestKind::Tool,
            subject: subject.to_string(),
            preview: "$ true".to_string(),
            risk: "medium",
            request_id: None,
            tool_use_id: None,
            tool_input: None,
            original_user_request: None,
            status: ApprovalRequestStatus::Pending,
            execution_path: None,
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            hook_requires_approval: false,
            hook_warnings: Vec::new(),
        }
    }

    #[test]
    fn executable_bash_tool_detection_accepts_provider_aliases() {
        for subject in [
            "Bash",
            "shell",
            "run_shell_command",
            "tool Bash",
            "tool shell",
            "tool run_shell_command",
        ] {
            assert!(
                request_is_executable_bash_tool(&tool_request(subject)),
                "{subject}"
            );
        }
        assert!(!request_is_executable_bash_tool(&tool_request("Read")));
    }

    #[test]
    fn readonly_builtin_tool_detection_accepts_provider_aliases() {
        for subject in [
            "Read",
            "Grep",
            "Glob",
            "LS",
            "read_file",
            "grep_search",
            "glob",
            "list_directory",
            "read_many_files",
            "tool Read",
            "tool Grep",
            "tool Glob",
            "tool LS",
            "tool read_file",
            "tool grep_search",
            "tool glob",
            "tool list_directory",
            "tool read_many_files",
        ] {
            assert!(
                request_is_readonly_builtin_tool(&tool_request(subject)),
                "{subject}"
            );
        }
        assert!(!request_is_readonly_builtin_tool(&tool_request("Bash")));
    }
}
