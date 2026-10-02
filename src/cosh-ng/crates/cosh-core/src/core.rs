use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::io::AsyncBufReadExt;

use cosh_platform::audit::LoadedPolicy;
use cosh_types::audit::{AuditOutcomeStatus, AuditProviderData, AuditToolData, Outcome};

use crate::audit::{CoreAuditRecorder, CoreAuditScope};
use crate::auth::is_auth_error;
use crate::cli::ExecutionProfile;
use crate::compaction::{CompactionRuntime, ModelCapability};
#[cfg(test)]
use crate::config;
use crate::config::{ApprovalMode, CoreConfig};
use crate::context::ContextBuilder;
use crate::extension::{GenerationController, RuntimeGeneration, RuntimeSnapshot};
use crate::hook::{HookDecision, HookNotification, HookSystem, PreToolUseResult};
use crate::loop_detect::LoopDetector;
use crate::metrics::TurnMetrics;
use crate::protocol::{
    ClientControlCapabilities, ControlResponseBody, HostExecutedCheckpointCreateOutcome,
    HostExecutedCheckpointCreateResult, HostExecutedCheckpointError,
    HostExecutedCheckpointErrorOutcome, InputMessage, OutputMessage, ShellContext,
    ShellControlRequest,
};
use crate::provider::{
    ContentGenerator, GenerateConfig, GenerateEvent, Message, MAX_TOOL_CALL_INDEX,
};
use crate::tool::ask_user_question;
use crate::tool::{
    SessionWorkspace, ToolContext, ToolKind, ToolRegistry, ToolResult, ToolRuntimeContext,
};
use crate::truncator::OutputTruncator;

use self::tool_execution::{
    hash_bytes, hash_json, invalid_arguments_exhausted_error, invalid_arguments_message,
    json_shape, parse_in_band_question, parse_tool_arguments, InBandQuestion,
    InvalidArgumentStreak, MAX_INVALID_ARGUMENT_ATTEMPTS,
};

mod auth;
mod extensions;
mod tool_execution;

fn is_sensitive_write(tool_name: &str, params: &serde_json::Value) -> bool {
    tool_name == "write_file"
        && params
            .get("content")
            .and_then(serde_json::Value::as_str)
            .is_some_and(crate::redaction::contains_sensitive_text)
}

/// Typed terminal state for one user-request loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTurnOutcome {
    Completed,
    MaxTurns { limit: u32 },
}

pub struct CoshCore {
    pub config: CoreConfig,
    pub provider: Box<dyn ContentGenerator>,
    pub tools: Arc<ToolRegistry>,
    pub session_id: String,
    pub messages: Vec<Message>,
    /// Compaction runtime state: the active projection over the transcript
    /// prefix and the provider usage accounting that prices it.
    ///
    /// `messages` always stays the complete transcript; the provider only
    /// sees the projected effective context.
    pub compaction: CompactionRuntime,
    pub model: String,
    session_resumed: bool,
    pub shell_context: Option<ShellContext>,
    project_root: PathBuf,
    workspace: SessionWorkspace,
    pub extension_context: Option<String>,
    pub extra_params: Option<serde_json::Value>,
    pub hook_system: HookSystem,
    pub metrics: TurnMetrics,
    pub(crate) audit: CoreAuditRecorder,
    pub extension_generation: GenerationController,
    bound_extension_generation: u64,
    loaded_policy: LoadedPolicy,
    request_counter: AtomicU32,
    truncator: OutputTruncator,
    loop_detector: LoopDetector,
    /// Control capabilities the attached client declared at `initialize`.
    ///
    /// Defaults to "no capabilities" (headless or legacy clients), which
    /// keeps trust-mode shell execution provider-native. A client declaring
    /// both flags opts trust-mode shell commands into the core-issued
    /// approval channel instead (#2067).
    pub client_capabilities: ClientControlCapabilities,
    // One-shot mode shortens only this fallback; explicit overrides and
    // receipt-based handoff keep their existing semantics.
    approval_response_timeout_default: Duration,
    approval_timed_out: AtomicBool,
    /// First control-transport failure of this process, if any.
    ///
    /// Set from `&self` paths (`handle_ask_user`, `handle_shell_evidence`), so
    /// it is a `OnceLock` rather than a plain field: the first failure is the
    /// diagnostic one and the session is over either way.
    control_transport_failure: OnceLock<String>,
    execution_profile: ExecutionProfile,
}

impl CoshCore {
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    pub(crate) fn set_session_resumed(&mut self, resumed: bool) {
        self.session_resumed = resumed;
    }

    pub(crate) fn use_one_shot_approval_timeout(&mut self) {
        self.approval_response_timeout_default =
            Duration::from_secs(APPROVAL_RESPONSE_TIMEOUT_ONE_SHOT_DEFAULT_SECS);
    }

    pub(crate) fn approval_timed_out(&self) -> bool {
        self.approval_timed_out.load(Ordering::SeqCst)
    }

    fn apply_execution_profile_constraints(&mut self) {
        if self.execution_profile.is_brokered() {
            self.config.hooks = Default::default();
            self.config.mcp = Default::default();
            self.config.skills = Default::default();
            self.config.agent.allowed_tools.clear();
            self.config.agent.approval_mode = ApprovalMode::Recommend;
            self.hook_system = HookSystem::from_config(&self.config.hooks);
            self.extension_context = None;
        }
    }

    fn tool_runtime_context(&self) -> ToolRuntimeContext {
        let snapshot = self.extension_generation.current();
        ToolRuntimeContext {
            model: self.model.clone(),
            approval_mode: self.config.agent.approval_mode.to_string(),
            session_resumed: self.session_resumed,
            compaction_revision: self.compaction.revision(),
            compacted_through: self.compaction.state().map(|state| state.compacted_through),
            tools: self.tool_names(),
            active_extensions: snapshot.active_extensions.iter().cloned().collect(),
        }
    }

