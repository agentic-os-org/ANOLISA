use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::types::{AgentEvent, AgentRequest, CoshApprovalMode};

mod driver;

use self::driver::{start_cancellable_qwen_process, start_control_protocol_qwen_process};
use super::claude::{join_reader_thread, read_lossy};
use super::prompt::provider_prompt_contract_for_request;
use super::qwen_stream::QwenStreamParser;
use super::{
    commit_pending_session, detach_committed_session, prompt_from_request,
    start_threaded_adapter_run, AdapterError, AdapterInstance, AgentAdapter,
    AgentBackendCapabilities, AgentRunHandle, FreshSessionOutcome, PreparedInvocation,
};

#[derive(Debug, Clone)]
pub struct QwenCliAdapter {
    pub program: String,
    pub allow_model_call: bool,
    pub session_id: Arc<Mutex<Option<String>>>,
}

impl Default for QwenCliAdapter {
    fn default() -> Self {
        Self {
            program: "co".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(None)),
        }
    }
}

impl QwenCliAdapter {
    pub fn with_model_call(mut self, allow: bool) -> Self {
        self.allow_model_call = allow;
        self
    }

    /// Detaches from the committed provider session so the next invocation
    /// omits `--resume` and starts a fresh conversation.
    pub(super) fn start_fresh_session(&self) -> FreshSessionOutcome {
        detach_committed_session(&self.session_id)
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

        args.extend(["--input-format".to_string(), "stream-json".to_string()]);
        match mode {
            CoshApprovalMode::Recommend => {
                args.extend(["--approval-mode".to_string(), "plan".to_string()]);
            }
            CoshApprovalMode::Auto => {
                args.extend(["--approval-mode".to_string(), "default".to_string()]);
                args.extend([
                    "--allowed-tools".to_string(),
                    "Read,Grep,Glob,LS,read_file,grep_search,glob,list_directory,read_many_files"
                        .to_string(),
                ]);
            }
            CoshApprovalMode::Trust => {
                args.extend(["--approval-mode".to_string(), "default".to_string()]);
            }
        }

        if let Some(session_id) = resume_session {
            args.push("--resume".to_string());
            args.push(session_id);
        }

        PreparedInvocation {
            program: self.program.clone(),
            args,
            prompt: qwen_prompt_from_request(request, mode),
        }
    }

    pub fn start_cancellable(
        &self,
        request: AgentRequest,
        mode: CoshApprovalMode,
    ) -> AgentRunHandle {
        let prepared = self.prepare_invocation(&request, mode);
        if !self.allow_model_call {
            let adapter = AdapterInstance::QwenCli(self.clone());
            return start_threaded_adapter_run(adapter, request);
        }

        if mode.uses_control_protocol() {
            return start_control_protocol_qwen_process(
                request.id,
                prepared,
                Arc::clone(&self.session_id),
            );
        }

        start_cancellable_qwen_process(request.id, prepared, Arc::clone(&self.session_id))
    }
}

impl AgentAdapter for QwenCliAdapter {
    fn name(&self) -> &'static str {
        "co"
    }

    fn capabilities(&self) -> AgentBackendCapabilities {
        AgentBackendCapabilities {
            text_stream: true,
            thinking_stream: false,
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
            for event in qwen_dry_run_events(request, &prepared) {
                sink(event)?;
            }
            return Ok(());
        }

        sink(AgentEvent::StatusChanged {
            run_id: request.id.clone(),
            phase: "starting".to_string(),
            message: "starting co stream-json backend".to_string(),
        })?;

        let mut child = Command::new(&prepared.program)
            .args(qwen_args_with_prompt(&prepared))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| AdapterError {
                message: format!("failed to run co cli: {err}"),
            })?;

        let stdout = child.stdout.take().ok_or_else(|| AdapterError {
            message: "failed to capture co cli stdout".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| AdapterError {
            message: "failed to capture co cli stderr".to_string(),
        })?;
        let stderr_handle = thread::spawn(move || read_lossy(stderr));

        let pending_session = Arc::new(Mutex::new(None));
        let mut parser =
            QwenStreamParser::new(request.id.clone(), Some(Arc::clone(&pending_session)));
        let mut completed = false;
        let mut failed = false;
        let mut terminal_events = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|err| AdapterError {
                message: format!("failed to read co cli stream: {err}"),
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
            message: format!("failed to wait for co cli: {err}"),
        })?;
        if !status.success() {
            let error = join_reader_thread(stderr_handle, "co cli stderr")?;
            sink(AgentEvent::AgentFailed {
                run_id: request.id.clone(),
                error: error.trim().to_string(),
                error_code: None,
                max_turns: None,
            })?;
            return Ok(());
        }

        let _stderr = join_reader_thread(stderr_handle, "co cli stderr")?;
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

