//! Standalone ACP doctor and prompt command orchestration.

use super::*;

pub(super) fn doctor(args: ProfileArgs, reporter: &Reporter) -> Result<u8, CliError> {
    let interrupted = install_interrupt_handler()?;
    let (driver, _) = launch_driver(&args)?;
    initialize_session(&driver, reporter, &interrupted)?;
    driver
        .shutdown()
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    wait_for_terminal(&driver, reporter, &interrupted)?;
    reporter.event("doctor_ok", json!({"profile": profile_name(args.profile)}))?;
    Ok(0)
}

pub(super) fn run(args: RunArgs, reporter: &Reporter) -> Result<u8, CliError> {
    let evidence_path = permission_evidence_path(&args)?;
    let prompt = read_prompt(args.prompt_file.as_ref())?;
    let interrupted = install_interrupt_handler()?;
    let (driver, workspace) = launch_driver(&args.profile)?;
    let mut permissions = LocalPermissionHandler::new(&args, &workspace, evidence_path);
    initialize_session(&driver, reporter, &interrupted)?;
    driver
        .prompt(prompt)
        .map_err(|error| CliError::Runtime(error.to_string()))?;

    let mut cancel_sent = false;
    loop {
        if interrupted.load(Ordering::Relaxed) && !cancel_sent {
            driver
                .control()
                .cancel()
                .map_err(|error| CliError::Runtime(error.to_string()))?;
            cancel_sent = true;
        }
        match driver.receive_timeout(EVENT_POLL_INTERVAL) {
            Ok(AcpSessionEvent::Observation(observation)) => {
                if let Some(exit) =
                    handle_observation(&driver, reporter, observation, Some(&mut permissions))?
                {
                    driver
                        .shutdown()
                        .map_err(|error| CliError::Runtime(error.to_string()))?;
                    wait_for_terminal(&driver, reporter, &interrupted)?;
                    return Ok(exit);
                }
            }
            Ok(AcpSessionEvent::Terminal(terminal)) => {
                report_terminal(reporter, &terminal)?;
                return terminal_exit(terminal.kind).map(Ok)?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliError::Runtime("ACP event channel closed".into()));
            }
        }
    }
}

fn launch_driver(args: &ProfileArgs) -> Result<(AcpSessionDriver, PathBuf), CliError> {
    let request = AcpRuntimeProfileRequest::from_current_environment(
        args.profile.into(),
        args.adapter.clone(),
        &args.workspace,
    );
    let resolved = AcpRuntimeProfileResolver::resolve(request)
        .map_err(|error| CliError::Profile(error.to_string()))?;
    let workspace = resolved.workspace().to_path_buf();
    let config = AcpSessionDriverConfig::new(
        resolved.launch_spec(),
        resolved.bind_client_config(AcpV1ClientConfig::new(
            "cosh-gateway",
            env!("CARGO_PKG_VERSION"),
            MAX_ACP_FRAME_BYTES,
        )),
        resolved.workspace(),
    );
    let driver =
        AcpSessionDriver::launch(config).map_err(|error| CliError::Runtime(error.to_string()))?;
    Ok((driver, workspace))
}

fn initialize_session(
    driver: &AcpSessionDriver,
    reporter: &Reporter,
    interrupted: &AtomicBool,
) -> Result<(), CliError> {
    check_interrupted(driver, interrupted)?;
    driver
        .initialize()
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    wait_for_observation(driver, reporter, interrupted, |observation| {
        matches!(observation, AcpV1Observation::Initialized { .. })
    })?;
    check_interrupted(driver, interrupted)?;
    driver
        .open_session()
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    wait_for_observation(driver, reporter, interrupted, |observation| {
        matches!(observation, AcpV1Observation::SessionOpened { .. })
    })
}

fn wait_for_observation(
    driver: &AcpSessionDriver,
    reporter: &Reporter,
    interrupted: &AtomicBool,
    expected: impl Fn(&AcpV1Observation) -> bool,
) -> Result<(), CliError> {
    let deadline = std::time::Instant::now() + EVENT_DEADLINE;
    loop {
        check_interrupted(driver, interrupted)?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(CliError::Runtime(
                "ACP event delivery deadline exceeded".into(),
            ));
        }
        match driver.receive_timeout(remaining.min(EVENT_POLL_INTERVAL)) {
            Ok(AcpSessionEvent::Observation(observation)) => {
                let matched = expected(&observation.observation);
                handle_observation(driver, reporter, observation, None)?;
                if matched {
                    return Ok(());
                }
            }
            Ok(AcpSessionEvent::Terminal(terminal)) => {
                report_terminal(reporter, &terminal)?;
                return terminal_exit(terminal.kind).map(|_| ());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliError::Runtime("ACP event channel closed".into()));
            }
        }
    }
}

