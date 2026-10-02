use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::types::{AgentEvent, AgentRequest, CoshApprovalMode};

mod driver;

use self::driver::{start_cancellable_claude_process, start_control_protocol_claude_process};
use super::prompt::provider_prompt_contract_for_request;
use super::{
    commit_pending_session, detach_committed_session, prompt_from_request,
    start_threaded_adapter_run, AdapterError, AdapterInstance, AgentAdapter,
    AgentBackendCapabilities, AgentRunHandle, ClaudeStreamParser, FreshSessionOutcome,
    PreparedInvocation, ProviderLineProgress,
};

#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    pub program: String,
    pub model: String,
    pub max_budget_usd: String,
    pub allow_model_call: bool,
    pub session_id: Arc<Mutex<Option<String>>>,
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: std::env::var("COSH_CLAUDE_MAX_BUDGET_USD")
                .unwrap_or_else(|_| "1.00".to_string()),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(None)),
        }
    }
}

impl ClaudeCodeAdapter {
    pub fn with_model_call(mut self, allow_model_call: bool) -> Self {
        self.allow_model_call = allow_model_call;
        self
    }

    /// Detaches from the committed provider session so the next invocation
    /// omits `--resume` and starts a fresh conversation.
    pub(super) fn start_fresh_session(&self) -> FreshSessionOutcome {
        detach_committed_session(&self.session_id)
    }

    pub fn start_cancellable(
        &self,
        request: AgentRequest,
        mode: CoshApprovalMode,
    ) -> AgentRunHandle {
        let prepared = self.prepare_invocation(&request, mode);
        if !self.allow_model_call {
            let adapter = AdapterInstance::ClaudeCode(self.clone());
            return start_threaded_adapter_run(adapter, request);
        }

        if mode.uses_control_protocol() {
            return start_control_protocol_claude_process(
                request.id,
                prepared,
                Arc::clone(&self.session_id),
            );
        }

        start_cancellable_claude_process(request.id, prepared, Arc::clone(&self.session_id))
    }

    pub fn prepare_invocation(
        &self,
        request: &AgentRequest,
        mode: CoshApprovalMode,
    ) -> PreparedInvocation {
        let disable_resume = request
            .context_hints
            .iter()
            .any(|hint| hint.contains("disable provider resume"));
        let resume_session = if disable_resume {
            None
        } else {
            self.session_id.lock().ok().and_then(|guard| guard.clone())
        };
        let mut args = vec![
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
        ];

        args.extend(["--print".to_string(), "--verbose".to_string()]);
        args.extend(["--model".to_string(), self.model.clone()]);
        args.extend(["--max-budget-usd".to_string(), self.max_budget_usd.clone()]);

        match mode {
            CoshApprovalMode::Recommend => {
                args.extend(["--permission-mode".to_string(), "plan".to_string()]);
                args.extend(["--tools".to_string(), "default".to_string()]);
            }
            CoshApprovalMode::Auto => {
                args.extend(["--input-format".to_string(), "stream-json".to_string()]);
                args.extend(["--permission-mode".to_string(), "default".to_string()]);
                args.extend([
                    "--permission-prompt-tool".to_string(),
                    "stdio".to_string(),
                    "--tools".to_string(),
                    "Bash,AskUserQuestion".to_string(),
                    "--settings".to_string(),
                    r#"{"permissions":{"allow":[],"deny":[],"ask":["Bash"]}}"#.to_string(),
                ]);
            }
            CoshApprovalMode::Trust => {
                args.extend(["--input-format".to_string(), "stream-json".to_string()]);
                args.extend(["--permission-mode".to_string(), "default".to_string()]);
                args.extend([
                    "--permission-prompt-tool".to_string(),
                    "stdio".to_string(),
                    "--tools".to_string(),
                    "Bash,AskUserQuestion".to_string(),
                    "--settings".to_string(),
                    r#"{"permissions":{"allow":[],"deny":[],"ask":["Bash"]}}"#.to_string(),
                ]);
            }
        }

        if let Some(session_id) = resume_session {
            args.push("--resume".to_string());
            args.push(session_id);
        }

        PreparedInvocation {
            program: self.program.clone(),
            args,
            prompt: claude_prompt_from_request(request, mode),
        }
    }
}