    pub fn emit<W: Write>(&self, writer: &mut W, msg: &OutputMessage) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = writeln!(writer, "{json}");
            let _ = writer.flush();
        }
    }

    /// Emits a control request whose response the core will then block on.
    ///
    /// [`Self::emit`] drops transport errors on the floor, which is fine for
    /// fire-and-forget stream events but not for a request that gates a
    /// blocking read: if the request did not arrive it can never be answered,
    /// so the wait would only end when the session dies.
    ///
    /// An error means *delivery is unknown*, not that nothing was sent.
    /// `write_all` may issue several underlying writes, a writer may accept a
    /// prefix before failing, and `BufWriter::flush` can fail after draining
    /// part of its buffer. Writing the line in one call keeps a failure from
    /// interleaving with other output; it cannot make the write atomic. The
    /// only safe reaction is therefore to stop waiting and end the session,
    /// whichever side of the pipe the bytes reached.
    fn emit_control_request_checked<W: Write>(
        &self,
        writer: &mut W,
        msg: &OutputMessage,
    ) -> Result<(), ControlTransportError> {
        let json = serde_json::to_string(msg)
            .map_err(|error| ControlTransportError::new("serialize", error.to_string()))?;
        let mut line = json.into_bytes();
        line.push(b'\n');
        writer
            .write_all(&line)
            .map_err(|error| ControlTransportError::new("write", error.to_string()))?;
        writer
            .flush()
            .map_err(|error| ControlTransportError::new("flush", error.to_string()))
    }

    /// Records a control-transport failure as session-fatal.
    ///
    /// The transport carries every request/response pair, so once a write on it
    /// fails there is no reliable way to reach the Shell and no way to learn
    /// what the user decided. Callers must return instead of waiting; the
    /// process then exits non-zero and the Shell recovers the run through its
    /// existing child-failure path, which also closes a card the Shell may have
    /// managed to display.
    fn note_control_transport_failure(&self, request_id: &str, error: &ControlTransportError) {
        let detail = format!("control transport {error} (request_id={request_id})");
        tracing::error!(
            request_id = %request_id,
            error_class = error.class(),
            "{detail}"
        );
        if self.control_transport_failure.set(detail.clone()).is_ok() {
            // The log file is not visible to the Shell; stderr is, and the
            // Shell surfaces its tail when the child exits non-zero.
            emit_fatal_diagnostic(&mut std::io::stderr().lock(), &detail);
        }
    }

    /// The session-fatal control-transport failure, if one happened.
    pub fn control_transport_failure(&self) -> Option<&str> {
        self.control_transport_failure.get().map(String::as_str)
    }

    /// Turns a control-transport failure into the error that ends this batch.
    ///
    /// [`Self::handle_ask_user`] and [`Self::handle_shell_evidence`] take
    /// `&self`, so the session flag is the only channel they have. Promoting it
    /// in the tool loop is what stops the remaining calls of the batch from
    /// running after the transport died; an already-set fatal turn wins.
    fn promote_control_transport_failure(&self, fatal_turn: &mut Option<FatalTurn>) {
        if fatal_turn.is_some() {
            return;
        }
        let Some(failure) = self.control_transport_failure() else {
            return;
        };
        let error = format!("{failure}; session cannot continue");
        *fatal_turn = Some(FatalTurn::new(error, CONTROL_TRANSPORT_AUDIT_REASON));
    }

    fn emit_hook_notifications<W: Write>(
        &self,
        writer: &mut W,
        notifications: &[HookNotification],
        tool_use_id: Option<&str>,
    ) {
        for n in notifications {
            self.emit(
                writer,
                &OutputMessage::hook_notification(
                    &n.hook_name,
                    &n.message,
                    tool_use_id,
                    n.decision.as_deref(),
                ),
            );
        }
    }

    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::SeqCst);
        format!("req-{n}")
    }

    pub fn cwd(&self) -> PathBuf {
        self.shell_context
            .as_ref()
            .map(|ctx| ctx.cwd.clone())
            .unwrap_or_else(|| self.project_root.clone())
    }

    /// Conservative runtime-prefix (`P`) estimate for budget computations.
    ///
    /// Skill summaries need async loading, so they are covered by the fixed
    /// reserve inside [`crate::compaction::estimate_prefix_tokens`] instead
    /// of being rendered here.
    pub(crate) fn estimate_prefix_tokens(&self) -> u64 {
        let system_prompt = ContextBuilder::build_system_prompt_with_extensions(
            &self.cwd(),
            &self.tool_names(),
            &[],
            self.config.agent.approval_mode.label(),
            self.config.ai.output_language.as_deref(),
            self.extension_context.as_deref(),
        );
        let declarations = serde_json::to_string(&self.tools.declarations()).unwrap_or_default();
        crate::compaction::estimate_prefix_tokens(&system_prompt, &declarations)
    }

    /// Current effective-context size in tokens under the active projection.
    pub(crate) fn effective_history_tokens(&self, prefix_tokens: u64) -> u64 {
        self.compaction
            .effective_history_tokens(&self.messages, prefix_tokens)
    }

    fn classify_tool(&self, tool_name: &str, params: &serde_json::Value) -> Outcome {
        let tool = match self.tools.get(tool_name) {
            Some(t) => t,
            None => return Outcome::Deny,
        };

        if self.execution_profile.is_brokered() {
            let workspace_write = self.execution_profile.writes_workspace()
                && tool.kind() == ToolKind::FileEdit
                && tool_name == "write_file";
            let hosted_checkpoint = self.execution_profile.hosts_checkpoint()
                && tool.kind() == ToolKind::HostedSideEffect
                && tool_name == "workspace_checkpoint_create"
                && params.as_object().is_some_and(serde_json::Map::is_empty);
            return if workspace_write || hosted_checkpoint {
                Outcome::RequireApproval
            } else {
                Outcome::Deny
            };
        }

        let mode = self.config.agent.approval_mode;

        if mode == ApprovalMode::Trust {
            // A control client that can answer `can_use_tool` and execute the
            // foreground handoff takes over trust-mode shell execution: the
            // approval channel is the only path where a hook Block reaches a
            // deterministic verdict instead of racing the shell-side staging
            // grace window (#2067). Legacy clients keep local execution.
            if self.client_capabilities.can_handle_can_use_tool
                && self.client_capabilities.can_handle_host_executed_shell
                && tool.kind() == ToolKind::ShellExec
            {
                return Outcome::RequireApproval;
            }
            return Outcome::Allow;
        }

        if self.config.agent.allowed_tools.contains(tool_name) {
            return Outcome::Allow;
        }

        if is_sensitive_write(tool_name, params) && mode == ApprovalMode::Auto {
            return Outcome::RequireApproval;
        }

        match (mode, tool.kind()) {
            (_, ToolKind::ReadOnly) => Outcome::Allow,
            (
                ApprovalMode::Auto,
                ToolKind::FileEdit | ToolKind::ShellEvidence | ToolKind::Other,
            ) => Outcome::Allow,
            // MCP, network, and extension tools are external boundaries. Do
            // not infer their side effects from descriptions or schemas.
            _ => Outcome::RequireApproval,
        }
    }

    pub(crate) async fn handle_user_message<W, R>(
        &mut self,
        content: &str,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> Result<AgentTurnOutcome, String>
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        self.handle_user_message_with_raw_input(content, None, reader, writer)
            .await
    }

    /// Handles a provider-facing envelope while giving UserPromptSubmit the
    /// structured raw input when the transport supplied one.
    ///
    /// The envelope remains authoritative for provider messages, transcripts,
    /// and compaction; the optional raw value affects only hook input.
    pub(crate) async fn handle_user_message_with_raw_input<W, R>(
        &mut self,
        content: &str,
        raw_user_input: Option<&str>,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> Result<AgentTurnOutcome, String>
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        self.approval_timed_out.store(false, Ordering::SeqCst);
        self.bind_current_extension_snapshot();
        let _generation_pin = self.extension_generation.pin();
        // Generate a unique run_id for this agent run.
        let run_id = uuid::Uuid::new_v4().to_string();
        self.hook_system.set_run_id(run_id.clone());

        // ─── Hook: UserPromptSubmit ───
        let cwd_str = self.cwd().to_string_lossy().to_string();
        let hook_prompt = raw_user_input.unwrap_or(content);
        let prompt_result = self
            .hook_system
            .fire_user_prompt_submit(&self.session_id, &cwd_str, hook_prompt)
            .await;
        self.audit.record_hook_decision(
            CoreAuditScope::run(&run_id),
            "user_prompt_submit",
            hook_outcome(&prompt_result.decision),
            hook_decision_name(&prompt_result.decision),
        );

        if let HookDecision::Block(reason) = &prompt_result.decision {
            // Block: no approval panel, notifications go to Governance fallback
            self.emit_hook_notifications(writer, &prompt_result.notifications, None);
            self.emit(
                writer,
                &OutputMessage::assistant_text(
                    &self.session_id,
                    &format!("Prompt blocked by hook: {reason}"),
                ),
            );
            return Ok(AgentTurnOutcome::Completed);
        }

        if matches!(prompt_result.decision, HookDecision::Ask) {
            let request_id = self.next_request_id();
            let synthetic_id = format!("prompt:{request_id}");

            // Extract the first hook name for the virtual HOOK: tool_name.
            let hook_name = prompt_result
                .notifications
                .first()
                .map(|n| n.hook_name.as_str())
                .unwrap_or("unknown");

            // Emit notifications (or fallback) with synthetic tool_use_id so
            // cosh-shell stores them in pending_hook_notifications.
            if prompt_result.notifications.is_empty() {
                // Hook returned ask but provided no reason/systemMessage — emit fallback.
                self.emit(
                    writer,
                    &OutputMessage::hook_notification(
                        hook_name,
                        "A hook requires your approval before this action can proceed.",
                        Some(&synthetic_id),
                        Some("ask"),
                    ),
                );
            } else {
                self.emit_hook_notifications(
                    writer,
                    &prompt_result.notifications,
                    Some(&synthetic_id),
                );
            }

            let approval_scope = CoreAuditScope::request(&run_id, None, &request_id, None);
            let audit_ref =
                self.audit
                    .record_approval_requested(approval_scope, "hook", "hook_ask", None);

            // Emit approval request with HOOK: prefix and empty input.
            // Checked like every other request the core then blocks on (#1994):
            // nothing has been appended to the transcript yet, so this one can
            // fail the turn immediately.
            if let Err(error) = self.emit_control_request_checked(
                writer,
                &OutputMessage::can_use_tool_with_audit_ref(
                    &request_id,
                    &format!("HOOK:{hook_name}"),
                    serde_json::json!({}),
                    &synthetic_id,
                    true, // hook_requires_approval
                    audit_ref,
                ),
            ) {
                self.note_control_transport_failure(&request_id, &error);
                let audit_error = self
                    .audit
                    .record_approval_emit_failed(approval_scope, "hook", None, error.class())
                    .err();
                return Err(control_transport_turn_error(
                    &request_id,
                    &error,
                    audit_error.as_deref(),
                ));
            }

            let approval = self.wait_for_approval(&request_id, None, reader).await;
            let (approval_status, approval_decision) = approval_audit_outcome(&approval);
            self.audit.record_approval_resolved(
                approval_scope,
                "hook",
                approval_status,
                None,
                approval_decision,
                None,
            )?;
            match approval {
                ApprovalResult::Allowed => { /* user confirmed, continue */ }
                ApprovalResult::Denied(reason) => {
                    self.emit(
                        writer,
                        &OutputMessage::assistant_text(
                            &self.session_id,
                            &format!(
                                "Prompt rejected: {}",
                                reason.unwrap_or_else(|| "user cancelled".to_string())
                            ),
                        ),
                    );
                    return Ok(AgentTurnOutcome::Completed);
                }
                ApprovalResult::TimedOut => {
                    // #1940: fail closed like the transport failure above —
                    // the turn must end rather than continue while a late
                    // shell-side decision could still execute.
                    self.emit(
                        writer,
                        &OutputMessage::assistant_text(
                            &self.session_id,
                            "Prompt approval timed out before reaching a decision surface. Nothing was executed; please retry.",
                        ),
                    );
                    return Err(format!(
                        "prompt approval timed out before reaching a decision surface (request_id={request_id}); nothing was executed and the turn ends here so a late decision cannot split state"
                    ));
                }
                ApprovalResult::Interrupted
                | ApprovalResult::HostExecutedShell { .. }
                | ApprovalResult::HostExecutedCheckpoint { .. } => {
                    return Ok(AgentTurnOutcome::Completed);
                }
            }
        } else {
            // allow / passthrough: notifications without tool_use_id go to
            // deferred_events → Governance panel at end of agent run.
            self.emit_hook_notifications(writer, &prompt_result.notifications, None);
        }

        self.messages.push(Message::user(content));

        // Inject additional context from hooks
        if let Some(ref ctx) = prompt_result.additional_context {
            self.messages
                .push(Message::system(&format!("[Hook context] {ctx}")));
        }

        let tool_decls = self.tools.declarations();
        let skill_summaries = self.tools.skill_summaries().await;
        // One resolver drives both sides of the output accounting: the cap this
        // request may spend and the `O` the compaction budget reserves for it.
        // Deriving them separately let the budget reserve a model's whole output
        // capability while the request still asked for it (#2240).
        let capability = ModelCapability::resolve(
            &self.config.session.compaction,
            self.config.agent.session_token_limit,
            &self.model,
        );
        let generate_config = GenerateConfig {
            model: self.model.clone(),
            max_tokens: capability.request_max_tokens(),
            temperature: None,
            // Usage reporting feeds compaction thresholds; the stream adapter
            // guarantees Usage is delivered before MessageEnd.
            include_usage: true,
            extra_params: self.extra_params.clone(),
        };

        let system_prompt = ContextBuilder::build_system_prompt_with_extensions(
            &self.cwd(),
            &self.tool_names(),
            &skill_summaries,
            self.config.agent.approval_mode.label(),
            self.config.ai.output_language.as_deref(),
            self.extension_context.as_deref(),
        );
        // Runtime prefix estimate (P): system prompt + serialized tool
        // declarations + the compaction module's reserve for hook context
        // injected mid-run.
        let prefix_tokens = crate::compaction::estimate_prefix_tokens(
            &system_prompt,
            &serde_json::to_string(&tool_decls).unwrap_or_default(),
        );

        let max_turns = self.config.agent.max_turns;
        // Spans the whole message, not one turn: the model re-issues a rejected
        // tool call on the *next* turn, so a per-turn counter would never see two
        // attempts in a row.
        let mut invalid_arguments = InvalidArgumentStreak::default();

        for _turn in 0..max_turns {
            // ─── Context preflight (every provider call, incl. tool loop) ───
            // The loop top is always a complete model/tool exchange boundary
            // with no pending approval or user question, so an emergency
            // compaction here can never split an unfinished interaction.
            crate::compaction::run_context_preflight(
                &mut self.compaction,
                &self.messages,
                self.provider.as_ref(),
                &self.model,
                &self.config,
                prefix_tokens,
                writer,
            )
            .await?;

            let turn_id = uuid::Uuid::new_v4().to_string();
            let turn_scope = CoreAuditScope::turn(&run_id, &turn_id);
            self.audit.record_turn_started(turn_scope);
            // Runtime context stays lossless; observers and stores redact their own copies.
            let provider_messages = self.compaction.effective_messages(&self.messages);

            // ─── Hook: BeforeModel ───
            let before_model_result = self
                .hook_system
                .fire_before_model(
                    &self.session_id,
                    &cwd_str,
                    &self.model,
                    &provider_messages,
                    &tool_decls,
                )
                .await;
            self.emit_hook_notifications(writer, &before_model_result.notifications, None);
            self.audit.record_hook_decision(
                turn_scope,
                "before_model",
                AuditOutcomeStatus::Success,
                "observed",
            );

            // A BeforeModel hook's rewritten declarations apply to this provider
            // call only — `tool_decls` and the ToolRegistry stay authoritative
            // for the next turn.
            let turn_tool_decls = before_model_result
                .updated_tools
                .as_deref()
                .unwrap_or(&tool_decls);

            let mut msgs_with_system = vec![Message::system(&system_prompt)];
            msgs_with_system.extend(provider_messages);

            let provider_request_id = uuid::Uuid::new_v4().to_string();
            let resolved_provider = self.config.resolve_provider();
            let provider_data = AuditProviderData {
                provider: resolved_provider.provider_type.clone(),
                model: Some(self.model.clone()),
                ..AuditProviderData::default()
            };
            let provider_scope =
                CoreAuditScope::request(&run_id, Some(&turn_id), &provider_request_id, None);
            self.audit.record_provider_started(
                provider_scope,
                &resolved_provider.provider_type,
                &provider_data,
            )?;

            // ─── SLS: API request timing ───
            self.metrics.api_requests += 1;
            let api_start = Instant::now();

            let stream_result = self
                .provider
                .generate(&msgs_with_system, turn_tool_decls, &generate_config)
                .await;

            let mut stream = match stream_result {
                Ok(s) => s,
                Err(e) if is_auth_error(&e) => {
                    self.metrics.api_errors += 1;
                    self.metrics.api_latency_ms += api_start.elapsed().as_millis() as u64;
                    self.audit.record_provider_terminal(
                        provider_scope,
                        &resolved_provider.provider_type,
                        &provider_data,
                        AuditOutcomeStatus::Failed,
                        "auth_error",
                        api_start.elapsed().as_millis() as u64,
                    );
                    self.audit.record_turn_terminal(
                        turn_scope,
                        AuditOutcomeStatus::Failed,
                        Some("provider_auth_error"),
                    );
                    // Attempt re-auth
                    if self.try_reauth(reader, writer).await {
                        continue; // Retry the turn with new credentials
                    }
                    return Err(e);
                }
                Err(e) => {
                    self.metrics.api_errors += 1;
                    self.metrics.api_latency_ms += api_start.elapsed().as_millis() as u64;
                    self.audit.record_provider_terminal(
                        provider_scope,
                        &resolved_provider.provider_type,
                        &provider_data,
                        AuditOutcomeStatus::Failed,
                        "request_error",
                        api_start.elapsed().as_millis() as u64,
                    );
                    self.audit.record_turn_terminal(
                        turn_scope,
                        AuditOutcomeStatus::Failed,
                        Some("provider_request_error"),
                    );
                    return Err(e);
                }
            };

            let mut text_buf = String::new();
            let mut tool_calls: Vec<PendingToolCall> = Vec::new();
            let mut usage_info: Option<(u32, u32, u32)> = None;
            let mut block_index: u32 = 0;
            let mut text_block_started = false;
            let mut thinking_block_started = false;
            let mut suppress_stream_text = false;
            let mut tool_call_seen = false;
            let mut message_end_seen = false;
            // Only the in-band question route may hide assistant text. With the
            // question tool disabled the marker can never become a question, so
            // suppressing the text would drop the reply with nothing to replace it.
            let in_band_questions_enabled =
                self.tools.supports_ask_user_question() && !self.execution_profile.is_brokered();

            self.emit(writer, &OutputMessage::stream_message_start());

            while let Some(event) = stream.next().await {
                // Tool indices size `tool_calls` below, so an out-of-range index
                // would let one malformed frame allocate billions of slots. Fail
                // the stream instead of trusting or silently dropping it.
                let event = match event.tool_call_index() {
                    Some(index) if index > MAX_TOOL_CALL_INDEX => GenerateEvent::Error(format!(
                        "provider reported tool call index {index}, above the supported \
                         maximum of {MAX_TOOL_CALL_INDEX}"
                    )),
                    _ => event,
                };
                match event {
                    GenerateEvent::ThinkingDelta(delta) => {
                        if !thinking_block_started {
                            self.emit(writer, &OutputMessage::stream_thinking_start(block_index));
                            thinking_block_started = true;
                        }
                        self.emit(
                            writer,
                            &OutputMessage::stream_thinking_delta(block_index, &delta),
                        );
                    }
                    GenerateEvent::TextDelta(delta) => {
                        if thinking_block_started {
                            self.emit(writer, &OutputMessage::stream_block_stop(block_index));
                            block_index += 1;
                            thinking_block_started = false;
                        }
                        if !tool_call_seen && !text_block_started {
                            self.emit(writer, &OutputMessage::stream_text_start(block_index));
                            text_block_started = true;
                        }
                        text_buf.push_str(&delta);
                        if !suppress_stream_text && !tool_call_seen {
                            if in_band_questions_enabled && text_buf.contains("COSH_QUESTION:") {
                                suppress_stream_text = true;
                            } else {
                                self.emit(
                                    writer,
                                    &OutputMessage::stream_text_delta(block_index, &delta),
                                );
                            }
                        }
                    }
                    GenerateEvent::ToolCallStart { index, id, name } => {
                        tool_call_seen = true;
                        if thinking_block_started {
                            self.emit(writer, &OutputMessage::stream_block_stop(block_index));
                            block_index += 1;
                            thinking_block_started = false;
                        }
                        if text_block_started {
                            self.emit(writer, &OutputMessage::stream_block_stop(block_index));
                            block_index += 1;
                            text_block_started = false;
                        }
                        let idx = index as usize;
                        if tool_calls.len() <= idx {
                            tool_calls.resize_with(idx + 1, PendingToolCall::default);
                        }
                        tool_calls[idx].id = id.clone();
                        tool_calls[idx].name = name.clone();
                        tool_calls[idx].block_index = block_index;
                        tool_calls[idx].block_closed = false;
                        tool_calls[idx].start_seen = true;
                        self.emit(
                            writer,
                            &OutputMessage::stream_tool_use_start(block_index, &id, &name),
                        );
                        block_index += 1;
                    }
                    GenerateEvent::ToolCallDelta {
                        index,
                        arguments_delta,
                    } => {
                        let idx = index as usize;
                        if tool_calls.len() <= idx {
                            tool_calls.resize_with(idx + 1, PendingToolCall::default);
                        }
                        let bi = tool_calls[idx].block_index;
                        self.emit(
                            writer,
                            &OutputMessage::stream_tool_use_delta(bi, &arguments_delta),
                        );
                        tool_calls[idx].arguments.push_str(&arguments_delta);
                        tool_calls[idx].delta_count += 1;
                    }
                    GenerateEvent::ToolCallEnd { index } => {
                        let idx = index as usize;
                        if idx < tool_calls.len() {
                            tool_calls[idx].end_seen = true;
                            let bi = tool_calls[idx].block_index;
                            self.emit(writer, &OutputMessage::stream_block_stop(bi));
                            tool_calls[idx].block_closed = true;
                            block_index = block_index.max(bi + 1);
                        }
                    }
                    GenerateEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_tokens,
                    } => {
                        usage_info = Some((prompt_tokens, completion_tokens, total_tokens));
                        // Explicit hand-off: provider usage feeds compaction
                        // thresholds through the runtime's accounting API.
                        self.compaction.note_provider_usage(prompt_tokens as u64);
                        // ─── SLS: token usage ───
                        self.metrics.tokens_input += prompt_tokens as u64;
                        self.metrics.tokens_output += completion_tokens as u64;
                        self.metrics.tokens_total += total_tokens as u64;
                        self.metrics.tokens_cached += cached_tokens as u64;
                    }
                    GenerateEvent::MessageEnd => {
                        self.metrics.api_latency_ms += api_start.elapsed().as_millis() as u64;
                        message_end_seen = true;
                        break;
                    }
                    GenerateEvent::Cancelled => {
                        self.audit.record_provider_terminal(
                            provider_scope,
                            &resolved_provider.provider_type,
                            &provider_data,
                            AuditOutcomeStatus::Cancelled,
                            "cancelled",
                            api_start.elapsed().as_millis() as u64,
                        );
                        self.audit.record_turn_terminal(
                            turn_scope,
                            AuditOutcomeStatus::Cancelled,
                            Some("provider_cancelled"),
                        );
                        return Err("provider request cancelled".to_string());
                    }
                    GenerateEvent::Error(e) => {
                        self.metrics.api_errors += 1;
                        self.metrics.api_latency_ms += api_start.elapsed().as_millis() as u64;
                        self.audit.record_provider_terminal(
                            provider_scope,
                            &resolved_provider.provider_type,
                            &provider_data,
                            AuditOutcomeStatus::Failed,
                            "stream_error",
                            api_start.elapsed().as_millis() as u64,
                        );
                        self.audit.record_turn_terminal(
                            turn_scope,
                            AuditOutcomeStatus::Failed,
                            Some("provider_stream_error"),
                        );
                        return Err(e);
                    }
                }
            }
            drop(stream);
            if !message_end_seen {
                self.audit.record_provider_terminal(
                    provider_scope,
                    &resolved_provider.provider_type,
                    &provider_data,
                    AuditOutcomeStatus::Failed,
                    "unexpected_eof",
                    api_start.elapsed().as_millis() as u64,
                );
                self.audit.record_turn_terminal(
                    turn_scope,
                    AuditOutcomeStatus::Failed,
                    Some("provider_unexpected_eof"),
                );
                return Err("provider stream ended without a terminal event".to_string());
            }
            let (input_tokens, output_tokens) = usage_info
                .map(|(input, output, _)| (Some(u64::from(input)), Some(u64::from(output))))
                .unwrap_or((None, None));
            let completed_provider_data = AuditProviderData {
                input_tokens,
                output_tokens,
                ..provider_data.clone()
            };
            self.audit.record_provider_terminal(
                provider_scope,
                &resolved_provider.provider_type,
                &completed_provider_data,
                AuditOutcomeStatus::Success,
                "completed",
                api_start.elapsed().as_millis() as u64,
            );

            // ─── Hook: AfterModel ───
            let after_model_result = self
                .hook_system
                .fire_after_model(
                    &self.session_id,
                    &cwd_str,
                    !tool_calls.is_empty(),
                    &text_buf,
                    &self.model,
                    &self.messages,
                    usage_info,
                )
                .await;
            self.emit_hook_notifications(writer, &after_model_result.notifications, None);
            self.audit.record_hook_decision(
                turn_scope,
                "after_model",
                AuditOutcomeStatus::Success,
                "observed",
            );

            if thinking_block_started {
                self.emit(writer, &OutputMessage::stream_block_stop(block_index));
                block_index += 1;
            }
            if text_block_started {
                self.emit(writer, &OutputMessage::stream_block_stop(block_index));
                block_index += 1;
            }
            for tc in &mut tool_calls {
                if !tc.id.is_empty() && !tc.block_closed {
                    self.emit(writer, &OutputMessage::stream_block_stop(tc.block_index));
                    tc.block_closed = true;
                    block_index = block_index.max(tc.block_index + 1);
                }
            }
            let emit_visible_text = tool_calls.is_empty()
                && !text_buf.is_empty()
                && !(in_band_questions_enabled && text_buf.contains("COSH_QUESTION:"));
            let _ = block_index;
            self.emit(writer, &OutputMessage::stream_message_stop());

            if emit_visible_text {
                self.emit(
                    writer,
                    &OutputMessage::assistant_text(&self.session_id, &text_buf),
                );
            }

            if tool_calls.is_empty() {
                if in_band_questions_enabled {
                    match parse_in_band_question(&text_buf) {
                        InBandQuestion::Valid(synthetic) => {
                            let result =
                                self.handle_ask_user(&synthetic, None, reader, writer).await;
                            if result.is_error {
                                self.messages.push(Message::assistant(&text_buf));
                                self.audit.record_turn_terminal(
                                    turn_scope,
                                    AuditOutcomeStatus::Failed,
                                    Some("question_failed"),
                                );
                                return Ok(AgentTurnOutcome::Completed);
                            }
                            self.messages.push(Message::assistant(&text_buf));
                            self.messages.push(Message::user(&format!(
                                "User answered the question: {}",
                                result.output
                            )));
                            self.audit.record_turn_terminal(
                                turn_scope,
                                AuditOutcomeStatus::Success,
                                Some("question_answered"),
                            );
                            continue;
                        }
                        // The marker already suppressed the visible text, so a
                        // rejected payload must fail the turn loudly instead of
                        // ending it as an ordinary answer the user never saw.
                        InBandQuestion::Invalid(error) => {
                            tracing::warn!(
                                provider_type = %resolved_provider.provider_type,
                                validation_error_code = error.code(),
                                text_bytes = text_buf.len(),
                                "rejected in-band COSH_QUESTION payload"
                            );
                            self.messages.push(Message::assistant(&text_buf));
                            self.audit.record_turn_terminal(
                                turn_scope,
                                AuditOutcomeStatus::Failed,
                                Some("question_invalid"),
                            );
                            return Err(tool_execution::in_band_question_error(error));
                        }
                        InBandQuestion::Absent => {}
                    }
                }

                // ─── Hook: Stop ───
                let stop_result = self
                    .hook_system
                    .fire_stop(&self.session_id, &cwd_str, &text_buf)
                    .await;
                self.emit_hook_notifications(writer, &stop_result.notifications, None);
                self.audit.record_hook_decision(
                    turn_scope,
                    "stop",
                    hook_outcome(&stop_result.decision),
                    hook_decision_name(&stop_result.decision),
                );
                if let HookDecision::Block(reason) = &stop_result.decision {
                    self.messages.push(Message::assistant(&text_buf));
                    self.messages.push(Message::user(&format!(
                        "[Hook rejected response] {reason}. Please revise your answer."
                    )));
                    self.audit.record_turn_terminal(
                        turn_scope,
                        AuditOutcomeStatus::Success,
                        Some("stop_hook_retry"),
                    );
                    continue;
                }

                self.messages.push(Message::assistant(&text_buf));
                self.audit
                    .record_turn_terminal(turn_scope, AuditOutcomeStatus::Success, None);
                return Ok(AgentTurnOutcome::Completed);
            }

            if tool_calls
                .iter()
                .any(|tc| tc.name.is_empty() && !tc.arguments.is_empty())
            {
                return Err(
                    "Provider emitted an incomplete tool call without a function name".to_string(),
                );
            }

            let tc_infos: Vec<crate::provider::ToolCallInfo> = tool_calls
                .iter()
                .filter(|tc| !tc.name.is_empty())
                .map(|tc| crate::provider::ToolCallInfo {
                    id: tc.id.clone(),
                    call_type: "function".to_string(),
                    function: crate::provider::ToolCallFunction {
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect();

            // An arguments-only streamed tool-call fragment cannot be executed or
            // represented in the next provider request. Continuing would append an
            // empty assistant message and ask the model again, eventually consuming
            // the entire turn budget without making progress.
            if tc_infos.is_empty() {
                return Err(
                    "Provider emitted an incomplete tool call without a function name".to_string(),
                );
            }
            self.messages
                .push(Message::assistant_with_tool_calls(&text_buf, tc_infos));

            let ctx = ToolContext::with_runtime(
                self.cwd(),
                self.session_id.clone(),
                self.project_root.clone(),
                self.workspace.clone(),
                self.tool_runtime_context(),
            );

            let mut interrupted = false;
            // Set once a tool call ends the run. The error is returned only after
            // this batch is fully answered.
            let mut fatal_turn: Option<FatalTurn> = None;

            for tc in &tool_calls {
                if tc.name.is_empty() {
                    continue;
                }

                // A transport failure seen on a `&self` path can only surface
                // through the session flag, so it becomes fatal here: the rest
                // of the batch must be skipped, not executed against a peer the
                // core can no longer talk to.
                self.promote_control_transport_failure(&mut fatal_turn);

                // Every id in the assistant message needs exactly one tool result,
                // or the next provider request violates tool-message pairing — and
                // headless persists and reuses the session even when a turn fails.
                if fatal_turn.is_some() {
                    self.skip_unexecuted_tool_call(
                        CoreAuditScope::tool(&run_id, &turn_id, &tc.id),
                        writer,
                        tc,
                    );
                    continue;
                }

                let tool_kind = self
                    .tools
                    .get(&tc.name)
                    .map(|tool| format!("{:?}", tool.kind()).to_ascii_lowercase())
                    .unwrap_or_else(|| "virtual".to_string());
                let tool_scope = CoreAuditScope::tool(&run_id, &turn_id, &tc.id);

                if tc.name == ask_user_question::TOOL_NAME
                    && self.tools.supports_ask_user_question()
                {
                    let dispatched = self
                        .dispatch_ask_user_tool_call(
                            tool_scope,
                            tc,
                            &resolved_provider.provider_type,
                            &tool_kind,
                            reader,
                            writer,
                        )
                        .await?;
                    if let Some(result) = dispatched {
                        self.messages.push(Message::tool_result(
                            &tc.id,
                            &result.output,
                            result.is_error,
                        ));
                    }
                    if interrupted {
                        return Ok(AgentTurnOutcome::Completed);
                    }
                    continue;
                }

                let parsed_params = parse_tool_arguments(&tc.arguments);
                let tool_data = AuditToolData {
                    tool_kind,
                    input_shape: Some(match &parsed_params {
                        Ok(params) => json_shape(params).to_string(),
                        Err(error) => error.audit_shape().to_string(),
                    }),
                    input_hash: Some(match &parsed_params {
                        Ok(params) => hash_json(params),
                        Err(_) => hash_bytes(tc.arguments.as_bytes()),
                    }),
                    execution_path: parsed_params
                        .as_ref()
                        .ok()
                        .filter(|params| is_sensitive_write(&tc.name, params))
                        .map(|_| "sensitive_write".to_string()),
                    ..AuditToolData::default()
                };
                self.audit
                    .record_tool_requested(tool_scope, &tc.name, &tool_data);

                let params = match parsed_params {
                    Ok(params) => {
                        invalid_arguments.clear();
                        params
                    }
                    Err(parse_error) => {
                        // Executing a tool with `null` parameters used to look like
                        // a call with every field absent; fail the call instead so
                        // the model can re-issue it.
                        let attempt =
                            invalid_arguments.record(&tc.name, parse_error.code(), &turn_id);
                        tracing::warn!(
                            provider_type = %resolved_provider.provider_type,
                            tool_call_id = %tc.id,
                            tool_name = %tc.name,
                            start_seen = tc.start_seen,
                            delta_count = tc.delta_count,
                            end_seen = tc.end_seen,
                            argument_bytes = tc.arguments.len(),
                            json_parse_status = parse_error.json_parse_status(),
                            validation_error_code = parse_error.code(),
                            attempt,
                            "rejected malformed tool arguments"
                        );
                        let result = self.reject_tool_arguments(
                            tool_scope,
                            &tc.name,
                            &tc.id,
                            &tool_data,
                            invalid_arguments_message(
                                &tc.name,
                                &parse_error,
                                attempt,
                                MAX_INVALID_ARGUMENT_ATTEMPTS,
                            ),
                        );
                        // Closes the pending tool in the UI before the run ends, so
                        // the last thing on screen is the failure, not a tool that
                        // looks like it is still generating arguments.
                        self.emit_provider_native_tool_result(writer, &tc.id, &result);
                        if attempt >= MAX_INVALID_ARGUMENT_ATTEMPTS {
                            // Held, not returned: the assistant message already
                            // declared every call in this batch, and the loop must
                            // still answer the rest before the run ends.
                            fatal_turn = Some(FatalTurn::new(
                                invalid_arguments_exhausted_error(&tc.name, &parse_error),
                                "invalid_tool_arguments_exhausted",
                            ));
                        }
                        continue;
                    }
                };

                if self
                    .tools
                    .get(&tc.name)
                    .map(|tool| tool.kind() == ToolKind::ShellEvidence)
                    .unwrap_or(false)
                {
                    self.audit
                        .record_tool_execution_started(tool_scope, &tc.name, &tool_data)?;
                    let tool_start = Instant::now();
                    let result = self
                        .handle_shell_evidence(&tc.id, &params, reader, writer)
                        .await;
                    self.audit.record_tool_terminal(
                        tool_scope,
                        &tc.name,
                        &tool_data,
                        result.is_error,
                        tool_start.elapsed().as_millis() as u64,
                        result.output.len() as u64,
                    );
                    self.emit_provider_native_tool_result(writer, &tc.id, &result);
                    self.messages.push(Message::tool_result(
                        &tc.id,
                        &result.output,
                        result.is_error,
                    ));
                    if interrupted {
                        return Ok(AgentTurnOutcome::Completed);
                    }
                    continue;
                }

                let outcome = self.classify_tool(&tc.name, &params);

                // 当工具是 skill 且 action=invoke 时，预查 skill_context 透传给
                // hook（供 agent-sec-core skill-ledger 等扩展使用）。
                let skill_context = if tc.name == "skill"
                    && params
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("invoke")
                        == "invoke"
                {
                    let skill_name = params.get("name").and_then(|v| v.as_str());
                    if let Some(name) = skill_name {
                        self.tools.lookup_skill(name).await.map(|s| {
                            serde_json::json!({
                                "skill_name": s.name,
                                "file_path": s.file_path.to_string_lossy(),
                            })
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };

                // ─── Hook: PreToolUse ───
                let hook_result = self
                    .hook_system
                    .fire_pre_tool_use(
                        &self.session_id,
                        &cwd_str,
                        &tc.id,
                        &tc.name,
                        &params,
                        skill_context.as_ref(),
                    )
                    .await;
                self.emit_hook_notifications(writer, &hook_result.notifications, Some(&tc.id));
                let (hook_status, hook_decision) = pre_tool_hook_audit(&hook_result);
                self.audit.record_hook_decision(
                    tool_scope,
                    "pre_tool_use",
                    hook_status,
                    hook_decision,
                );

                let (outcome, params) = match hook_result.decision {
                    HookDecision::Block(reason) | HookDecision::HookFailure(reason) => {
                        // ─── SLS: hook-blocked tool call counts as total + fail ───
                        self.metrics.tool_calls_total += 1;
                        self.metrics.tool_calls_fail += 1;
                        let result = ToolResult::error(format!("Blocked by hook: {reason}"));
                        self.messages.push(Message::tool_result(
                            &tc.id,
                            &result.output,
                            result.is_error,
                        ));
                        // Release the staged provider-native call on the
                        // client: without this result event the shell can
                        // only drop the staged call after its grace timeout,
                        // which opened the block-bypass handoff race (#2067).
                        // The machine-readable verdict marker lets the client
                        // journal the rejection without trusting result text.
                        self.emit_provider_native_hook_block_result(writer, &tc.id, &result);
                        self.audit.record_tool_terminal(
                            tool_scope,
                            &tc.name,
                            &tool_data,
                            result.is_error,
                            0,
                            result.output.len() as u64,
                        );
                        continue;
                    }
                    HookDecision::Ask => {
                        // Apply tool_input_patch even when decision is Ask so that
                        // sandbox-guard wrapping is preserved through the approval flow.
                        let params = if let Some(patch) = hook_result.tool_input_patch.clone() {
                            crate::hook::merge_json_pub(params, patch)
                        } else {
                            params
                        };
                        (Outcome::RequireApproval, params)
                    }
                    _ => {
                        let params = if let Some(patch) = hook_result.tool_input_patch {
                            crate::hook::merge_json_pub(params, patch)
                        } else {
                            params
                        };
                        (outcome, params)
                    }
                };

                let params_for_post_hook = params.clone();

                let mut tool_result_already_emitted = false;
                let tool_start = Instant::now();
                let result = match outcome {
                    Outcome::Allow => {
                        self.audit
                            .record_tool_execution_started(tool_scope, &tc.name, &tool_data)?;
                        let result = self.execute_tool(&tc.name, params, &ctx).await;
                        self.emit_provider_native_tool_result(writer, &tc.id, &result);
                        tool_result_already_emitted = true;
                        result
                    }
                    Outcome::RequireApproval => {
                        let hook_requires_approval =
                            matches!(hook_result.decision, HookDecision::Ask);
                        let request_id = self.next_request_id();
                        let approval_scope = CoreAuditScope::request(
                            &run_id,
                            Some(&turn_id),
                            &request_id,
                            Some(&tc.id),
                        );
                        let audit_ref = self.audit.record_approval_requested(
                            approval_scope,
                            &tc.name,
                            if hook_requires_approval {
                                "hook_ask"
                            } else if self.config.agent.approval_mode == ApprovalMode::Trust {
                                // In trust mode a non-hook RequireApproval is
                                // the capable-client shell handoff reroute.
                                "trust_shell_handoff"
                            } else {
                                "policy_approval"
                            },
                            Some(hash_json(&params)),
                        );
                        // Checked, not fire-and-forget: waiting for a decision
                        // on a request that may never have arrived is the silent
                        // permanent hang from #1994.
                        if let Err(error) = self.emit_control_request_checked(
                            writer,
                            &OutputMessage::can_use_tool_with_audit_ref(
                                &request_id,
                                &tc.name,
                                params.clone(),
                                &tc.id,
                                hook_requires_approval,
                                audit_ref,
                            ),
                        ) {
                            self.note_control_transport_failure(&request_id, &error);
                            // Held, not propagated with `?`: this barrier can
                            // fail too, and returning here would leave the
                            // assistant's tool call without a result in a
                            // transcript headless still persists.
                            let audit_error = self
                                .audit
                                .record_approval_emit_failed(
                                    approval_scope,
                                    &tc.name,
                                    None,
                                    error.class(),
                                )
                                .err();
                            // No interaction reached the wait, so this is
                            // neither an approval decision nor part of the
                            // average approval-wait denominator.
                            if fatal_turn.is_none() {
                                fatal_turn = Some(FatalTurn::new(
                                    control_transport_turn_error(
                                        &request_id,
                                        &error,
                                        audit_error.as_deref(),
                                    ),
                                    CONTROL_TRANSPORT_AUDIT_REASON,
                                ));
                            }
                            ToolResult::error(approval_emit_failed_tool_error(&error))
                        } else {
                            let accepted_tool_kind =
                                self.tools.get(&tc.name).map(|tool| tool.kind());
                            // ─── SLS: approval wait timing ───
                            let approval_start = Instant::now();
                            let approval_result = self
                                .wait_for_approval(&request_id, accepted_tool_kind, reader)
                                .await;
                            let approval_wait_ms = approval_start.elapsed().as_millis() as u64;
                            let (approval_status, approval_decision) =
                                approval_audit_outcome(&approval_result);
                            if !matches!(
                                &approval_result,
                                ApprovalResult::HostExecutedShell { .. }
                                    | ApprovalResult::HostExecutedCheckpoint { .. }
                            ) {
                                self.audit.record_approval_resolved(
                                    approval_scope,
                                    &tc.name,
                                    approval_status,
                                    None,
                                    approval_decision,
                                    Some(approval_wait_ms),
                                )?;
                            }
                            self.metrics.approval_wait_ms += approval_wait_ms;
                            self.metrics.approval_count += 1;
                            match approval_result {
                                ApprovalResult::Allowed => {
                                    self.metrics.approval_allow += 1;
                                    if self.execution_profile.is_brokered()
                                        && accepted_tool_kind == Some(ToolKind::HostedSideEffect)
                                    {
                                        ToolResult::error(
                                            "brokered side effect rejected generic allow; a typed Gateway result is required",
                                        )
                                    } else {
                                        self.audit.record_tool_execution_started(
                                            tool_scope, &tc.name, &tool_data,
                                        )?;
                                        let result =
                                            self.execute_tool(&tc.name, params, &ctx).await;
                                        self.emit_provider_native_tool_result(
                                            writer, &tc.id, &result,
                                        );
                                        tool_result_already_emitted = true;
                                        result
                                    }
                                }
                                ApprovalResult::HostExecutedShell {
                                    llm_content,
                                    exit_code,
                                } => {
                                    self.metrics.approval_allow += 1;
                                    let is_error = exit_code.is_some_and(|c| c != 0);
                                    ToolResult {
                                        output: llm_content,
                                        is_error,
                                    }
                                }
                                ApprovalResult::HostExecutedCheckpoint { result } => {
                                    self.metrics.approval_allow += 1;
                                    result
                                }
                                ApprovalResult::Denied(reason) => {
                                    self.metrics.approval_deny += 1;
                                    ToolResult::error(format!(
                                        "Tool call denied: {}",
                                        reason.unwrap_or_else(|| "no reason given".to_string())
                                    ))
                                }
                                ApprovalResult::Interrupted => {
                                    self.metrics.approval_deny += 1;
                                    interrupted = true;
                                    ToolResult::error("Interrupted by user")
                                }
                                ApprovalResult::TimedOut => {
                                    self.metrics.approval_deny += 1;
                                    // #1940: fail closed. The batch still answers
                                    // every declared call so tool-message pairing
                                    // holds, then the fatal ends the turn before
                                    // another provider generation — the core must
                                    // never record "not executed" and continue a
                                    // turn in which a late shell-side decision
                                    // could still execute.
                                    hold_approval_timeout_fatal(&mut fatal_turn, &request_id);
                                    ToolResult::error(
                                        "approval timed out: the request never reached a decision surface; the tool was not executed",
                                    )
                                }
                            }
                        }
                    }
                    Outcome::Deny => {
                        self.metrics.approval_deny += 1;
                        ToolResult::error(format!("Tool '{}' denied by security policy", tc.name))
                    }
                };
                // ─── SLS: tool call total/duration/success/fail ───
                self.metrics.tool_calls_total += 1;
                self.metrics.tool_calls_duration_ms += tool_start.elapsed().as_millis() as u64;
                if result.is_error {
                    self.metrics.tool_calls_fail += 1;
                } else {
                    self.metrics.tool_calls_success += 1;
                }

                // ─── Hook: PostToolUse ───
                let post_hook = self
                    .hook_system
                    .fire_post_tool_use(
                        &self.session_id,
                        &cwd_str,
                        &tc.id,
                        &tc.name,
                        &params_for_post_hook,
                        &result.output,
                        skill_context.as_ref(),
                    )
                    .await;
                self.emit_hook_notifications(writer, &post_hook.notifications, Some(&tc.id));
                self.audit.record_hook_decision(
                    tool_scope,
                    "post_tool_use",
                    hook_outcome(&post_hook.decision),
                    hook_decision_name(&post_hook.decision),
                );

                // Precedence: block/deny > updated response > original,
                // then append additional context.
                let mut result = if let HookDecision::Block(reason) = &post_hook.decision {
                    ToolResult::error(format!("Post-tool hook denied: {reason}"))
                } else if post_hook.updated_tool_response.is_some()
                    || post_hook.additional_context.is_some()
                {
                    let base = post_hook
                        .updated_tool_response
                        .as_deref()
                        .unwrap_or(&result.output);
                    let output = if let Some(ref extra) = post_hook.additional_context {
                        format!("{base}\n[Hook context] {extra}")
                    } else {
                        base.to_string()
                    };
                    ToolResult {
                        output,
                        // Preserve the original is_error flag on normal replacement.
                        is_error: result.is_error,
                    }
                } else {
                    result
                };

                // ─── Hook: PostToolUseFailure ───
                if result.is_error {
                    // Emit tool_result BEFORE running PostToolUseFailure hooks, but only
                    // if it hasn't been emitted yet. The Allowed path already emits
                    // in-line; HostExecutedShell needs this early emit to prevent
                    // cosh-shell stall timeout from racing against hook execution.
                    if !tool_result_already_emitted {
                        self.emit_provider_native_tool_result(writer, &tc.id, &result);
                    }
                    let failure_hook = self
                        .hook_system
                        .fire_post_tool_use_failure(
                            &self.session_id,
                            &cwd_str,
                            &tc.id,
                            &tc.name,
                            &params_for_post_hook,
                            &result.output,
                            skill_context.as_ref(),
                        )
                        .await;
                    self.emit_hook_notifications(writer, &failure_hook.notifications, Some(&tc.id));
                    let bypass_requested = failure_hook.sandbox_bypass_request.is_some();
                    self.audit.record_hook_decision(
                        tool_scope,
                        "post_tool_use_failure",
                        AuditOutcomeStatus::Success,
                        if bypass_requested {
                            "sandbox_bypass_requested"
                        } else {
                            "observed"
                        },
                    );

                    // ─── Sandbox Bypass ───
                    // If a hook requests sandbox bypass, present an approval
                    // panel with the original (un-sandboxed) command.
                    // ─── SLS: sandbox blocked ───
                    // #1940: when the turn is already fatal (the policy
                    // approval above timed out), the recorded tool result is
                    // final — never open a second approval whose Allowed arm
                    // would still execute the tool behind that result.
                    if let Some(bypass) = failure_hook
                        .sandbox_bypass_request
                        .filter(|_| fatal_turn.is_none())
                    {
                        self.metrics.sandbox_blocked += 1;
                        self.emit(
                            writer,
                            &OutputMessage::hook_notification(
                                "sandbox-failure-handler",
                                &bypass.reason,
                                Some(&tc.id),
                                Some("ask"),
                            ),
                        );
                        let request_id = self.next_request_id();
                        let approval_scope = CoreAuditScope::request(
                            &run_id,
                            Some(&turn_id),
                            &request_id,
                            Some(&tc.id),
                        );
                        let audit_ref = self.audit.record_approval_requested(
                            approval_scope,
                            &tc.name,
                            "sandbox_bypass",
                            Some(hash_json(&serde_json::json!({
                                "command": &bypass.original_command
                            }))),
                        );
                        // Same #1994 guard as the policy approval above: an
                        // unsent bypass panel must not become a blocking
                        // read. The sandbox failure stays as the tool result,
                        // exactly as for a denied bypass.
                        if let Err(error) = self.emit_control_request_checked(
                            writer,
                            &OutputMessage::can_use_tool_with_audit_ref(
                                &request_id,
                                &tc.name,
                                serde_json::json!({"command": &bypass.original_command}),
                                &tc.id,
                                true,
                                audit_ref,
                            ),
                        ) {
                            self.note_control_transport_failure(&request_id, &error);
                            let audit_error = self
                                .audit
                                .record_approval_emit_failed(
                                    approval_scope,
                                    &tc.name,
                                    Some("sandbox_bypass"),
                                    error.class(),
                                )
                                .err();
                            if fatal_turn.is_none() {
                                fatal_turn = Some(FatalTurn::new(
                                    control_transport_turn_error(
                                        &request_id,
                                        &error,
                                        audit_error.as_deref(),
                                    ),
                                    CONTROL_TRANSPORT_AUDIT_REASON,
                                ));
                            }
                        } else {
                            let approval_start = Instant::now();
                            let approval_result = self
                                .wait_for_approval(&request_id, Some(ToolKind::ShellExec), reader)
                                .await;
                            let (approval_status, approval_decision) =
                                approval_audit_outcome(&approval_result);
                            if !matches!(
                                &approval_result,
                                ApprovalResult::HostExecutedShell { .. }
                                    | ApprovalResult::HostExecutedCheckpoint { .. }
                            ) {
                                self.audit.record_approval_resolved(
                                    approval_scope,
                                    &tc.name,
                                    approval_status,
                                    Some("sandbox_bypass"),
                                    approval_decision,
                                    Some(approval_start.elapsed().as_millis() as u64),
                                )?;
                            }

                            match approval_result {
                                ApprovalResult::Allowed => {
                                    self.audit.record_tool_execution_started(
                                        tool_scope, &tc.name, &tool_data,
                                    )?;
                                    self.hook_system.set_hook_disabled("sandbox-guard", true);
                                    let retry_params =
                                        serde_json::json!({"command": &bypass.original_command});
                                    let retry =
                                        self.execute_tool(&tc.name, retry_params, &ctx).await;
                                    // Re-enable immediately after execute, before any other
                                    // operation. execute_tool returns ToolResult (infallible),
                                    // so this line is always reached.
                                    self.hook_system.set_hook_disabled("sandbox-guard", false);
                                    self.emit_provider_native_tool_result(writer, &tc.id, &retry);
                                    result = retry;
                                }
                                ApprovalResult::HostExecutedShell {
                                    llm_content,
                                    exit_code,
                                } => {
                                    let is_error = exit_code.is_some_and(|c| c != 0);
                                    result = ToolResult {
                                        output: llm_content,
                                        is_error,
                                    };
                                }
                                ApprovalResult::HostExecutedCheckpoint { .. } => {
                                    // Sandbox bypass can only resolve a legacy shell tool.
                                }
                                // #1940: same fail-closed contract as the
                                // policy approval above — a timed-out bypass
                                // approval ends the turn; the original sandbox
                                // failure stays as this call's tool result.
                                ApprovalResult::TimedOut => {
                                    hold_approval_timeout_fatal(&mut fatal_turn, &request_id);
                                }
                                _ => { /* denied / interrupted: keep original error */ }
                            }
                        }
                    }
                }

                self.audit.record_tool_terminal(
                    tool_scope,
                    &tc.name,
                    &tool_data,
                    result.is_error,
                    tool_start.elapsed().as_millis() as u64,
                    result.output.len() as u64,
                );
                self.messages.push(Message::tool_result(
                    &tc.id,
                    &result.output,
                    result.is_error,
                ));

                if self.loop_detector.record_action(&tc.name, &tc.arguments) {
                    self.messages
                        .push(Message::system(LoopDetector::loop_warning()));
                }

                if interrupted {
                    self.audit.record_turn_terminal(
                        turn_scope,
                        AuditOutcomeStatus::Cancelled,
                        Some("interrupted"),
                    );
                    return Ok(AgentTurnOutcome::Completed);
                }
            }
            // Also checked after the last call: a failure there never re-enters
            // the loop boundary above.
            self.promote_control_transport_failure(&mut fatal_turn);
            // A turn terminal is emitted only after every declared call has a
            // terminal event and a paired history result.
            if let Some(fatal) = fatal_turn {
                self.audit.record_turn_terminal(
                    turn_scope,
                    AuditOutcomeStatus::Failed,
                    Some(fatal.reason_code),
                );
                return Err(fatal.error);
            }
            self.audit
                .record_turn_terminal(turn_scope, AuditOutcomeStatus::Success, None);
        }

        Ok(AgentTurnOutcome::MaxTurns { limit: max_turns })
    }

    fn emit_provider_native_tool_result<W: Write>(
        &self,
        writer: &mut W,
        tool_use_id: &str,
        result: &ToolResult,
    ) {
        self.emit(
            writer,
            &OutputMessage::tool_result(
                &self.session_id,
                tool_use_id,
                &result.output,
                result.is_error,
            ),
        );
    }

    /// The M2 hook-block release: emits the provider-native error result
    /// with the machine-readable verdict marker so the client can tell a
    /// hook rejection apart from an executed-but-failed command without
    /// reading user-controlled text (#2156).
    fn emit_provider_native_hook_block_result<W: Write>(
        &self,
        writer: &mut W,
        tool_use_id: &str,
        result: &ToolResult,
    ) {
        self.emit(
            writer,
            &OutputMessage::tool_result_hook_blocked(&self.session_id, tool_use_id, &result.output),
        );
    }

    async fn execute_tool(
        &self,
        name: &str,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> ToolResult {
        let is_workspace_write = self.execution_profile.writes_workspace()
            && name == "write_file"
            && self.tools.get(name).map(|tool| tool.kind()) == Some(ToolKind::FileEdit);
        if self.execution_profile.is_brokered() && !is_workspace_write {
            return ToolResult::error(format!(
                "tool {name} cannot execute inside cosh-core under the gateway brokered profile"
            ));
        }
        let result = match self.tools.get(name) {
            Some(tool) => match tool.invoke(params, ctx).await {
                Ok(r) => r,
                Err(e) => return ToolResult::error(e),
            },
            None => return ToolResult::error(format!("Unknown tool: {name}")),
        };

        let (output, _truncated) = self.truncator.truncate(&result.output);
        ToolResult {
            output,
            is_error: result.is_error,
        }
    }

    async fn wait_for_answer<R: AsyncBufReadExt + Unpin>(
        &self,
        expected_request_id: &str,
        reader: &mut tokio::io::Lines<R>,
    ) -> Option<String> {
        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let msg: InputMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };
            match msg {
                InputMessage::ControlResponse { response } => {
                    if response.request_id != expected_request_id {
                        continue;
                    }
                    return response.response.answer;
                }
                InputMessage::ControlRequest { request, .. } => {
                    if matches!(request, ShellControlRequest::Interrupt) {
                        self.provider.cancel();
                        return None;
                    }
                }
                _ => {}
            }
        }
        None
    }

    async fn handle_shell_evidence<W, R>(
        &self,
        tool_use_id: &str,
        params: &serde_json::Value,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> ToolResult
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        let Some(action) = params.get("action").and_then(|v| v.as_str()) else {
            return ToolResult::error("cosh_shell_evidence missing required action");
        };

        let request_id = self.next_request_id();
        match action {
            "list_commands" => {
                if params.get("output_id").is_some()
                    || params.get("lines").is_some()
                    || params.get("bypass_recent_filter").is_some()
                {
                    return ToolResult::error(
                        "cosh_shell_evidence action=list_commands accepts only limit and cursor",
                    );
                }
                let limit = params
                    .get("limit")
                    .map(|v| {
                        v.as_u64().ok_or_else(|| {
                            ToolResult::error("cosh_shell_evidence limit must be an integer")
                        })
                    })
                    .transpose();
                let limit = match limit {
                    Ok(limit) => limit.unwrap_or(20).clamp(1, 100) as u16,
                    Err(result) => return result,
                };
                let cursor = match params.get("cursor") {
                    Some(serde_json::Value::Null) | None => None,
                    Some(v) => match v.as_str() {
                        Some(s) => Some(s),
                        None => {
                            return ToolResult::error(
                                "cosh_shell_evidence cursor must be a string or null",
                            );
                        }
                    },
                };
                if let Err(error) = self.emit_control_request_checked(
                    writer,
                    &OutputMessage::shell_evidence_list_commands(
                        &request_id,
                        tool_use_id,
                        limit,
                        cursor,
                    ),
                ) {
                    return self.evidence_request_emit_failed(&request_id, &error);
                }
            }
            "read_output" => {
                let Some(output_id) = params.get("output_id").and_then(|v| v.as_str()) else {
                    return ToolResult::error(
                        "cosh_shell_evidence action=read_output missing required output_id",
                    );
                };
                let direction = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tail");
                if direction != "head" && direction != "tail" {
                    return ToolResult::error(
                        "cosh_shell_evidence action=read_output direction must be head or tail",
                    );
                }
                let lines = params
                    .get("lines")
                    .map(|v| {
                        v.as_u64().ok_or_else(|| {
                            ToolResult::error(
                                "cosh_shell_evidence action=read_output lines must be an integer",
                            )
                        })
                    })
                    .transpose();
                let lines = match lines {
                    Ok(lines) => lines.unwrap_or(120).clamp(1, 300) as u16,
                    Err(result) => return result,
                };
                let bypass_recent_filter = match params.get("bypass_recent_filter") {
                    Some(value) => match value.as_bool() {
                        Some(value) => value,
                        None => {
                            return ToolResult::error(
                                "cosh_shell_evidence action=read_output bypass_recent_filter must be a boolean",
                            );
                        }
                    },
                    None => false,
                };

                if let Err(error) = self.emit_control_request_checked(
                    writer,
                    &OutputMessage::shell_evidence_read_output(
                        &request_id,
                        tool_use_id,
                        output_id,
                        direction,
                        lines,
                        bypass_recent_filter,
                    ),
                ) {
                    return self.evidence_request_emit_failed(&request_id, &error);
                }
            }
            _ => {
                return ToolResult::error(
                    "cosh_shell_evidence action must be list_commands or read_output",
                );
            }
        }

        self.wait_for_shell_evidence(&request_id, reader).await
    }

    /// Fails an evidence request that could not be sent.
    ///
    /// Returning instead of calling [`Self::wait_for_shell_evidence`] keeps the
    /// core off a read that no response can end (#1994); the flag makes the
    /// session fatal, and the tool loop promotes it before the next call runs.
    fn evidence_request_emit_failed(
        &self,
        request_id: &str,
        error: &ControlTransportError,
    ) -> ToolResult {
        self.note_control_transport_failure(request_id, error);
        ToolResult::error(format!(
            "cosh_shell_evidence request was not answered: delivery could not be confirmed ({})",
            error.class()
        ))
    }

    async fn wait_for_shell_evidence<R: AsyncBufReadExt + Unpin>(
        &self,
        expected_request_id: &str,
        reader: &mut tokio::io::Lines<R>,
    ) -> ToolResult {
        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let msg: InputMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };

            match msg {
                InputMessage::ControlResponse { response } => {
                    if response.request_id != expected_request_id {
                        continue;
                    }
                    if response.response.behavior.as_deref() != Some("shell_evidence") {
                        return ToolResult::error("cosh_shell_evidence received unknown response");
                    }
                    let Some(result) = response.response.result else {
                        return ToolResult::error("cosh_shell_evidence response missing result");
                    };
                    let is_error = result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("is_error"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                        || result
                            .metadata
                            .as_ref()
                            .and_then(|m| m.get("excerpt_status"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|status| {
                                !matches!(status, "available" | "already_delivered")
                            });
                    let is_error = is_error
                        || result
                            .metadata
                            .as_ref()
                            .and_then(|m| m.get("status"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|status| {
                                matches!(
                                    status,
                                    "unavailable" | "failed" | "redacted_confirmation_required"
                                )
                            })
                        || result
                            .metadata
                            .as_ref()
                            .and_then(|m| m.get("reason"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|reason| reason == "redacted_confirmation_required");
                    return ToolResult {
                        output: result.llm_content,
                        is_error,
                    };
                }
                InputMessage::ControlRequest { request, .. } => {
                    if matches!(request, ShellControlRequest::Interrupt) {
                        self.provider.cancel();
                        return ToolResult::error("Interrupted by user");
                    }
                }
                _ => {}
            }
        }
        ToolResult::error("cosh_shell_evidence response was not received")
    }

    async fn wait_for_approval<R: AsyncBufReadExt + Unpin>(
        &self,
        expected_request_id: &str,
        accepted_tool_kind: Option<ToolKind>,
        reader: &mut tokio::io::Lines<R>,
    ) -> ApprovalResult {
        // #1940 residual guard: this whole-wait deadline only ends the form
        // where the request never reached the shell's decision surface at
        // all — without it the turn would hang forever with no visible
        // cause. Once the shell acknowledges the request with an
        // `approval_receipt`, the shell owns its terminal state and the
        // guard is disarmed: a legitimate wait (a card pending on the user,
        // a host-executed command running) can then take as long as it
        // needs, and a dead shell still surfaces via EOF below.
        let mut deadline = Some(
            tokio::time::Instant::now()
                + approval_response_timeout(self.approval_response_timeout_default),
        );
        loop {
            let line = match deadline {
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, reader.next_line()).await {
                        Ok(Ok(Some(line))) => line,
                        Ok(Ok(None)) | Ok(Err(_)) => return ApprovalResult::Interrupted,
                        Err(_) => {
                            self.approval_timed_out.store(true, Ordering::SeqCst);
                            return ApprovalResult::TimedOut;
                        }
                    }
                }
                None => match reader.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) | Err(_) => return ApprovalResult::Interrupted,
                },
            };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let msg: InputMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };

            match msg {
                InputMessage::ApprovalReceipt { request_id } => {
                    // Disarm only the request this wait owns; a receipt for a
                    // different id (a concurrent approval observed while this
                    // wait holds the reader) is dropped, and that request
                    // simply keeps its residual guard.
                    if request_id == expected_request_id {
                        deadline = None;
                    }
                }
                InputMessage::ControlResponse { response } => {
                    if response.request_id != expected_request_id {
                        continue;
                    }
                    match response.response.behavior.as_deref() {
                        Some("allow") => {
                            if self.execution_profile.is_brokered()
                                && accepted_tool_kind == Some(ToolKind::HostedSideEffect)
                            {
                                return ApprovalResult::Denied(Some(
                                    "brokered side effect rejected generic allow; a typed Gateway result is required"
                                        .to_string(),
                                ));
                            }
                            return ApprovalResult::Allowed;
                        }
                        Some("deny") => return ApprovalResult::Denied(response.response.message),
                        Some("host_executed_shell") => {
                            if accepted_tool_kind != Some(ToolKind::ShellExec)
                                || self.execution_profile.is_brokered()
                            {
                                return ApprovalResult::Denied(Some(
                                    "host_executed_shell is only valid for shell tools".to_string(),
                                ));
                            }
                            let Some(result) = response.response.result else {
                                return ApprovalResult::Denied(Some(
                                    "host_executed_shell response missing result".to_string(),
                                ));
                            };
                            let exit_code = result
                                .metadata
                                .as_ref()
                                .and_then(|m| m.get("exit_code"))
                                .and_then(|v| v.as_i64())
                                .map(|v| v as i32);
                            return ApprovalResult::HostExecutedShell {
                                llm_content: result.llm_content,
                                exit_code,
                            };
                        }
                        Some("host_executed_checkpoint_create") => {
                            if !self.execution_profile.hosts_checkpoint() {
                                return ApprovalResult::Denied(Some(
                                    "unknown response".to_string(),
                                ));
                            }
                            if accepted_tool_kind != Some(ToolKind::HostedSideEffect)
                                || response_has_unrelated_checkpoint_fields(
                                    &response.response,
                                    false,
                                )
                            {
                                return ApprovalResult::Denied(Some(
                                    "typed checkpoint result is not valid for this request"
                                        .to_string(),
                                ));
                            }
                            let Some(result) = response.response.checkpoint_result else {
                                return ApprovalResult::Denied(Some(
                                    "typed checkpoint response missing checkpointResult"
                                        .to_string(),
                                ));
                            };
                            let result =
                                match serde_json::from_value(result) {
                                    Ok(result) => result,
                                    Err(_) => return ApprovalResult::Denied(Some(
                                        "typed checkpoint response has an invalid checkpointResult"
                                            .to_string(),
                                    )),
                                };
                            return match hosted_checkpoint_result(result) {
                                Ok(result) => ApprovalResult::HostExecutedCheckpoint { result },
                                Err(reason) => ApprovalResult::Denied(Some(reason)),
                            };
                        }
                        Some("host_executed_checkpoint_error") => {
                            if !self.execution_profile.hosts_checkpoint() {
                                return ApprovalResult::Denied(Some(
                                    "unknown response".to_string(),
                                ));
                            }
                            if accepted_tool_kind != Some(ToolKind::HostedSideEffect)
                                || response_has_unrelated_checkpoint_fields(
                                    &response.response,
                                    true,
                                )
                            {
                                return ApprovalResult::Denied(Some(
                                    "typed checkpoint error is not valid for this request"
                                        .to_string(),
                                ));
                            }
                            let Some(error) = response.response.checkpoint_error else {
                                return ApprovalResult::Denied(Some(
                                    "typed checkpoint response missing checkpointError".to_string(),
                                ));
                            };
                            let error =
                                match serde_json::from_value(error) {
                                    Ok(error) => error,
                                    Err(_) => return ApprovalResult::Denied(Some(
                                        "typed checkpoint response has an invalid checkpointError"
                                            .to_string(),
                                    )),
                                };
                            return match hosted_checkpoint_error(error) {
                                Ok(result) => ApprovalResult::HostExecutedCheckpoint { result },
                                Err(reason) => ApprovalResult::Denied(Some(reason)),
                            };
                        }
                        _ => return ApprovalResult::Denied(Some("unknown response".to_string())),
                    }
                }
                InputMessage::ControlRequest { request, .. } => {
                    if matches!(request, ShellControlRequest::Interrupt) {
                        self.provider.cancel();
                        return ApprovalResult::Interrupted;
                    }
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn max_turns_error(max_turns: u32) -> String {
    format!("Agent exceeded max turns ({max_turns})")
}

/// Audit reason code for a turn ended by a dead control transport.
const CONTROL_TRANSPORT_AUDIT_REASON: &str = "control_transport_failed";

/// Audit reason code for a turn ended by an approval that never reached a
/// decision surface before the residual deadline (#1940).
const APPROVAL_TIMEOUT_AUDIT_REASON: &str = "approval_timeout";

/// One fatal turn outcome held until every declared tool call is closed.
struct FatalTurn {
    error: String,
    reason_code: &'static str,
}

impl FatalTurn {
    fn new(error: String, reason_code: &'static str) -> Self {
        Self { error, reason_code }
    }
}

/// #1940: holds the turn-fatal for a timed-out approval, keeping the first
/// fatal in the batch. The batch still answers every declared call, then
/// the turn ends before another provider generation so a late shell-side
/// decision can never split state against a recorded "not executed".
fn hold_approval_timeout_fatal(fatal_turn: &mut Option<FatalTurn>, request_id: &str) {
    if fatal_turn.is_none() {
        *fatal_turn = Some(FatalTurn::new(
            format!(
                "approval timed out before reaching a decision surface (request_id={request_id}); the tool was not executed and the turn ends here so a late decision cannot split state"
            ),
            APPROVAL_TIMEOUT_AUDIT_REASON,
        ));
    }
}

/// Writes the fatal diagnostic without letting a broken stderr abort cleanup.
fn emit_fatal_diagnostic<W: Write>(writer: &mut W, detail: &str) {
    let _ = writeln!(writer, "cosh-core fatal: {detail}");
}

/// Why a control request that must be answered could not be sent intact.
///
/// Delivery is unknown after any of these: the Shell may have received the
/// whole line, part of it, or nothing at all.
#[derive(Debug)]
pub(crate) struct ControlTransportError {
    /// Stable failure class: `serialize`, `write`, or `flush`.
    class: &'static str,
    detail: String,
}

impl ControlTransportError {
    fn new(class: &'static str, detail: String) -> Self {
        Self { class, detail }
    }

    pub(crate) fn class(&self) -> &'static str {
        self.class
    }
}

impl std::fmt::Display for ControlTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.class, self.detail)
    }
}

/// Tool-result text for a call whose approval request could not be sent.
///
/// Not phrased as a denial: nobody decided anything, and the transcript must
/// not teach the model that the user refused. The call did not run, so it still
/// owes a result rather than an unpaired tool call.
fn approval_emit_failed_tool_error(error: &ControlTransportError) -> String {
    format!(
        "Tool call not executed: delivery of the approval request could not be confirmed ({})",
        error.class()
    )
}

/// Turn-level error for a broken control transport.
///
/// Carries the audit failure too when the terminal approval record could not be
/// persisted either: both are session-fatal, and the transport is the cause.
fn control_transport_turn_error(
    request_id: &str,
    error: &ControlTransportError,
    audit_error: Option<&str>,
) -> String {
    let base =
        format!("control transport {error} (request_id={request_id}); session cannot continue");
    match audit_error {
        Some(audit_error) => format!("{base}; audit record failed: {audit_error}"),
        None => base,
    }
}

fn response_has_unrelated_checkpoint_fields(
    response: &ControlResponseBody,
    expects_error: bool,
) -> bool {
    response.result.is_some()
        || !response.unknown_fields.is_empty()
        || response.tool_use_id.is_some()
        || response.updated_permissions.is_some()
        || response.answer.is_some()
        || response.selected_options.is_some()
        || response.provider_id.is_some()
        || response.provider_type.is_some()
        || response.values.is_some()
        || response.persist.is_some()
        || response.message.is_some()
        || if expects_error {
            response.checkpoint_result.is_some()
        } else {
            response.checkpoint_error.is_some()
        }
}

fn hosted_checkpoint_result(
    result: HostExecutedCheckpointCreateResult,
) -> Result<ToolResult, String> {
    validate_checkpoint_id(&result.checkpoint_id)?;
    match result.outcome {
        HostExecutedCheckpointCreateOutcome::Created { snapshot_id } => {
            validate_bounded_gateway_text(&snapshot_id, 1_024, "snapshot_id")?;
            Ok(ToolResult::success(format!(
                "Created checkpoint {} as snapshot {}.",
                result.checkpoint_id, snapshot_id
            )))
        }
        HostExecutedCheckpointCreateOutcome::Skipped { reason } => {
            validate_bounded_gateway_text(&reason, 4_096, "checkpoint skip reason")?;
            Ok(ToolResult::success(format!(
                "Checkpoint {} was safely skipped: {}",
                result.checkpoint_id, reason
            )))
        }
    }
}

fn hosted_checkpoint_error(error: HostExecutedCheckpointError) -> Result<ToolResult, String> {
    validate_bounded_gateway_text(&error.code, 128, "checkpoint error code")?;
    validate_bounded_gateway_text(&error.message, 4_096, "checkpoint error message")?;
    match error.outcome {
        HostExecutedCheckpointErrorOutcome::Failed => Ok(ToolResult::error(format!(
            "Checkpoint execution failed [{}]: {}",
            error.code, error.message
        ))),
        HostExecutedCheckpointErrorOutcome::Uncertain => Ok(ToolResult::error(format!(
            "Checkpoint execution is uncertain [{}]: {} Do not retry without Gateway reconciliation.",
            error.code, error.message
        ))),
    }
}

fn validate_checkpoint_id(value: &str) -> Result<(), String> {
    let body = value
        .strip_prefix("ckp_")
        .ok_or_else(|| "typed checkpoint result has an invalid checkpoint_id".to_string())?;
    let id = uuid::Uuid::parse_str(body)
        .map_err(|_| "typed checkpoint result has an invalid checkpoint_id".to_string())?;
    if id.hyphenated().to_string() != body {
        return Err("typed checkpoint result has an invalid checkpoint_id".to_string());
    }
    Ok(())
}

fn validate_bounded_gateway_text(value: &str, max_bytes: usize, field: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        Err(format!("typed checkpoint result has an invalid {field}"))
    } else {
        Ok(())
    }
}