fn handle_observation(
    driver: &AcpSessionDriver,
    reporter: &Reporter,
    observation: AcpSessionObservation,
    permissions: Option<&mut LocalPermissionHandler>,
) -> Result<Option<u8>, CliError> {
    let sequence = observation.sequence;
    let report = |event, fields| reporter.event(event, with_observation_sequence(sequence, fields));
    match observation.observation {
        AcpV1Observation::Initialized { agent_info, .. } => {
            report(
                "initialized",
                json!({"agent": agent_info.map(|info| json!({
                    "name": info.name, "version": info.version
                }))}),
            )?;
        }
        AcpV1Observation::SessionOpened { session_id } => {
            report("session_opened", json!({"session_id":session_id}))?;
        }
        AcpV1Observation::SessionUpdate { session_id, update } => {
            let text = update
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str);
            if let Some(text) = text {
                report(
                    "session_update",
                    json!({"session_id":session_id, "text":text}),
                )?;
            } else {
                report(
                    "session_diagnostic",
                    json!({"session_id":session_id, "kind":"non_text_update"}),
                )?;
            }
        }
        AcpV1Observation::PermissionRequested(request) => {
            let request_id = request.request_id.clone();
            let resolved = permissions
                .ok_or_else(|| CliError::Permission("permission UI is unavailable".into()))
                .and_then(|handler| handler.resolve(&request));
            let (decision, decision_name) = match resolved {
                Ok(value) => value,
                Err(error) => {
                    let _ =
                        driver.answer_permission(request_id, AcpV1PermissionDecision::Cancelled);
                    return Err(error);
                }
            };
            driver
                .answer_permission(request_id, decision)
                .map_err(|error| CliError::Runtime(error.to_string()))?;
            report("permission_decided", json!({"decision":decision_name}))?;
        }
        AcpV1Observation::PromptCancelledWithPendingPermissions {
            session_id,
            request_ids,
        } => {
            report(
                "permission_callbacks_abandoned",
                abandoned_permission_fields(&session_id, &request_ids),
            )?;
            return Ok(Some(prompt_exit_code(AcpV1StopReason::Cancelled)));
        }
        AcpV1Observation::PromptFinished {
            session_id,
            stop_reason,
        } => {
            report(
                "prompt_finished",
                json!({"session_id":session_id, "stop_reason":stop_reason_name(stop_reason)}),
            )?;
            return Ok(Some(prompt_exit_code(stop_reason)));
        }
        AcpV1Observation::RequestFailed {
            request,
            code,
            message,
        } => {
            report(
                "request_failed",
                json!({"request":format!("{request:?}"), "code":code, "message":message}),
            )?;
            return Err(CliError::Agent);
        }
        AcpV1Observation::UnsupportedClientRequest { request_id, method } => {
            report(
                "unsupported_request",
                json!({"request_id":request_id.to_string(), "method":method}),
            )?;
        }
        AcpV1Observation::UnsupportedNotification { method } => {
            report("unsupported_notification", json!({"method":method}))?;
        }
        AcpV1Observation::TransportClosed => {
            return Err(CliError::Runtime("ACP transport closed".into()));
        }
    }
    Ok(None)
}

pub(super) fn abandoned_permission_fields(
    session_id: &str,
    request_ids: &[AcpV1RequestId],
) -> Value {
    json!({
        "session_id": session_id,
        "pending_count": request_ids.len(),
        "stop_reason": "cancelled",
    })
}

pub(super) fn prompt_exit_code(stop_reason: AcpV1StopReason) -> u8 {
    match stop_reason {
        AcpV1StopReason::EndTurn => 0,
        AcpV1StopReason::Cancelled => EXIT_CANCELLED,
        AcpV1StopReason::MaxTokens
        | AcpV1StopReason::MaxTurnRequests
        | AcpV1StopReason::Refusal
        | AcpV1StopReason::Unsupported => EXIT_AGENT,
    }
}

pub(super) fn with_observation_sequence(sequence: u64, mut fields: Value) -> Value {
    if let Some(fields) = fields.as_object_mut() {
        fields.insert("sequence".to_owned(), Value::from(sequence));
    }
    fields
}

struct LocalPermissionHandler {
    mode: PermissionMode,
    profile: &'static str,
    workspace: Vec<u8>,
    evidence_path: PathBuf,
    evidence: Option<FilePermissionEvidenceSink>,
}

impl LocalPermissionHandler {
    fn new(args: &RunArgs, workspace: &Path, evidence_path: PathBuf) -> Self {
        #[cfg(unix)]
        use std::os::unix::ffi::OsStrExt;

        #[cfg(unix)]
        let workspace = workspace.as_os_str().as_bytes().to_vec();
        #[cfg(not(unix))]
        let workspace = workspace.to_string_lossy().as_bytes().to_vec();
        Self {
            mode: args.permission,
            profile: profile_name(args.profile.profile),
            workspace,
            evidence_path,
            evidence: None,
        }
    }

