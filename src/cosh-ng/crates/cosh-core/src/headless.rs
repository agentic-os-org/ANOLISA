use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::brokered_profile::{
    verify_task_only_runtime_tools, verify_workspace_checkpoint_runtime_tools,
    verify_workspace_write_runtime_tools, BrokeredCapabilityProfileIdentity,
};
use crate::cli::CliArgs;
use crate::compaction::{ContextBudget, ModelCapability};
#[cfg(test)]
use crate::config::ApprovalMode;
use crate::config::CoreConfig;
use crate::core::{AgentTurnOutcome, CoshCore};
use crate::extension::{
    ExtensionManager, RuntimeGeneration, RuntimeSnapshot, RuntimeSnapshotBuilder,
};
use crate::metrics::TurnMetrics;
use crate::protocol::{
    InputMessage, OutputMessage, ShellControlRequest, BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION,
    BROKERED_CONTROL_PROTOCOL_VERSION, BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION,
    CONTROL_PROTOCOL_VERSION,
};
use crate::session::{PersistedSession, ProviderSessionId, SessionError, SessionStore};
use crate::sls;
use crate::tool::SessionWorkspace;

mod auth;

use auth::request_auth;

/// Exit code for a session that lost the JSONL control transport.
///
/// The Shell owns recovery: it already replaces a missing terminal result when
/// the child exits non-zero, and stderr carries the reason. Staying alive on a
/// transport we cannot write to would only reproduce the #1994 hang.
const EXIT_CONTROL_TRANSPORT_FAILURE: i32 = 74;
const EXIT_PROTOCOL_MISMATCH: i32 = 65;
const EXIT_TURN_FAILURE: i32 = 1;