enum ApprovalResult {
    Allowed,
    Denied(Option<String>),
    HostExecutedShell {
        llm_content: String,
        exit_code: Option<i32>,
    },
    HostExecutedCheckpoint {
        result: ToolResult,
    },
    Interrupted,
    /// #1940 residual guard: the wait exceeded the last-resort deadline,
    /// meaning the request never reached a decision surface on the shell
    /// side. Always fails the turn closed, never ends in an execution.
    TimedOut,
}

/// #1940 residual guard: hours-scale by design so legitimate waits (card
/// pending, host-executed command running) effectively never hit it. A
/// request with no receipt beyond this horizon is failed closed as
/// "shell presumed dead": the turn ends fatally rather than recording
/// "not executed" and continuing into a state split, and a late decision
/// degrades through the shell's OwnerUnavailable recovery path.
/// env override exists for tests and incident response.
const APPROVAL_RESPONSE_TIMEOUT_DEFAULT_SECS: u64 = 6 * 60 * 60;
/// A one-shot caller has no built-in approval surface, so an unanswered
/// request should fail closed instead of inheriting the terminal wait horizon.
const APPROVAL_RESPONSE_TIMEOUT_ONE_SHOT_DEFAULT_SECS: u64 = 30;
/// Upper bound for the env override: an absurd value would overflow
/// `Instant + Duration` and panic at the next approval wait.
const APPROVAL_RESPONSE_TIMEOUT_MAX_SECS: u64 = 30 * 24 * 60 * 60;