fn claude_prompt_from_request(request: &AgentRequest, mode: CoshApprovalMode) -> String {
    format!(
        "{}{}",
        prompt_from_request(request),
        provider_prompt_contract_for_request(request, mode, "Bash")
    )
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn capabilities(&self) -> AgentBackendCapabilities {
        AgentBackendCapabilities {
            text_stream: true,
            thinking_stream: true,
            session_resume: true,
            tool_intent: true,
            user_question: true,
            cancellable: true,
            control_protocol: true,
        }
    }

    fn run(&self, request: &AgentRequest) -> Result<Vec<AgentEvent>, AdapterError> {
        let mut events = Vec::new();
        self.run_stream(request, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    fn run_stream(
        &self,
        request: &AgentRequest,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let prepared = self.prepare_invocation(request, CoshApprovalMode::Recommend);
        if !self.allow_model_call {
            for event in claude_dry_run_events(request, &prepared) {
                sink(event)?;
            }
            return Ok(());
        }

        sink(AgentEvent::StatusChanged {
            run_id: request.id.clone(),
            phase: "starting".to_string(),
            message: "starting claude-code stream-json backend".to_string(),
        })?;

        let mut child = Command::new(&prepared.program)
            .args(&prepared.args)
            .arg(&prepared.prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| AdapterError {
                message: format!("failed to run claude code: {err}"),
            })?;

        let stdout = child.stdout.take().ok_or_else(|| AdapterError {
            message: "failed to capture claude code stdout".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| AdapterError {
            message: "failed to capture claude code stderr".to_string(),
        })?;
        let stderr_handle = thread::spawn(move || read_lossy(stderr));

        let pending_session = Arc::new(Mutex::new(None));
        let mut parser =
            ClaudeStreamParser::new(request.id.clone(), Some(Arc::clone(&pending_session)));
        let mut completed = false;
        let mut failed = false;
        let mut terminal_events = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|err| AdapterError {
                message: format!("failed to read claude code stream: {err}"),
            })?;
            for event in parser.parse_line(&line) {
                update_completion_flags(&event, &mut completed, &mut failed);
                if is_terminal_agent_event(&event) {
                    terminal_events.push(event);
                } else {
                    sink(event)?;
                }
            }
        }

        let status = child.wait().map_err(|err| AdapterError {
            message: format!("failed to wait for claude code: {err}"),
        })?;
        if !status.success() {
            let error = join_reader_thread(stderr_handle, "claude code stderr")?;
            sink(AgentEvent::AgentFailed {
                run_id: request.id.clone(),
                error: error.trim().to_string(),
                error_code: None,
                max_turns: None,
            })?;
            return Ok(());
        }

        let _stderr = join_reader_thread(stderr_handle, "claude code stderr")?;
        parser.finish(&mut |event| {
            update_completion_flags(&event, &mut completed, &mut failed);
            if is_terminal_agent_event(&event) {
                terminal_events.push(event);
                Ok(())
            } else {
                sink(event)
            }
        })?;
        if completed && !failed {
            commit_pending_session(&self.session_id, &pending_session);
        }
        for event in terminal_events {
            sink(event)?;
        }
        Ok(())
    }
}

pub(super) fn line_progress(progressed: bool) -> ProviderLineProgress {
    if progressed {
        ProviderLineProgress::Progress
    } else {
        ProviderLineProgress::NoProgress
    }
}

pub(super) fn update_completion_flags(event: &AgentEvent, completed: &mut bool, failed: &mut bool) {
    match event {
        AgentEvent::AgentCompleted { .. } => *completed = true,
        AgentEvent::AgentFailed { .. } | AgentEvent::AgentCancelled { .. } => *failed = true,
        _ => {}
    }
}

pub(super) fn is_terminal_agent_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )
}

pub(super) fn send_agent_event(
    sender: &mpsc::Sender<Result<AgentEvent, AdapterError>>,
    event: AgentEvent,
) {
    let _ = sender.send(Ok(event));
}

pub(super) fn terminate_process(pid: u32) {
    super::terminate_process_group(pid);
}

