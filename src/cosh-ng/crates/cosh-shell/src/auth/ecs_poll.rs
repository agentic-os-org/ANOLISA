use std::io::{self, Write};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapter::{AdapterInstance, EcsProbeTask};
use crate::i18n::{I18n, MessageId};
use crate::runtime::prelude::AuthResponse;
use crate::runtime::state::InlineState;

use super::completion::finish_auth_configuration;
use super::menu::{EcsRamRolePrepare, SysomMenu};
use super::prompt::{clear_active_auth_panel, render_current_auth_panel};
use super::provider_management::{core_auth_configure, AuthConfigureFailure};
use super::runtime::{self, AuthBackend, AuthPhase, RuntimeAuthState};

const INTERVAL: Duration = Duration::from_secs(2);
const WAIT_LIMIT: Duration = Duration::from_secs(200);
const OPERATION_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Preparing,
    Checking,
    Waiting,
    Submitting,
    Cancelling,
    TimedOut,
    Failed,
    Unknown,
    Editing,
}

#[derive(Debug)]
enum Operation {
    Probe(EcsProbeTask, bool),
    Configure(Option<JoinHandle<Result<(), AuthConfigureFailure>>>),
}

impl Drop for Operation {
    fn drop(&mut self) {
        if let Self::Configure(worker) = self {
            if let Some(worker) = worker.take() {
                if worker.join().is_err() {
                    tracing::warn!("ECS configuration worker failed during shutdown");
                }
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct EcsFlow {
    id: String,
    stage: Stage,
    operation: Option<Operation>,
    operation_deadline: Option<Instant>,
    deadline: Option<Instant>,
    next_check: Instant,
    stop_stage: Stage,
    error: Option<String>,
    challenge: Option<EcsRamRolePrepare>,
    waiting_for_refresh: bool,
    cleanup_reported: bool,
}

impl EcsFlow {
    fn new(auth: &RuntimeAuthState, now: Instant) -> Self {
        let challenge = match &auth.phase {
            AuthPhase::AliyunEcsChallenge {
                instance_id,
                console_url,
            } => Some(EcsRamRolePrepare {
                instance_id: instance_id.clone(),
                console_url: console_url.clone(),
                values: Default::default(),
            }),
            _ => None,
        };
        Self {
            id: auth.id.clone(),
            stage: if challenge.is_some() {
                Stage::Checking
            } else {
                Stage::Preparing
            },
            operation: None,
            operation_deadline: None,
            // WAIT_LIMIT budgets waiting for role authorization, so it starts with a
            // challenge; prepare stays bounded by its per-operation deadline.
            deadline: challenge.as_ref().map(|_| now + WAIT_LIMIT),
            next_check: now,
            stop_stage: Stage::Cancelling,
            error: None,
            challenge,
            waiting_for_refresh: false,
            cleanup_reported: false,
        }
    }

    fn stop(&mut self, destination: Stage, now: Instant) {
        if self.stage == Stage::Cancelling {
            if destination == Stage::Cancelling {
                self.stop_stage = Stage::Cancelling;
            }
            return;
        }
        if let Some(Operation::Probe(task, _)) = &self.operation {
            task.cancel();
        }
        self.stage = Stage::Cancelling;
        self.stop_stage = destination;
        self.operation_deadline = Some(now + OPERATION_LIMIT);
    }
}

pub(super) fn handles(auth: &RuntimeAuthState) -> bool {
    matches!(
        auth.phase,
        AuthPhase::PreparingMenu
            | AuthPhase::AliyunEcsPreparing
            | AuthPhase::AliyunEcsChallenge { .. }
    )
}

pub(super) fn option_count(state: &InlineState) -> usize {
    usize::from(state.auth.ecs.as_ref().is_some_and(|flow| {
        flow.operation.is_none()
            && matches!(flow.stage, Stage::TimedOut | Stage::Failed | Stage::Unknown)
    }))
}

pub(super) fn question(state: &InlineState) -> (String, Vec<String>, bool) {
    let i18n = I18n::new(state.language);
    let flow = state.auth.ecs.as_ref();
    let stage = flow.map_or(Stage::Checking, |flow| flow.stage);
    let id = match stage {
        Stage::Preparing | Stage::Checking => MessageId::AuthEcsChecking,
        Stage::Waiting if flow.is_some_and(|flow| flow.waiting_for_refresh) => {
            MessageId::AuthEcsRefreshing
        }
        Stage::Waiting => MessageId::AuthEcsWaiting,
        Stage::Submitting => MessageId::AuthEcsSaving,
        Stage::Cancelling if flow.is_some_and(|flow| flow.cleanup_reported) => {
            MessageId::AuthEcsCleanupFailed
        }
        Stage::Cancelling => MessageId::AuthEcsCancelling,
        Stage::TimedOut => MessageId::AuthEcsTimedOut,
        Stage::Unknown if flow.is_some_and(|flow| flow.operation.is_some()) => {
            MessageId::AuthEcsCleanupFailed
        }
        Stage::Unknown => MessageId::AuthEcsUnknown,
        _ => MessageId::AuthEcsFailed,
    };
    let mut text = i18n.t(id).to_string();
    if let Some(error) = flow.and_then(|flow| flow.error.as_ref()) {
        text.push('\n');
        text.push_str(error);
    }
    if !matches!(stage, Stage::Submitting | Stage::Cancelling) {
        text.push('\n');
        text.push_str(i18n.t(MessageId::AuthEcsCancelHint));
    }
    let options = if option_count(state) == 1 {
        vec![i18n
            .t(if stage == Stage::Unknown {
                MessageId::AuthEcsReturn
            } else {
                MessageId::AuthEcsRetry
            })
            .to_string()]
    } else {
        Vec::new()
    };
    (
        text,
        options,
        matches!(stage, Stage::Waiting | Stage::TimedOut)
            && !flow.is_some_and(|flow| flow.waiting_for_refresh),
    )
}

fn redraw<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<()> {
    let cancelling = state
        .auth
        .ecs
        .as_ref()
        .is_some_and(|flow| flow.stage == Stage::Cancelling);
    // Cancellation finishes the submitted capture instead of arming a new input owner.
    if !cancelling {
        if let Some(auth) = state.auth.state.as_mut() {
            auth.field_capture_revision = auth.field_capture_revision.wrapping_add(1);
        }
    }
    clear_active_auth_panel(state, output)?;
    render_current_auth_panel(state, output)
}

pub(super) fn cancel<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<bool> {
    let Some(flow) = state.auth.ecs.as_mut() else {
        return Ok(false);
    };
    if matches!(flow.operation, Some(Operation::Configure(_))) || flow.stage == Stage::Submitting {
        return Ok(true);
    }
    flow.stop(Stage::Cancelling, Instant::now());
    if flow.operation.is_none() {
        state.auth.ecs = None;
        return Ok(false);
    }
    redraw(state, output)?;
    Ok(true)
}

pub(super) fn answer<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    if option_count(state) == 0 {
        return Ok(());
    }
    let Some(flow) = state.auth.ecs.as_mut() else {
        return Ok(());
    };
    if flow.stage == Stage::Unknown {
        state.auth.ecs = None;
        state.auth.state = None;
        clear_active_auth_panel(state, output)?;
        return runtime::trigger_auth_from_slash(adapter, state, output);
    }
    flow.stage = if flow.challenge.is_some() {
        Stage::Checking
    } else {
        Stage::Preparing
    };
    let now = Instant::now();
    flow.deadline = flow.challenge.as_ref().map(|_| now + WAIT_LIMIT);
    flow.next_check = now;
    flow.error = None;
    flow.cleanup_reported = false;
    redraw(state, output)
}

pub(super) fn start_configuration<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    if state.shell_exited {
        return Ok(());
    }
    let Some(auth) = state.auth.state.as_ref() else {
        return Ok(());
    };
    let mut flow = state
        .auth
        .ecs
        .take()
        .unwrap_or_else(|| EcsFlow::new(auth, Instant::now()));
    if flow.operation.is_some() {
        state.auth.ecs = Some(flow);
        return Ok(());
    }
    let response = AuthResponse {
        request_id: auth.request_id.clone(),
        provider_id: auth
            .editing_provider_name
            .clone()
            .or_else(|| auth.collected_values.get("provider_id").cloned())
            .unwrap_or_else(|| auth.current_provider().id.clone()),
        provider_type: Some(auth.current_provider().id.clone()),
        values: auth.collected_values.clone(),
        persist: true,
    };
    let adapter = adapter.clone();
    match thread::Builder::new()
        .name("cosh-auth-ecs-save".into())
        .spawn(move || core_auth_configure(&adapter, &response))
    {
        Ok(worker) => {
            flow.operation = Some(Operation::Configure(Some(worker)));
            flow.operation_deadline = Some(Instant::now() + Duration::from_secs(12));
            flow.stage = Stage::Submitting;
            if let (Some(auth), Some(challenge)) =
                (state.auth.state.as_mut(), flow.challenge.as_ref())
            {
                set_challenge(auth, challenge.clone());
            }
        }
        Err(_) => {
            flow.stage = Stage::Failed;
            flow.error = Some("Unable to start configuration worker".into());
        }
    }
    state.auth.ecs = Some(flow);
    redraw(state, output)
}

pub(super) fn set_challenge(auth: &mut RuntimeAuthState, prepare: EcsRamRolePrepare) {
    auth.collected_values
        .insert("auth_source".into(), "ecs_ram_role".into());
    for key in ["access_key_id", "access_key_secret", "security_token"] {
        auth.collected_values.remove(key);
    }
    auth.phase = AuthPhase::AliyunEcsChallenge {
        instance_id: prepare.instance_id,
        console_url: prepare.console_url,
    };
}

enum Reply {
    Prepared(Result<Value, String>),
    Verified(Result<Value, String>),
    Configured(Result<(), AuthConfigureFailure>),
}

fn take_reply(flow: &mut EcsFlow) -> Option<Reply> {
    let reply = match flow.operation.as_mut()? {
        Operation::Probe(task, prepare) => {
            let result = task.try_finish()?;
            if *prepare {
                Reply::Prepared(result)
            } else {
                Reply::Verified(result)
            }
        }
        Operation::Configure(worker) => {
            if !worker.as_ref()?.is_finished() {
                return None;
            }
            Reply::Configured(worker.take()?.join().unwrap_or_else(|_| {
                Err(AuthConfigureFailure {
                    message: "Configuration worker failed; save result is unknown".into(),
                    code: None,
                })
            }))
        }
    };
    flow.operation = None;
    flow.operation_deadline = None;
    Some(reply)
}

pub(crate) fn poll<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let now = Instant::now();
    let orphaned = state.auth.ecs.as_ref().is_some_and(|flow| {
        state
            .auth
            .state
            .as_ref()
            .is_none_or(|auth| auth.id != flow.id)
    });
    if orphaned {
        let mut flow = state.auth.ecs.take().expect("orphaned flow present");
        if flow.stage != Stage::Cancelling {
            flow.stop(Stage::Cancelling, now);
        }
        let _ = take_reply(&mut flow);
        if flow.operation.is_some() {
            if flow
                .operation_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                flow.cleanup_reported = true;
            }
            state.auth.ecs = Some(flow);
        }
        return Ok(());
    }
    let Some(auth) = state.auth.state.as_ref() else {
        return Ok(());
    };
    if !handles(auth) && state.auth.ecs.is_none() {
        return Ok(());
    }
    let preparing_menu = auth.phase == AuthPhase::PreparingMenu;
    let mut flow = state
        .auth
        .ecs
        .take()
        .unwrap_or_else(|| EcsFlow::new(auth, now));
    let previous = flow.stage;
    let was_waiting_for_refresh = flow.waiting_for_refresh;
    let had_operation = flow.operation.is_some();
    if !handles(auth)
        && flow.operation.is_none()
        && (auth.phase != AuthPhase::FillingField
            || auth.collected_values.get("auth_source").map(String::as_str) != Some("ecs_ram_role"))
    {
        return Ok(());
    }
    if flow.stage == Stage::Editing && handles(auth) {
        flow.stage = Stage::Checking;
        flow.deadline = Some(now + WAIT_LIMIT);
        flow.next_check = now;
        flow.error = None;
    }
    if state.shell_exited {
        // Closing can cancel a read-only probe, never a save or its unknown result.
        if matches!(flow.operation, Some(Operation::Probe(..)))
            || matches!(
                flow.stage,
                Stage::Preparing | Stage::Checking | Stage::Waiting | Stage::Cancelling
            )
        {
            flow.stop(Stage::Cancelling, now);
        }
    } else if matches!(
        flow.stage,
        Stage::Preparing | Stage::Checking | Stage::Waiting
    ) && flow.deadline.is_some_and(|deadline| now >= deadline)
    {
        flow.stop(Stage::TimedOut, now);
    } else if matches!(
        flow.stage,
        Stage::Preparing | Stage::Checking | Stage::Waiting
    ) && flow
        .operation_deadline
        .is_some_and(|deadline| now >= deadline)
    {
        flow.error = Some("ECS credential check timed out".into());
        flow.stop(Stage::Failed, now);
    }
    let completed_save = if matches!(flow.stage, Stage::Submitting | Stage::Unknown) {
        take_reply(&mut flow)
    } else {
        None
    };
    if flow.stage == Stage::Submitting
        && completed_save.is_none()
        && flow
            .operation_deadline
            .is_some_and(|deadline| now >= deadline)
    {
        flow.stage = Stage::Unknown;
    }
    let reply = completed_save.or_else(|| take_reply(&mut flow));
    if flow.stage == Stage::Cancelling {
        if flow.operation.is_none() {
            if flow.stop_stage == Stage::Cancelling {
                return runtime::cancel_auth_panel(state, output);
            }
            flow.stage = flow.stop_stage;
        } else if !flow.cleanup_reported
            && flow
                .operation_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            flow.cleanup_reported = true;
            state.auth.ecs = Some(flow);
            return redraw(state, output);
        }
    } else if let Some(reply) = reply {
        match reply {
            Reply::Prepared(result) if preparing_menu => {
                runtime::finish_sysom_menu_prepare(
                    state
                        .auth
                        .state
                        .as_mut()
                        .expect("auth retained while preparing menu"),
                    result,
                );
                return redraw(state, output);
            }
            Reply::Prepared(Ok(value)) if value["mode"] == "manual" => {
                let auth = state
                    .auth
                    .state
                    .as_mut()
                    .expect("auth retained while preparing");
                auth.sysom = SysomMenu::on_manual();
                auth.collected_values.remove("auth_source");
                auth.phase = AuthPhase::FillingField;
                auth.current_field = auth.first_editable_field();
                if auth
                    .current_field_info()
                    .is_some_and(|field| field.name == "provider_id")
                    && auth.collected_values.contains_key("provider_id")
                {
                    auth.current_field = auth.editable_field_at_or_after(auth.current_field + 1);
                }
                auth.load_current_field_input();
                return redraw(state, output);
            }
            Reply::Prepared(Ok(value)) => {
                if let (Some(instance), Some(url)) =
                    (value["instance_id"].as_str(), value["console_url"].as_str())
                {
                    if value["mode"] == "ecs_ram_role" && !instance.is_empty() && !url.is_empty() {
                        let prepare = EcsRamRolePrepare {
                            instance_id: instance.into(),
                            console_url: url.into(),
                            values: Default::default(),
                        };
                        set_challenge(
                            state.auth.state.as_mut().expect("auth retained"),
                            prepare.clone(),
                        );
                        flow.challenge = Some(prepare);
                        flow.deadline = Some(now + WAIT_LIMIT);
                        flow.stage = Stage::Checking;
                        flow.next_check = now;
                    } else {
                        flow.stage = Stage::Failed;
                    }
                } else {
                    flow.stage = Stage::Failed;
                }
            }
            Reply::Verified(Ok(value)) if value["status"] == "ready" => {
                let active = state
                    .auth
                    .state
                    .as_ref()
                    .is_some_and(|auth| auth.backend == AuthBackend::ActiveRun);
                if active {
                    clear_active_auth_panel(state, output)?;
                    return runtime::send_auth_response(Some(adapter), state, output);
                }
                state.auth.ecs = Some(flow);
                return start_configuration(adapter, state, output);
            }
            Reply::Verified(Ok(value))
                if value["status"] == "not_ready"
                    && matches!(
                        value["reason"].as_str(),
                        Some("role_missing" | "credentials_expired")
                    ) =>
            {
                flow.stage = Stage::Waiting;
                flow.waiting_for_refresh = value["reason"] == "credentials_expired";
                flow.next_check = now + INTERVAL;
            }
            Reply::Prepared(Err(error)) | Reply::Verified(Err(error)) => {
                flow.stage = Stage::Failed;
                flow.error = Some(error);
            }
            Reply::Verified(Ok(_)) => {
                flow.stage = Stage::Failed;
                flow.error = Some("Invalid ECS verification result".into());
            }
            Reply::Configured(result) => match result {
                Ok(()) => {
                    let auth = state.auth.state.take().expect("auth retained while saving");
                    let label = auth.current_provider().label.clone();
                    state.auth.completed_ids.insert(auth.id);
                    runtime::clear_observed_model_after_provider_change(state);
                    clear_active_auth_panel(state, output)?;
                    return finish_auth_configuration(state, output, &label);
                }
                Err(error) if error.code.as_deref() == Some("credential_source_unavailable") => {
                    flow.stage = Stage::Checking;
                    flow.waiting_for_refresh = false;
                    flow.next_check = now + INTERVAL;
                }
                Err(error) => {
                    let auth = state
                        .auth
                        .state
                        .as_mut()
                        .expect("auth retained while saving");
                    let field = error
                        .focus_field(&auth.current_provider().id)
                        .and_then(|name| {
                            auth.current_provider()
                                .fields
                                .iter()
                                .position(|field| field.name == name && !field.secret)
                        });
                    if let Some(index) = field {
                        if auth.current_provider().fields[index].name == "provider_id"
                            && auth.editing_provider_name.is_none()
                        {
                            auth.default_provider_id = false;
                        }
                        if auth.field_is_editable(index) {
                            auth.phase = AuthPhase::FillingField;
                            auth.current_field = index;
                            auth.load_current_field_input();
                            auth.field_error = Some(error.message.clone());
                            flow.stage = Stage::Editing;
                        } else {
                            flow.stage = Stage::Failed;
                        }
                    } else {
                        flow.stage = if error.code.is_none() {
                            Stage::Unknown
                        } else {
                            Stage::Failed
                        };
                    }
                    flow.error = Some(error.message);
                }
            },
        }
    }
    if !state.shell_exited
        && flow.operation.is_none()
        && now >= flow.next_check
        && matches!(
            flow.stage,
            Stage::Preparing | Stage::Checking | Stage::Waiting
        )
    {
        let prepare = flow.stage == Stage::Preparing;
        let result = match adapter {
            AdapterInstance::CoshCore(core) => {
                core.start_ecs_probe(if prepare { "prepare" } else { "verify" })
            }
            _ => Err(io::Error::other("ECS authentication requires cosh-core")),
        };
        match result {
            Ok(task) => {
                flow.operation = Some(Operation::Probe(task, prepare));
                flow.operation_deadline = Some(now + OPERATION_LIMIT);
            }
            Err(_) => {
                flow.stage = Stage::Failed;
                flow.error = Some("Unable to start ECS credential check".into());
            }
        }
    }
    if preparing_menu && flow.stage == Stage::Failed && flow.operation.is_none() {
        runtime::finish_sysom_menu_prepare(
            state
                .auth
                .state
                .as_mut()
                .expect("auth retained while preparing menu"),
            Err(flow
                .error
                .take()
                .unwrap_or_else(|| "ECS menu prepare failed".into())),
        );
        return redraw(state, output);
    }
    let changed = previous != flow.stage
        || was_waiting_for_refresh != flow.waiting_for_refresh
        || (previous == Stage::Unknown && had_operation && flow.operation.is_none());
    state.auth.ecs = Some(flow);
    let width_changed = state.questions.active_panel_width.is_some_and(|width| {
        width != crate::ui::RatatuiInlineRenderer::for_terminal().panel_standard_width()
    });
    if changed || width_changed {
        redraw(state, output)?;
    }
    Ok(())
}

pub(crate) fn shutdown(state: &mut InlineState) {
    if let Some(mut flow) = state.auth.ecs.take() {
        if let Some(Operation::Probe(task, _)) = &flow.operation {
            task.cancel();
        }
        flow.operation.take();
    }
}

#[cfg(test)]
#[path = "ecs_poll_tests.rs"]
mod tests;