pub async fn run(
    args: &CliArgs,
    mut config: CoreConfig,
    project_root: PathBuf,
    workspace: SessionWorkspace,
) -> Result<i32, String> {
    apply_cli_overrides(args, &mut config);

    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    let mut session = match SessionRuntime::initialize(args, &config) {
        Ok(session) => session,
        Err(error) => {
            emit_session_error(&mut writer, args.resume.as_deref(), &error);
            return Ok(0);
        }
    };

    // Build and validate the complete runtime before authentication so invalid
    // tool selections fail without entering the interactive auth protocol.
    // Use the workspace passed from the shell host (e.g. cosh-shell) so tools
    // such as MCP filesystem servers resolve the user's project root, not the
    // cosh-core process cwd. The helper absolutizes relative paths and treats
    // empty --workspace "" as missing.
    let mut ext_manager = if args.execution_profile.is_brokered() {
        None
    } else {
        Some(ExtensionManager::new(project_root.clone()))
    };
    if !args.bare {
        if let Some(manager) = ext_manager.as_mut() {
            manager.refresh();
        }
    }
    let snapshot = if args.execution_profile.is_brokered() {
        let tools = match args.execution_profile {
            crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => {
                crate::tool::ToolRegistry::gateway_brokered_checkpoint_v1()
            }
            crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
                crate::tool::ToolRegistry::gateway_brokered_workspace_write_v1()
            }
            crate::cli::ExecutionProfile::GatewayBrokeredV1 => {
                crate::tool::ToolRegistry::gateway_brokered_v1()
            }
            crate::cli::ExecutionProfile::Legacy => {
                unreachable!("brokered snapshot branch excludes legacy execution")
            }
        };
        RuntimeSnapshot::bootstrap(
            RuntimeGeneration::healthy(
                0,
                match args.execution_profile {
                    crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => {
                        "gateway-brokered-checkpoint-v1"
                    }
                    crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
                        "gateway-brokered-workspace-write-v1"
                    }
                    crate::cli::ExecutionProfile::GatewayBrokeredV1 => {
                        // Preserve the private v3 runtime projection byte-for-byte.
                        "gateway-brokered-v1"
                    }
                    crate::cli::ExecutionProfile::Legacy => {
                        unreachable!("brokered snapshot branch excludes legacy execution")
                    }
                },
            ),
            Arc::new(tools),
        )
    } else {
        let generation_id =
            crate::extension::state::publish_next_generation(None).unwrap_or_else(|error| {
                tracing::error!(code = %error.code(), "failed to persist extension generation: {error}");
                1
            });
        let Some(manager) = ext_manager.as_mut() else {
            unreachable!("legacy profile always constructs its extension manager");
        };
        RuntimeSnapshotBuilder::new(manager, &config, project_root.clone(), generation_id)
            .with_shell_evidence(args.enable_shell_evidence_tool)
            .with_skill_loading(!args.bare)
            .with_tool_selection(args.tools.as_deref())
            .build()
            .await
    };
    if let Some(diagnostic) = snapshot
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tool_selection_invalid")
    {
        let error = diagnostic.message.clone();
        snapshot.mcp.shutdown().await;
        let message = OutputMessage::result_error_with_code(
            session.record.session_id.as_str(),
            &error,
            Some("InvalidToolSelection"),
        );
        if let Ok(json) = serde_json::to_string(&message) {
            let _ = writeln!(writer, "{json}");
            let _ = writer.flush();
        }
        eprintln!("[cosh-core] {error}");
        return Ok(2);
    }
    for diagnostic in &snapshot.diagnostics {
        tracing::warn!(code = %diagnostic.code, "{}", diagnostic.message);
    }

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    // --- Auth check: if no API key, request auth from Shell ---
    let mut buffered_lines: Vec<String> = Vec::new();
    let provider = if crate::needs_auth(&config) {
        match request_auth(&mut config, &mut lines, &mut writer, &mut buffered_lines).await {
            Some(p) => p,
            None => {
                // Auth failed/cancelled, use mock provider
                Box::new(crate::provider::mock::MockProvider::text_only(
                    "Authentication required. Please configure API key via environment variable or config.toml.",
                )) as Box<dyn crate::provider::ContentGenerator>
            }
        }
    } else {
        crate::create_provider(&config)
    };

    let resolved = config.resolve_provider();
    let extra_params = resolved.extra_params.clone();
    session.finalize_model(&resolved.model, args.model.is_some());

    tracing::debug!(
        generation = snapshot.generation.id,
        declared_agents = snapshot.agents.list().len(),
        executable_agents = 0,
        "loaded extension runtime snapshot"
    );
    let mut engine = CoshCore::new_with_snapshot_and_session_id(
        config,
        provider,
        snapshot,
        session.record.session_id.to_string(),
        project_root,
        workspace,
        args.execution_profile,
    );
    engine.set_session_resumed(session.resumed);
    let live_extension_runtime = crate::registry::LiveExtensionRuntime::new(
        engine.extension_generation.clone(),
        !args.execution_profile.is_brokered() && args.enable_shell_evidence_tool,
        !args.execution_profile.is_brokered() && !args.bare,
        (!args.execution_profile.is_brokered())
            .then(|| args.tools.clone())
            .flatten(),
    );
    engine.extra_params = extra_params;
    engine.messages = session.record.messages.clone();
    engine.compaction.load_state(
        session.record.compaction.clone(),
        session.record.compaction_revision,
    );
    if !session.record.model.is_empty() {
        engine.model = session.record.model.clone();
    }
    if let Some(ref prompt) = args.prompt {
        engine.use_one_shot_approval_timeout();
        if !session.resumable() {
            engine.emit(
                &mut writer,
                &OutputMessage::system_init(
                    &engine.session_id,
                    &engine.model,
                    engine.tool_names(),
                    false,
                ),
            );
        }
        let start = std::time::Instant::now();
        let turn_result = engine
            .handle_user_message(prompt, &mut lines, &mut writer)
            .await;
        let persist_result = session.persist(&engine);
        let upload_handle;
        match combine_turn_and_persist(turn_result, persist_result) {
            Ok(()) => {
                let duration = start.elapsed();
                let result_msg = OutputMessage::Result {
                    subtype: Some("success".to_string()),
                    is_error: false,
                    result: Some("completed".to_string()),
                    errors: None,
                    error_code: None,
                    max_turns: None,
                    session_error_code: None,
                    session_error_phase: None,
                    session_id: Some(engine.session_id.clone()),
                    env_delta: None,
                    duration_ms: Some(duration.as_millis() as u64),
                };
                // Emit the result before awaiting telemetry so a stalled
                // metadata probe does not delay the visible turn result.
                engine.emit(&mut writer, &result_msg);
                upload_handle = sls::emit(&engine.build_sls_record(duration)).await;
                session.recommend_auto_compaction(&mut engine, &mut writer);
            }
            Err(failure) => {
                let err_msg = failure.output_message(&engine.session_id);
                // Emit the error result before awaiting telemetry so a stalled
                // metadata probe does not delay the visible turn result.
                engine.emit(&mut writer, &err_msg);
                upload_handle = sls::emit(&engine.build_sls_record(start.elapsed())).await;
            }
        }
        let turn_exit_code = if engine.approval_timed_out() {
            EXIT_TURN_FAILURE
        } else {
            0
        };
        let transport_failed = engine.control_transport_failure().is_some();
        engine.shutdown_extension_runtime().await;
        // Await the standalone upload handle with a 1-second grace period
        // reserved for the POST itself. The region probe (also bounded, see
        // `metadata_probe_deadline`) already completed before this handle was
        // returned, so the upload budget is not consumed by discovery. Telemetry
        // is best-effort and a slow or unreachable network must not delay the
        // CLI exit.
        if let Some(handle) = upload_handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        }
        return Ok(if transport_failed {
            EXIT_CONTROL_TRANSPORT_FAILURE
        } else {
            turn_exit_code
        });
    }

    let mut extensions = HeadlessExtensionRuntime {
        manager: ext_manager.as_mut(),
        live: &live_extension_runtime,
    };
    let mut protocol = ProtocolState::new(args.execution_profile);

    // Replay any lines that were buffered during the auth wait
    for buffered_line in buffered_lines {
        match process_input_line(
            &buffered_line,
            &mut engine,
            &mut lines,
            &mut writer,
            args,
            &mut session,
            &mut extensions,
            &mut protocol,
        )
        .await
        {
            InputLineResult::Continue => {}
            InputLineResult::Shutdown => {
                engine.shutdown_extension_runtime().await;
                return Ok(0);
            }
            InputLineResult::InvalidJson => {
                engine.shutdown_extension_runtime().await;
                return Ok(1);
            }
            InputLineResult::ProtocolMismatch => {
                engine.shutdown_extension_runtime().await;
                return Ok(EXIT_PROTOCOL_MISMATCH);
            }
        }
        if engine.control_transport_failure().is_some() {
            engine.shutdown_extension_runtime().await;
            return Ok(EXIT_CONTROL_TRANSPORT_FAILURE);
        }
    }

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        match process_input_line(
            &line,
            &mut engine,
            &mut lines,
            &mut writer,
            args,
            &mut session,
            &mut extensions,
            &mut protocol,
        )
        .await
        {
            InputLineResult::Continue => {}
            InputLineResult::Shutdown => {
                engine.shutdown_extension_runtime().await;
                return Ok(0);
            }
            InputLineResult::InvalidJson => {
                engine.shutdown_extension_runtime().await;
                return Ok(1);
            }
            InputLineResult::ProtocolMismatch => {
                engine.shutdown_extension_runtime().await;
                return Ok(EXIT_PROTOCOL_MISMATCH);
            }
        }
        // Checked after every line, not only after a turn: once the transport
        // is gone the process can neither answer nor be answered.
        if engine.control_transport_failure().is_some() {
            engine.shutdown_extension_runtime().await;
            return Ok(EXIT_CONTROL_TRANSPORT_FAILURE);
        }
    }
    engine.shutdown_extension_runtime().await;
    Ok(0)
}