fn approval_response_timeout(fallback: Duration) -> Duration {
    let Ok(raw) = std::env::var("COSH_CORE_APPROVAL_TIMEOUT_SECS") else {
        return fallback;
    };
    match raw.parse::<u64>() {
        Ok(secs) if secs > 0 && secs <= APPROVAL_RESPONSE_TIMEOUT_MAX_SECS => {
            std::time::Duration::from_secs(secs)
        }
        _ => {
            // Loud fallback (PR #1968 review): an ignored override must not
            // leave the operator wondering why their timeout did not apply.
            tracing::warn!(
                value = %raw,
                "invalid COSH_CORE_APPROVAL_TIMEOUT_SECS (want 1..={} secs); \
                 falling back to the default approval timeout",
                APPROVAL_RESPONSE_TIMEOUT_MAX_SECS
            );
            fallback
        }
    }
}

fn hook_decision_name(decision: &HookDecision) -> &'static str {
    match decision {
        HookDecision::Allow => "allow",
        HookDecision::Block(_) => "block",
        HookDecision::HookFailure(_) => "hook_failure",
        HookDecision::Ask => "ask",
        HookDecision::Passthrough => "passthrough",
    }
}

fn pre_tool_hook_audit(result: &PreToolUseResult) -> (AuditOutcomeStatus, &'static str) {
    if !result.hook_failures.is_empty()
        && matches!(
            result.decision,
            HookDecision::Allow | HookDecision::Passthrough
        )
    {
        // A fail-open hook failed and no stronger decision stopped the tool.
        (AuditOutcomeStatus::Failed, "hook_failure")
    } else {
        (
            hook_outcome(&result.decision),
            hook_decision_name(&result.decision),
        )
    }
}