pub(super) fn read_lossy(mut reader: impl Read) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub(super) fn join_reader_thread(
    handle: thread::JoinHandle<std::io::Result<String>>,
    stream_name: &str,
) -> Result<String, AdapterError> {
    handle
        .join()
        .map_err(|_| AdapterError {
            message: format!("failed to join {stream_name} reader"),
        })?
        .map_err(|err| AdapterError {
            message: format!("failed to read {stream_name}: {err}"),
        })
}

fn claude_dry_run_events(request: &AgentRequest, prepared: &PreparedInvocation) -> Vec<AgentEvent> {
    let run_id = format!("claude-dry-run-{}", request.command_block.id);
    vec![
        AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "dry_run".to_string(),
            message: "prepared claude-code invocation without model call".to_string(),
        },
        AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: format!(
                "Claude Code adapter prepared a safe recommend-only invocation; model execution requires --run.\n\nPrepared invocation:\n  {}",
                prepared.argv_preview().join(" ")
            ),
        },
        AgentEvent::AgentCompleted {
            run_id,
            summary: "claude code dry-run completed without model call".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::ClaudeCodeAdapter;
    use crate::adapter::FreshSessionOutcome;
    use crate::types::{
        AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
    };

    fn test_request() -> AgentRequest {
        AgentRequest {
            id: "test".to_string(),
            session_id: "sess".to_string(),
            command_block: CommandBlock {
                id: "blk".to_string(),
                session_id: "sess".to_string(),
                command: "echo test".to_string(),
                origin: Default::default(),
                cwd: "/tmp".to_string(),
                end_cwd: "/tmp".to_string(),
                started_at_ms: 0,
                ended_at_ms: 0,
                duration_ms: 0,
                exit_code: 1,
                status: CommandStatus::Failed,
                output: OutputRefs {
                    terminal_output_ref: None,
                    terminal_output_bytes: 0,
                },
                shell_environment_generation: None,
                audit_identity: None,
            },
            context_blocks: vec![],
            context_hints: vec![],
            user_input: Some("test".to_string()),
            findings: vec![],
            mode: AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: None,
        }
    }

    fn test_adapter() -> ClaudeCodeAdapter {
        ClaudeCodeAdapter {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: "1".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    fn shell_evidence_request() -> AgentRequest {
        let mut request = test_request();
        request.id = "evidence-follow-up".to_string();
        request.user_input = Some(
            "ShellEvidenceExcerpt\n\
             output_id: terminal-output://sess/blk\n\
             command_id: blk\n\
             command: echo test\n\
             excerpt_status: included\n\
             redaction_status: excerpt_included\n\
             bounded_output_excerpt:\n\
             test\n"
                .to_string(),
        );
        request
    }

    #[test]
    fn mode_flags_claude_recommend() {
        let adapter = test_adapter();
        let req = test_request();
        let inv = adapter.prepare_invocation(&req, CoshApprovalMode::Recommend);
        assert!(inv.args.contains(&"--permission-mode".to_string()));
        assert!(inv.args.contains(&"plan".to_string()));
        assert!(inv.args.contains(&"--print".to_string()));
        assert!(inv.prompt.contains("recommend mode"), "{}", inv.prompt);
        assert!(
            inv.prompt.contains("do not emit tool calls"),
            "{}",
            inv.prompt
        );
        assert!(inv.prompt.contains("Bash"), "{}", inv.prompt);
        assert!(!inv.args.contains(&"--permission-prompt-tool".to_string()));
    }

    #[test]
    fn shell_handoff_continuation_keeps_plan_args_without_recommend_claim() {
        let adapter = test_adapter();
        let mut req = test_request();
        req.context_hints = vec![
            crate::types::SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{}auto", crate::types::USER_APPROVAL_MODE_HINT_PREFIX),
        ];
        let inv = adapter.prepare_invocation(&req, CoshApprovalMode::Recommend);
        assert!(inv.args.contains(&"--permission-mode".to_string()));
        assert!(inv.args.contains(&"plan".to_string()));
        assert!(!inv.prompt.contains("recommend mode"), "{}", inv.prompt);
        assert!(
            inv.prompt
                .contains("approval mode is auto and has not changed"),
            "{}",
            inv.prompt
        );
        assert!(
            inv.prompt.contains("Do not emit tool calls in this turn"),
            "{}",
            inv.prompt
        );
    }

    #[test]
    fn mode_flags_claude_auto() {
        let adapter = test_adapter();
        let req = test_request();
        let inv = adapter.prepare_invocation(&req, CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--print".to_string()));
        assert!(inv.args.contains(&"--input-format".to_string()));
        assert!(inv.args.contains(&"--permission-prompt-tool".to_string()));
        assert!(inv.args.contains(&"stdio".to_string()));
        assert!(inv.args.contains(&"Bash,AskUserQuestion".to_string()));
        assert!(inv.args.contains(&"--settings".to_string()));
        assert!(inv.args.iter().any(|arg| arg.contains(r#""ask":["Bash"]"#)));
        assert!(
            inv.prompt
                .contains("Always emit a provider permission request"),
            "{}",
            inv.prompt
        );
        assert!(
            !inv.prompt.contains("Claude adapter compatibility"),
            "{}",
            inv.prompt
        );
    }

    #[test]
    fn mode_flags_claude_trust() {
        let adapter = test_adapter();
        let req = test_request();
        let inv = adapter.prepare_invocation(&req, CoshApprovalMode::Trust);
        assert!(inv.args.contains(&"--permission-mode".to_string()));
        assert!(inv.args.contains(&"default".to_string()));
        assert!(!inv.args.contains(&"bypassPermissions".to_string()));
        assert!(inv.args.contains(&"--print".to_string()));
        assert!(inv.args.contains(&"--input-format".to_string()));
        assert!(inv.args.contains(&"stream-json".to_string()));
        assert!(inv.args.contains(&"--permission-prompt-tool".to_string()));
        assert!(inv.args.contains(&"Bash,AskUserQuestion".to_string()));
        assert!(inv.args.iter().any(|arg| arg.contains(r#""ask":["Bash"]"#)));
    }

    #[test]
    fn mode_flags_claude_session_resume() {
        let adapter = ClaudeCodeAdapter {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: "1".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-session".to_string()))),
        };
        let req = test_request();
        let inv = adapter.prepare_invocation(&req, CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--resume".to_string()));
        assert!(inv.args.contains(&"prev-session".to_string()));
    }

    #[test]
    fn fresh_session_detaches_claude_resume_id() {
        let adapter = ClaudeCodeAdapter {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: "1".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-session".to_string()))),
        };

        assert_eq!(
            adapter.start_fresh_session(),
            FreshSessionOutcome::Detached {
                previous_session_id: Some("prev-session".to_string()),
            }
        );
        let invocation = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);
        assert!(!invocation.args.contains(&"--resume".to_string()));
        assert!(!invocation.args.contains(&"prev-session".to_string()));
    }

    #[test]
    fn claude_evidence_follow_up_reuses_committed_session_as_plain_prompt() {
        let adapter = ClaudeCodeAdapter {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: "1".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-session".to_string()))),
        };
        let inv = adapter.prepare_invocation(&shell_evidence_request(), CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--resume".to_string()));
        assert!(inv.args.contains(&"prev-session".to_string()));
        assert!(
            inv.prompt.contains("user-requested shell evidence excerpt"),
            "{}",
            inv.prompt
        );
        assert!(
            !inv.prompt.contains("Tool result for request"),
            "{}",
            inv.prompt
        );
        assert!(
            !inv.prompt.contains("host_executed_shell"),
            "{}",
            inv.prompt
        );
    }

    #[test]
    fn mode_flags_claude_fallback_can_disable_session_resume() {
        let adapter = ClaudeCodeAdapter {
            program: "claude".to_string(),
            model: "sonnet".to_string(),
            max_budget_usd: "1".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-session".to_string()))),
        };
        let mut request = test_request();
        request
            .context_hints
            .push("disable provider resume for shell handoff fallback".to_string());
        let inv = adapter.prepare_invocation(&request, CoshApprovalMode::Auto);
        assert!(!inv.args.contains(&"--resume".to_string()));
        assert!(!inv.args.contains(&"prev-session".to_string()));
    }
}