enum InputLineResult {
    Continue,
    Shutdown,
    InvalidJson,
    ProtocolMismatch,
}

struct ProtocolState {
    profile: crate::cli::ExecutionProfile,
    initialized: bool,
}

impl ProtocolState {
    fn new(profile: crate::cli::ExecutionProfile) -> Self {
        Self {
            profile,
            initialized: false,
        }
    }
}

fn negotiate_profile(
    profile: crate::cli::ExecutionProfile,
    protocol_version: Option<u32>,
    requested_profile: Option<&str>,
    requested_capability_profile: Option<&BrokeredCapabilityProfileIdentity>,
    runtime_tools: &[String],
) -> Result<(), String> {
    if profile.is_brokered() {
        let expected_version = brokered_protocol_version(profile);
        if protocol_version != Some(expected_version) {
            return Err(format!(
                "gateway brokered profile requires exact control protocol version {expected_version}"
            ));
        }
        if requested_profile != Some(profile.wire_name()) {
            return Err(format!(
                "gateway brokered profile requires exact execution_profile {}",
                profile.wire_name()
            ));
        }
        let requested_capability_profile = requested_capability_profile.ok_or_else(|| {
            "gateway brokered profile requires a capability_profile identity".to_string()
        })?;
        match profile {
            crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => {
                requested_capability_profile
                    .verify_workspace_checkpoint_v1()
                    .map_err(str::to_owned)?;
                verify_workspace_checkpoint_runtime_tools(runtime_tools).map_err(str::to_owned)?;
            }
            crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
                requested_capability_profile
                    .verify_workspace_write_v1()
                    .map_err(str::to_owned)?;
                verify_workspace_write_runtime_tools(runtime_tools).map_err(str::to_owned)?;
            }
            crate::cli::ExecutionProfile::GatewayBrokeredV1 => {
                requested_capability_profile
                    .verify_task_only_v1()
                    .map_err(str::to_owned)?;
                verify_task_only_runtime_tools(runtime_tools).map_err(str::to_owned)?;
            }
            crate::cli::ExecutionProfile::Legacy => {
                unreachable!("brokered negotiation excludes legacy execution")
            }
        }
        return Ok(());
    }

    if protocol_version.is_some_and(|version| version != CONTROL_PROTOCOL_VERSION) {
        return Err(format!(
            "unsupported control protocol version {}; expected exact version {CONTROL_PROTOCOL_VERSION}",
            protocol_version.unwrap_or_default()
        ));
    }
    if requested_profile.is_some_and(|requested| requested != profile.wire_name()) {
        return Err(format!(
            "legacy launch profile cannot acknowledge execution_profile {}",
            requested_profile.unwrap_or_default()
        ));
    }
    Ok(())
}

fn brokered_protocol_version(profile: crate::cli::ExecutionProfile) -> u32 {
    match profile {
        crate::cli::ExecutionProfile::GatewayBrokeredV1 => BROKERED_CONTROL_PROTOCOL_VERSION,
        crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => {
            BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION
        }
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
            BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION
        }
        crate::cli::ExecutionProfile::Legacy => CONTROL_PROTOCOL_VERSION,
    }
}