fn hook_outcome(decision: &HookDecision) -> AuditOutcomeStatus {
    match decision {
        HookDecision::Allow | HookDecision::Passthrough => AuditOutcomeStatus::Allowed,
        HookDecision::Block(_) => AuditOutcomeStatus::Denied,
        HookDecision::HookFailure(_) => AuditOutcomeStatus::Failed,
        HookDecision::Ask => AuditOutcomeStatus::Started,
    }
}

fn approval_audit_outcome(approval: &ApprovalResult) -> (AuditOutcomeStatus, &'static str) {
    match approval {
        ApprovalResult::Allowed
        | ApprovalResult::HostExecutedShell { .. }
        | ApprovalResult::HostExecutedCheckpoint { .. } => (AuditOutcomeStatus::Allowed, "allow"),
        ApprovalResult::Denied(_) => (AuditOutcomeStatus::Denied, "deny"),
        ApprovalResult::TimedOut => (AuditOutcomeStatus::Denied, "timeout"),
        ApprovalResult::Interrupted => (AuditOutcomeStatus::Cancelled, "interrupted"),
    }
}

#[derive(Default, Clone)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
    block_index: u32,
    block_closed: bool,
    /// Stream-shape facts kept for bounded diagnostics when arguments are
    /// rejected: they distinguish "provider never sent arguments" from
    /// "arguments arrived but were malformed".
    start_seen: bool,
    delta_count: u32,
    end_seen: bool,
}

#[cfg(test)]
#[path = "core/tests.rs"]
mod tests;