fn qwen_prompt_from_request(request: &AgentRequest, mode: CoshApprovalMode) -> String {
    format!(
        "{}{}",
        prompt_from_request(request),
        provider_prompt_contract_for_request(request, mode, "run_shell_command")
    )
}

fn qwen_args_with_prompt(prepared: &PreparedInvocation) -> Vec<String> {
    let mut args = prepared.args.clone();
    args.push("--prompt".to_string());
    args.push(prepared.prompt.clone());
    args
}

fn update_completion_flags(event: &AgentEvent, completed: &mut bool, failed: &mut bool) {
    match event {
        AgentEvent::AgentCompleted { .. } => *completed = true,
        AgentEvent::AgentFailed { .. } | AgentEvent::AgentCancelled { .. } => *failed = true,
        _ => {}
    }
}

fn is_terminal_agent_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )
}

fn qwen_dry_run_events(request: &AgentRequest, prepared: &PreparedInvocation) -> Vec<AgentEvent> {
    let run_id = format!("qwen-dry-run-{}", request.command_block.id);
    vec![
        AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "dry_run".to_string(),
            message: "prepared co invocation without model call".to_string(),
        },
        AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: format!(
                "co adapter prepared a safe recommend-only invocation; model execution is disabled by default.\n\nPrepared invocation:\n  {}",
                prepared.argv_preview().join(" ")
            ),
        },
        AgentEvent::AgentCompleted {
            run_id,
            summary: "co dry-run completed without model call".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{qwen_args_with_prompt, PreparedInvocation, QwenCliAdapter};
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

    fn test_adapter() -> QwenCliAdapter {
        QwenCliAdapter {
            program: "qwen".to_string(),
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
    fn mode_flags_co_recommend() {
        let inv = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Recommend);
        assert!(inv.args.contains(&"--approval-mode".to_string()));
        assert!(inv.args.contains(&"plan".to_string()));
        assert!(inv.args.contains(&"--input-format".to_string()));
        assert!(inv.args.contains(&"stream-json".to_string()));
        assert!(inv.prompt.contains("recommend mode"), "{}", inv.prompt);
        assert!(
            inv.prompt.contains("do not emit tool calls"),
            "{}",
            inv.prompt
        );
        assert!(inv.prompt.contains("run_shell_command"), "{}", inv.prompt);
        assert!(!inv.args.contains(&"--allowed-tools".to_string()));
    }

    #[test]
    fn shell_handoff_continuation_keeps_plan_args_without_recommend_claim() {
        let mut request = test_request();
        request.context_hints = vec![
            crate::types::SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{}auto", crate::types::USER_APPROVAL_MODE_HINT_PREFIX),
        ];
        let inv = test_adapter().prepare_invocation(&request, CoshApprovalMode::Recommend);
        assert!(inv.args.contains(&"--approval-mode".to_string()));
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
    fn mode_flags_co_auto() {
        let inv = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--approval-mode".to_string()));
        assert!(inv.args.contains(&"default".to_string()));
        assert!(inv.args.contains(&"--allowed-tools".to_string()));
        assert!(inv.args.iter().any(|arg| arg.contains("read_file")));
        assert!(inv.prompt.contains("run_shell_command"), "{}", inv.prompt);
        assert!(
            inv.prompt
                .contains("Always emit a provider permission request"),
            "{}",
            inv.prompt
        );
        assert!(
            !inv.prompt.contains("Qwen adapter compatibility"),
            "{}",
            inv.prompt
        );
        assert!(!inv.prompt.contains("COSH_QUESTION"), "{}", inv.prompt);
        assert!(!inv.args.contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn mode_flags_co_trust() {
        let inv = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Trust);
        assert!(inv.args.contains(&"--approval-mode".to_string()));
        assert!(inv.args.contains(&"default".to_string()));
        assert!(!inv.args.contains(&"yolo".to_string()));
        assert!(inv.args.contains(&"--input-format".to_string()));
    }

    #[test]
    fn mode_flags_co_session_resume() {
        let adapter = QwenCliAdapter {
            program: "qwen".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-sess".to_string()))),
        };
        let inv = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--resume".to_string()));
        assert!(inv.args.contains(&"prev-sess".to_string()));
    }

    #[test]
    fn fresh_session_detaches_qwen_resume_id() {
        let adapter = QwenCliAdapter {
            program: "qwen".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-sess".to_string()))),
        };

        assert_eq!(
            adapter.start_fresh_session(),
            FreshSessionOutcome::Detached {
                previous_session_id: Some("prev-sess".to_string()),
            }
        );
        let invocation = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);
        assert!(!invocation.args.contains(&"--resume".to_string()));
        assert!(!invocation.args.contains(&"prev-sess".to_string()));
    }

    #[test]
    fn co_evidence_follow_up_reuses_committed_session_as_plain_prompt() {
        let adapter = QwenCliAdapter {
            program: "qwen".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-sess".to_string()))),
        };
        let inv = adapter.prepare_invocation(&shell_evidence_request(), CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--resume".to_string()));
        assert!(inv.args.contains(&"prev-sess".to_string()));
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
    fn mode_flags_co_fallback_can_disable_session_resume() {
        let adapter = QwenCliAdapter {
            program: "qwen".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-sess".to_string()))),
        };
        let mut request = test_request();
        request
            .context_hints
            .push("disable provider resume for shell handoff fallback".to_string());
        let inv = adapter.prepare_invocation(&request, CoshApprovalMode::Auto);
        assert!(!inv.args.contains(&"--resume".to_string()));
        assert!(!inv.args.contains(&"prev-sess".to_string()));
    }

    // The same-session retry fallback (T2) does not carry the
    // disable hint, so qwen keeps resuming the previous session like
    // cosh-core does.
    #[test]
    fn mode_flags_same_session_retry_fallback_keeps_session_resume() {
        let adapter = QwenCliAdapter {
            program: "qwen".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("prev-sess".to_string()))),
        };
        let mut request = test_request();
        request
            .context_hints
            .push("same-session retry for shell handoff fallback".to_string());
        let inv = adapter.prepare_invocation(&request, CoshApprovalMode::Auto);
        assert!(inv.args.contains(&"--resume".to_string()), "{:?}", inv.args);
        assert!(
            inv.args.contains(&"prev-sess".to_string()),
            "{:?}",
            inv.args
        );
    }

    #[test]
    fn qwen_process_args_append_prompt_flag() {
        let args = qwen_args_with_prompt(&PreparedInvocation {
            program: "qwen".to_string(),
            args: vec![
                "--allowed-tools".to_string(),
                "Read,Grep,Glob,LS".to_string(),
            ],
            prompt: "hello prompt".to_string(),
        });

        let prompt_at = args
            .iter()
            .position(|arg| arg == "--prompt")
            .expect("prompt flag");
        assert_eq!(
            args.get(prompt_at + 1).map(String::as_str),
            Some("hello prompt")
        );
    }
}