fn brokered_capability_profile(
    profile: crate::cli::ExecutionProfile,
) -> BrokeredCapabilityProfileIdentity {
    match profile {
        crate::cli::ExecutionProfile::GatewayBrokeredV1 => {
            BrokeredCapabilityProfileIdentity::task_only_v1()
        }
        crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => {
            BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1()
        }
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => {
            BrokeredCapabilityProfileIdentity::workspace_write_v1()
        }
        crate::cli::ExecutionProfile::Legacy => {
            unreachable!("legacy execution has no brokered capability profile")
        }
    }
}

/// Processes a single JSONL input line.
async fn process_input_line<W, R>(
    line: &str,
    engine: &mut CoshCore,
    lines: &mut tokio::io::Lines<R>,
    writer: &mut W,
    args: &CliArgs,
    session: &mut SessionRuntime,
    extensions: &mut HeadlessExtensionRuntime<'_>,
    protocol: &mut ProtocolState,
) -> InputLineResult
where
    W: io::Write,
    R: AsyncBufReadExt + Unpin,
{
    let msg: InputMessage = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => {
            const ERROR: &str = "failed to parse stdin line as JSON";
            tracing::debug!("{ERROR}: {e}");
            engine.emit(
                writer,
                &OutputMessage::result_error_with_code(
                    &engine.session_id,
                    ERROR,
                    Some("InvalidJsonlInput"),
                ),
            );
            return InputLineResult::InvalidJson;
        }
    };

    if let Some(manager) = extensions.manager.as_deref_mut() {
        match extensions
            .live
            .refresh_linked_runtime(&engine.config, manager)
            .await
        {
            Ok(true) => {
                engine.drain_retired_extension_snapshots().await;
                if let Err(error) = extensions.live.persist_current_generation() {
                    tracing::error!("failed to persist linked extension generation: {error}");
                }
            }
            Ok(false) => {}
            Err(error) => tracing::warn!("linked extension reload deferred: {error}"),
        }
    }

    match msg {
        InputMessage::ControlRequest {
            request_id,
            request,
        } => match request {
            ShellControlRequest::Initialize {
                fire_session_start,
                protocol_version,
                capabilities,
                execution_profile,
                capability_profile,
            } => {
                engine.client_capabilities = capabilities;
                let runtime_tools = engine.tool_names();
                let negotiation = negotiate_profile(
                    protocol.profile,
                    protocol_version,
                    execution_profile.as_deref(),
                    capability_profile.as_ref(),
                    &runtime_tools,
                );
                let expected_capability_profile = protocol
                    .profile
                    .is_brokered()
                    .then(|| brokered_capability_profile(protocol.profile));
                if let Err(error) = negotiation {
                    let expected_version = if protocol.profile.is_brokered() {
                        brokered_protocol_version(protocol.profile)
                    } else {
                        CONTROL_PROTOCOL_VERSION
                    };
                    engine.emit(
                        writer,
                        &OutputMessage::initialize_negotiation_error(
                            &request_id,
                            expected_version,
                            protocol
                                .profile
                                .is_brokered()
                                .then(|| protocol.profile.wire_name()),
                            expected_capability_profile,
                            error,
                        ),
                    );
                    return InputLineResult::ProtocolMismatch;
                }
                protocol.initialized = true;
                engine.emit(
                    writer,
                    &OutputMessage::initialize_success_for_profile(
                        &request_id,
                        if protocol.profile.is_brokered() {
                            brokered_protocol_version(protocol.profile)
                        } else {
                            CONTROL_PROTOCOL_VERSION
                        },
                        protocol
                            .profile
                            .is_brokered()
                            .then(|| protocol.profile.wire_name()),
                        expected_capability_profile,
                        protocol
                            .profile
                            .is_brokered()
                            .then(|| runtime_tools.clone()),
                        !protocol.profile.is_brokered() && args.enable_shell_evidence_tool,
                    ),
                );
                let init_msg = OutputMessage::system_init(
                    &engine.session_id,
                    &engine.model,
                    runtime_tools,
                    session.resumable(),
                );
                engine.emit(writer, &init_msg);

                // Only the shell-owned transport may suppress SessionStart.
                // Generic headless clients keep the historical lifecycle even
                // if they send the optional control field themselves.
                if !protocol.profile.is_brokered()
                    && (fire_session_start || !args.cosh_shell_transport)
                {
                    // ─── Hook: SessionStart ───
                    let cwd_str = engine.cwd().to_string_lossy().to_string();
                    let ss_result = engine
                        .hook_system
                        .fire_session_start(&engine.session_id, &cwd_str)
                        .await;
                    engine
                        .audit
                        .record_session_hook_decision("session_start", "observed");
                    for n in &ss_result.notifications {
                        engine.emit(
                            writer,
                            &OutputMessage::hook_notification(
                                &n.hook_name,
                                &n.message,
                                None,
                                n.decision.as_deref(),
                            ),
                        );
                    }
                    if let Some(ref ctx) = ss_result.additional_context {
                        engine
                            .messages
                            .push(crate::provider::Message::system(&format!(
                                "[Hook context] {ctx}"
                            )));
                    }
                }

                // A resumed session may already exceed the soft threshold;
                // surface the same background-compaction recommendation here.
                session.recommend_auto_compaction(engine, writer);
            }
            ShellControlRequest::Interrupt => {
                engine.provider.cancel();
            }
            ShellControlRequest::Shutdown => return InputLineResult::Shutdown,
            ShellControlRequest::SwitchModel { model } => {
                if protocol.profile.is_brokered() {
                    engine.emit(
                        writer,
                        &OutputMessage::result_error_with_code(
                            &engine.session_id,
                            "switch_model is disabled by the gateway brokered profile",
                            Some("BrokeredProfileViolation"),
                        ),
                    );
                    return InputLineResult::Continue;
                }
                engine.model = model.clone();
                engine.emit(
                    writer,
                    &OutputMessage::system_status(&format!("model_switched:{model}")),
                );
            }
            ShellControlRequest::ReloadConfig => {
                if protocol.profile.is_brokered() {
                    engine.emit(
                        writer,
                        &OutputMessage::result_error_with_code(
                            &engine.session_id,
                            "reload_config is disabled by the gateway brokered profile",
                            Some("BrokeredProfileViolation"),
                        ),
                    );
                    return InputLineResult::Continue;
                }
                engine.config =
                    load_runtime_config(args, std::path::Path::new(session.workspace_scope()));
                engine.emit(writer, &OutputMessage::system_status("config_reloaded"));
            }
            ShellControlRequest::ConfigOverride {
                approval_mode,
                allowed_tools: _,
            } => {
                if protocol.profile.is_brokered() {
                    engine.emit(
                        writer,
                        &OutputMessage::result_error_with_code(
                            &engine.session_id,
                            "config_override is disabled by the gateway brokered profile",
                            Some("BrokeredProfileViolation"),
                        ),
                    );
                    return InputLineResult::Continue;
                }
                if let Some(mode) = approval_mode {
                    engine.config.agent.approval_mode = mode;
                }
                engine.emit(
                    writer,
                    &OutputMessage::system_status("config_override_applied"),
                );
            }
        },

        InputMessage::User {
            message,
            session_id,
            shell_context,
            ..
        } => {
            if protocol.profile.is_brokered() && !protocol.initialized {
                engine.emit(
                    writer,
                    &OutputMessage::result_error_with_code(
                        &engine.session_id,
                        "gateway brokered profile requires a successful v2 initialize before user input",
                        Some("BrokeredProfileNotInitialized"),
                    ),
                );
                return InputLineResult::ProtocolMismatch;
            }
            if let Some(sid) = session_id {
                if !sid.is_empty() && sid != "default" && sid != engine.session_id {
                    let error = format!(
                        "session identity conflict: initialized {}, received {sid}",
                        engine.session_id
                    );
                    engine.emit(
                        writer,
                        &OutputMessage::result_error(&engine.session_id, &error),
                    );
                    return InputLineResult::Continue;
                }
            }
            if let Some(ctx) = shell_context {
                engine.shell_context = Some(ctx);
            }

            // Reset per-turn metrics before each user message
            engine.metrics = TurnMetrics::default();
            let start = std::time::Instant::now();

            let raw_user_input = args
                .cosh_shell_transport
                .then_some(message.raw_user_input.as_deref())
                .flatten();
            let turn_result = engine
                .handle_user_message_with_raw_input(&message.content, raw_user_input, lines, writer)
                .await;
            let persist_result = session.persist(engine);
            match combine_turn_and_persist(turn_result, persist_result) {
                Ok(()) => {
                    let duration = start.elapsed();
                    let result_msg = OutputMessage::Result {
                        subtype: Some("success".to_string()),
                        is_error: false,
                        result: Some("completed".to_string()),
                        errors: None,
                        error_code: None,
                        max_turns: None,
                        session_error_code: None,
                        session_error_phase: None,
                        session_id: Some(engine.session_id.clone()),
                        env_delta: None,
                        duration_ms: Some(duration.as_millis() as u64),
                    };
                    // Emit the result before awaiting telemetry so a stalled
                    // metadata probe does not delay the visible turn result.
                    engine.emit(writer, &result_msg);
                    // Interactive mode: the process stays alive, so fire-and-forget is safe.
                    let _ = sls::emit(&engine.build_sls_record(duration)).await;
                    // Idle boundary: the Agent run finished and its transcript
                    // was persisted, so background compaction is safe now. The
                    // shell owns the compactor process so its prompt returns
                    // immediately; this process only reports the pressure.
                    session.recommend_auto_compaction(engine, writer);
                }
                Err(failure) => {
                    let err_msg = failure.output_message(&engine.session_id);
                    // Emit the error result before awaiting telemetry so a stalled
                    // metadata probe does not delay the visible turn result.
                    engine.emit(writer, &err_msg);
                    let _ = sls::emit(&engine.build_sls_record(start.elapsed())).await;
                }
            }
            engine.drain_retired_extension_snapshots().await;
            if extensions.manager.is_some() {
                if let Err(error) = extensions.live.persist_current_generation() {
                    tracing::error!("failed to persist activated extension generation: {error}");
                }
            }
        }

        InputMessage::ControlResponse { .. } => {}
        // Receipts only matter to an interactive `wait_for_approval` loop;
        // the headless path never waits on one.
        InputMessage::ApprovalReceipt { .. } => {}
        InputMessage::RegistryRequest {
            request_id,
            domain,
            action,
            params,
        } => {
            let Some(manager) = extensions.manager.as_deref_mut() else {
                engine.emit(
                    writer,
                    &OutputMessage::RegistryResponse {
                        request_id,
                        success: false,
                        data: None,
                        error: Some(
                            "registry mutation is disabled by the gateway brokered profile"
                                .to_string(),
                        ),
                    },
                );
                return InputLineResult::Continue;
            };
            let response = crate::registry::handle_registry_request(
                &request_id,
                &domain,
                &action,
                &params,
                &mut engine.config,
                manager,
                None,
                Some(extensions.live),
            )
            .await;
            engine.emit(writer, &response);
            engine.drain_retired_extension_snapshots().await;
        }
    }
    InputLineResult::Continue
}