    fn resolve(
        &mut self,
        request: &cosh_gateway::runtime::AcpV1PermissionRequest,
    ) -> Result<(AcpV1PermissionDecision, &'static str), CliError> {
        let occurred_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CliError::Permission("system clock precedes the Unix epoch".into()))?
            .as_millis()
            .try_into()
            .map_err(|_| CliError::Permission("system clock is out of range".into()))?;
        let context = PermissionEvidenceContext {
            profile: self.profile,
            canonical_workspace: &self.workspace,
            actor_uid: nix::unistd::Uid::effective().as_raw(),
            occurred_at_ms,
        };
        if self.evidence.is_none() {
            self.evidence = Some(
                FilePermissionEvidenceSink::open_in_private_state(&self.evidence_path)
                    .map_err(|error| CliError::Permission(error.to_string()))?,
            );
        }
        let evidence = self
            .evidence
            .as_mut()
            .ok_or_else(|| CliError::Permission("permission evidence is unavailable".into()))?;
        let decision = match self.mode {
            PermissionMode::Deny => {
                resolve_permission(CancelPermissionPresenter, evidence, context, request)?
            }
            PermissionMode::Prompt => match local_terminal_presenter() {
                Some(presenter) => resolve_permission(presenter, evidence, context, request)?,
                None => resolve_permission(CancelPermissionPresenter, evidence, context, request)?,
            },
        };
        let name = match &decision {
            AcpV1PermissionDecision::Cancelled => "cancelled",
            AcpV1PermissionDecision::Selected { option_id } => request
                .options
                .iter()
                .find(|option| &option.option_id == option_id)
                .map_or("cancelled", |option| match option.kind {
                    AcpV1PermissionOptionKind::AllowOnce => "allow_once",
                    AcpV1PermissionOptionKind::RejectOnce => "reject_once",
                    _ => "cancelled",
                }),
        };
        Ok((decision, name))
    }
}

fn resolve_permission<P: PermissionPresenter>(
    presenter: P,
    evidence: &mut FilePermissionEvidenceSink,
    context: PermissionEvidenceContext<'_>,
    request: &cosh_gateway::runtime::AcpV1PermissionRequest,
) -> Result<AcpV1PermissionDecision, CliError> {
    let mut proxy = OncePermissionProxy::new(presenter, evidence);
    proxy
        .resolve(context, request)
        .map_err(|error| CliError::Permission(error.to_string()))
}

fn local_terminal_presenter() -> Option<TextPermissionPresenter<BufReader<File>, File>> {
    let terminal = File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    if !terminal.is_terminal() {
        return None;
    }
    let input = terminal.try_clone().ok()?;
    Some(TextPermissionPresenter::new(
        BufReader::new(input),
        terminal,
    ))
}

fn permission_evidence_path(args: &RunArgs) -> Result<PathBuf, CliError> {
    if let Some(path) = &args.permission_evidence {
        return if path.is_absolute() {
            Ok(path.clone())
        } else {
            Err(CliError::Permission(
                "permission evidence path must be absolute".into(),
            ))
        };
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        let state = PathBuf::from(state);
        if !state.is_absolute() {
            return Err(CliError::Permission(
                "XDG_STATE_HOME must be absolute".into(),
            ));
        }
        return Ok(state.join("cosh/gateway/permission-evidence.jsonl"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| CliError::Permission("absolute HOME is required".into()))?;
    Ok(home.join(".local/state/cosh/gateway/permission-evidence.jsonl"))
}

fn wait_for_terminal(
    driver: &AcpSessionDriver,
    reporter: &Reporter,
    interrupted: &AtomicBool,
) -> Result<(), CliError> {
    let deadline = std::time::Instant::now() + SHUTDOWN_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(CliError::Runtime(
                "ACP shutdown event deadline exceeded".into(),
            ));
        }
        match driver.receive_timeout(remaining.min(EVENT_POLL_INTERVAL)) {
            Ok(AcpSessionEvent::Observation(observation)) => {
                handle_observation(driver, reporter, observation, None)?;
            }
            Ok(AcpSessionEvent::Terminal(terminal)) => {
                report_terminal(reporter, &terminal)?;
                return terminal_exit(terminal.kind).map(|_| ());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if interrupted.load(Ordering::Relaxed) {
                    let _ = driver.control().cancel();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliError::Runtime("ACP terminal channel closed".into()));
            }
        }
    }
}

fn report_terminal(
    reporter: &Reporter,
    terminal: &cosh_gateway::runtime::AcpSessionTerminal,
) -> Result<(), CliError> {
    reporter.event(
        "terminal",
        json!({
            "kind":format!("{:?}", terminal.kind).to_ascii_lowercase(),
            "detail":terminal.detail,
        }),
    )
}

fn terminal_exit(kind: AcpSessionTerminalKind) -> Result<u8, CliError> {
    match kind {
        AcpSessionTerminalKind::Shutdown => Ok(0),
        AcpSessionTerminalKind::Cancelled => Err(CliError::Cancelled),
        AcpSessionTerminalKind::Failed => Err(CliError::Runtime("ACP session failed".into())),
    }
}

fn check_interrupted(driver: &AcpSessionDriver, interrupted: &AtomicBool) -> Result<(), CliError> {
    if interrupted.load(Ordering::Relaxed) {
        let _ = driver.control().cancel();
        Err(CliError::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn install_interrupt_handler() -> Result<Arc<AtomicBool>, CliError> {
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
        .map_err(CliError::Signal)?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&interrupted))
        .map_err(CliError::Signal)?;
    Ok(interrupted)
}