struct HeadlessExtensionRuntime<'a> {
    manager: Option<&'a mut ExtensionManager>,
    live: &'a crate::registry::LiveExtensionRuntime,
}

struct SessionRuntime {
    store: Option<SessionStore>,
    workspace_scope: String,
    record: PersistedSession,
    auto_persist: bool,
    resumed: bool,
}

impl SessionRuntime {
    fn initialize(args: &CliArgs, config: &CoreConfig) -> Result<Self, SessionError> {
        let workspace = args.workspace_root();
        let store = match SessionStore::for_workspace(&config.session.persist_dir, &workspace) {
            Ok(store) => Some(store),
            // Resume cannot proceed without the store, but a fresh turn can:
            // degrade to a non-resumable session instead of refusing to run.
            Err(error) => {
                if args.resume.is_some() {
                    return Err(error);
                }
                tracing::warn!("session persistence disabled: {error}");
                None
            }
        };
        let workspace_scope = store
            .as_ref()
            .map(|store| store.workspace_scope().to_string())
            .unwrap_or_else(|| workspace.to_string_lossy().into_owned());
        let record = match (args.resume.as_deref(), store.as_ref()) {
            (Some(value), Some(store)) => {
                let session_id = ProviderSessionId::parse(value)?;
                store.load(&session_id)?
            }
            _ => PersistedSession::new(
                ProviderSessionId::new(),
                workspace_scope.clone(),
                String::new(),
                Vec::new(),
            ),
        };
        Ok(Self {
            store,
            workspace_scope,
            record,
            auto_persist: config.session.auto_persist,
            resumed: args.resume.is_some(),
        })
    }

    fn workspace_scope(&self) -> &str {
        &self.workspace_scope
    }

    fn finalize_model(&mut self, resolved_model: &str, explicit_override: bool) {
        if !self.resumed || explicit_override || self.record.model.is_empty() {
            self.record.model = resolved_model.to_string();
        }
    }

    fn persist(&mut self, engine: &CoshCore) -> Result<(), SessionError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        if !self.auto_persist {
            return Ok(());
        }
        self.record.messages = engine.messages.clone();
        self.record.model = engine.model.clone();
        // Emergency in-run compaction updates the projection in memory; it
        // commits together with the transcript it belongs to.
        self.record.compaction = engine.compaction.state().cloned();
        // The revision clock is persisted even when no projection survives, so
        // the next commit cannot reuse an already published revision.
        self.record.compaction_revision = engine.compaction.revision();
        store.persist(&mut self.record)
    }

    fn resumable(&self) -> bool {
        self.auto_persist && self.store.is_some()
    }

    /// Runs idle-boundary automatic compaction when the soft threshold is
    /// crossed, with per-context-revision failure suppression.
    /// Emits a background-compaction recommendation when the soft threshold
    /// is crossed at an idle boundary.
    ///
    /// The recommendation is a cheap synchronous status line; the shell owns
    /// the actual compactor process (`cosh-core --compact`) so this process
    /// can exit and the user gets the normal shell prompt back immediately.
    /// The payload carries the context revision (generation + projection
    /// revision) so the shell can suppress retrigger loops per revision.
    fn recommend_auto_compaction<W: io::Write>(&self, engine: &mut CoshCore, writer: &mut W) {
        let policy = &engine.config.session.compaction;
        if !policy.enabled || !policy.auto || !self.resumable() {
            return;
        }
        let capability = ModelCapability::resolve(
            policy,
            engine.config.agent.session_token_limit,
            &engine.model,
        );
        let prefix_tokens = engine.estimate_prefix_tokens();
        let budget = ContextBudget::compute(capability, prefix_tokens, policy);
        let history_tokens = engine.effective_history_tokens(prefix_tokens);
        if !budget.over_trigger(history_tokens) {
            return;
        }
        let compacted_through = engine
            .compaction
            .state()
            .map(|state| state.compacted_through)
            .unwrap_or(0);
        match crate::compaction::has_new_compactable_prefix(
            &engine.messages,
            policy.preserve_recent_runs,
            compacted_through,
        ) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(
                    session_id = %self.record.session_id,
                    "automatic compaction preflight rejected the transcript: {error}"
                );
                return;
            }
        }
        // Bind to the durable revision clock, not the live projection: this
        // recommendation is emitted right after a persist, and a projection
        // that sanitization later rejects must not make it look stale.
        let projection_revision = engine.compaction.revision();
        // Versioned protocol: the shell must be able to bind the recommendation
        // to the exact session and context revision it was emitted for, and
        // reject anything malformed. Field order is fixed:
        //   compaction_recommended_v1:<session-id>:<generation>:<revision>:<history>:<usable>
        engine.emit(
            writer,
            &OutputMessage::system_status(&format!(
                "compaction_recommended_v1:{}:{}:{}:{}:{}",
                self.record.session_id,
                self.record.generation,
                projection_revision,
                history_tokens,
                budget.usable_history
            )),
        );
    }
}

struct TurnFailure {
    message: String,
    error_code: Option<&'static str>,
    max_turns: Option<u32>,
    session_error_code: Option<&'static str>,
}

impl TurnFailure {
    fn output_message(&self, session_id: &str) -> OutputMessage {
        match self.session_error_code {
            Some(code) => {
                OutputMessage::session_result_error(session_id, &self.message, code, "persist")
            }
            None if self.error_code == Some("max_turns") && self.max_turns.is_some() => {
                OutputMessage::max_turns_result_error(
                    session_id,
                    &self.message,
                    self.max_turns.unwrap_or_default(),
                )
            }
            None => {
                OutputMessage::result_error_with_code(session_id, &self.message, self.error_code)
            }
        }
    }
}

fn combine_turn_and_persist(
    turn: Result<AgentTurnOutcome, String>,
    persist: Result<(), SessionError>,
) -> Result<(), TurnFailure> {
    match (turn, persist) {
        (Ok(AgentTurnOutcome::Completed), Ok(())) => Ok(()),
        (Ok(AgentTurnOutcome::MaxTurns { limit }), Ok(())) => Err(TurnFailure {
            message: crate::core::max_turns_error(limit),
            error_code: Some("max_turns"),
            max_turns: Some(limit),
            session_error_code: None,
        }),
        (Err(turn_error), Ok(())) => Err(TurnFailure {
            error_code: None,
            max_turns: None,
            message: turn_error,
            session_error_code: None,
        }),
        (Ok(AgentTurnOutcome::Completed), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "session persistence failed [{}]: {persist_error}",
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
        (Ok(AgentTurnOutcome::MaxTurns { limit }), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "{}; session persistence failed [{}]: {persist_error}",
                crate::core::max_turns_error(limit),
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
        (Err(turn_error), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "{turn_error}; session persistence failed [{}]: {persist_error}",
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
    }
}

fn emit_session_error<W: io::Write>(
    writer: &mut W,
    requested_id: Option<&str>,
    error: &SessionError,
) {
    let session_id = requested_id.unwrap_or("<new>");
    let message = format!("session recovery failed [{}]: {error}", error.code());
    if let Ok(json) = serde_json::to_string(&OutputMessage::session_result_error(
        session_id,
        &message,
        error.code(),
        "load",
    )) {
        let _ = writeln!(writer, "{json}");
        let _ = writer.flush();
    }
}

fn apply_cli_overrides(args: &CliArgs, config: &mut CoreConfig) {
    if let Some(ref model) = args.model {
        config.ai.active_model = Some(model.clone());
    }
    if let Some(ref mode) = args.approval_mode {
        config.agent.approval_mode = *mode;
    }
    if let Some(ref tools) = args.allowed_tools {
        config.agent.allowed_tools = tools
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(ToString::to_string)
            .collect();
    }
}

fn load_runtime_config(args: &CliArgs, workspace: &std::path::Path) -> CoreConfig {
    let mut config = if args.bare {
        CoreConfig::load_bare()
    } else {
        CoreConfig::load_for_workspace(workspace)
    };
    apply_cli_overrides(args, &mut config);
    config
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use super::*;

    #[test]
    fn brokered_profile_requires_exact_v3_profile_and_inventory() {
        let profile = crate::cli::ExecutionProfile::GatewayBrokeredV1;
        let capability_profile = BrokeredCapabilityProfileIdentity::task_only_v1();
        let runtime_tools = vec!["ask_user_question".to_string()];
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_ok());
        assert!(negotiate_profile(
            profile,
            None,
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            None,
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some("legacy"),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_err());
    }

    #[test]
    fn brokered_profile_rejects_digest_and_inventory_drift() {
        let profile = crate::cli::ExecutionProfile::GatewayBrokeredV1;
        let mut drifted_identity = BrokeredCapabilityProfileIdentity::task_only_v1();
        drifted_identity.manifest_digest = "0".repeat(64);

        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&drifted_identity),
            &["ask_user_question".to_string()],
        )
        .unwrap_err()
        .contains("manifest digest"));
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&BrokeredCapabilityProfileIdentity::task_only_v1()),
            &["ask_user_question".to_string(), "shell".to_string()],
        )
        .unwrap_err()
        .contains("tool inventory"));
    }

    #[test]
    fn checkpoint_profile_requires_exact_v4_profile_and_inventory() {
        let profile = crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1;
        let capability_profile = BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1();
        let runtime_tools = vec![
            "ask_user_question".to_string(),
            "workspace_checkpoint_create".to_string(),
        ];

        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_ok());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&BrokeredCapabilityProfileIdentity::task_only_v1()),
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &[
                "workspace_checkpoint_create".to_string(),
                "ask_user_question".to_string()
            ],
        )
        .is_err());
    }

    #[test]
    fn task_only_v3_rejects_checkpoint_v4_identity_and_inventory() {
        let profile = crate::cli::ExecutionProfile::GatewayBrokeredV1;

        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION),
            Some(crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1.wire_name()),
            Some(&BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1()),
            &[
                "ask_user_question".to_string(),
                "workspace_checkpoint_create".to_string(),
            ],
        )
        .is_err());
    }

    #[test]
    fn workspace_write_profile_requires_exact_v5_profile_and_inventory() {
        let profile = crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1;
        let capability_profile = BrokeredCapabilityProfileIdentity::workspace_write_v1();
        let runtime_tools = vec!["ask_user_question".to_string(), "write_file".to_string()];

        assert!(negotiate_profile(
            profile,
            Some(BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_ok());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CHECKPOINT_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&BrokeredCapabilityProfileIdentity::task_only_v1()),
            &runtime_tools,
        )
        .is_err());
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_WORKSPACE_WRITE_CONTROL_PROTOCOL_VERSION),
            Some(profile.wire_name()),
            Some(&capability_profile),
            &["write_file".to_string(), "ask_user_question".to_string()],
        )
        .is_err());
    }

    #[test]
    fn legacy_profile_keeps_unversioned_and_v1_compatibility() {
        let profile = crate::cli::ExecutionProfile::Legacy;
        assert!(negotiate_profile(profile, None, None, None, &[]).is_ok());
        assert!(
            negotiate_profile(profile, Some(CONTROL_PROTOCOL_VERSION), None, None, &[],).is_ok()
        );
        assert!(negotiate_profile(
            profile,
            Some(BROKERED_CONTROL_PROTOCOL_VERSION),
            Some("gateway_brokered_v1"),
            None,
            &[],
        )
        .is_err());
    }

    #[test]
    fn typed_max_turn_outcome_sets_structured_result_fields() {
        let failure = combine_turn_and_persist(Ok(AgentTurnOutcome::MaxTurns { limit: 5 }), Ok(()))
            .expect_err("max-turn outcome");

        assert_eq!(failure.error_code, Some("max_turns"));
        assert_eq!(failure.max_turns, Some(5));
        assert_eq!(failure.message, "Agent exceeded max turns (5)");
        assert_eq!(failure.session_error_code, None);
    }

    #[test]
    fn matching_provider_error_text_stays_an_ordinary_failure() {
        let failure = combine_turn_and_persist(Err(crate::core::max_turns_error(5)), Ok(()))
            .expect_err("provider error");

        assert_eq!(failure.error_code, None);
        assert_eq!(failure.max_turns, None);
        assert_eq!(failure.message, "Agent exceeded max turns (5)");
        assert_eq!(failure.session_error_code, None);
    }

    #[test]
    fn bare_reload_keeps_project_config_isolated() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let project_config = workspace.path().join(".copilot-shell/config.toml");
        fs::create_dir_all(project_config.parent().expect("config parent"))
            .expect("create project config directory");
        fs::write(
            project_config,
            "[agent]\napproval_mode = \"project-only-mode\"\n",
        )
        .expect("write project config");
        let args = CliArgs::try_parse_from(["cosh-core", "--headless", "--bare"])
            .expect("parse bare args");

        let config = load_runtime_config(&args, workspace.path());

        assert_eq!(config.agent.approval_mode, ApprovalMode::Recommend);
        assert!(!config.session.auto_persist);
    }
}
