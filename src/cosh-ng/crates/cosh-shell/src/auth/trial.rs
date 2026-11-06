use std::io::{self, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::adapter::{AdapterInstance, CoshCoreAdapter};
use crate::raw_input::RawInputCapture;
use crate::runtime::state::InlineState;
use crate::types::{AgentRequest, EcsTrialAction, EcsTrialProgress, ShellEvent};

use super::provider_management::{auth_configure_failure, AuthConfigureFailure};
use super::trial_state::{TrialFlow, TrialPhase};
use super::trial_terminal::{clear, redraw};

#[derive(Debug)]
pub(crate) struct TrialSession {
    pub(super) flow: Option<TrialFlow>,
    pub(crate) auto_use_allowed: bool,
    pub(crate) cancelled_question: bool,
    pub(super) owner: Option<String>,
    pending_invalidation: Option<String>,
    pub(super) retained: Option<AgentRequest>,
    pub(super) height: u16,
    pub(super) dimensions: Option<(u16, u16)>,
    pub(super) signature: String,
    pub(super) deadline: Option<Instant>,
    preference_worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) preference_job: Option<mpsc::Receiver<Result<(), AuthConfigureFailure>>>,
    pub(super) enabling: bool,
    pub(super) disable_save_failed: bool,
    pub(super) permanent: bool,
    pub(super) saw_activation_wait: bool,
}

impl TrialSession {
    pub(crate) fn take_invalidation(&mut self) -> Option<String> {
        self.pending_invalidation.take()
    }
    pub(crate) fn owns_run(&self, owner: &str) -> bool {
        self.flow.is_some() && self.owner.as_deref() == Some(owner)
    }
    pub(crate) fn handles_cancelled_result(&self, error: &str) -> bool {
        error.strip_prefix("provider_access:").unwrap_or(error) == "trial_cancelled"
            && self.flow.as_ref().is_some_and(|flow| {
                matches!(flow.phase, TrialPhase::Cancelling | TrialPhase::Closed)
            })
    }

    pub(crate) fn new_user_request(&mut self) {
        self.cancelled_question = false;
        self.retained = None;
    }

    pub(crate) fn is_blocking(&self) -> bool {
        self.flow.is_some() || self.preference_job.is_some()
    }
}

impl Drop for TrialSession {
    fn drop(&mut self) {
        if let Some(worker) = self.preference_worker.take() {
            if worker.join().is_err() {
                tracing::warn!("ECS trial preference worker failed during cleanup");
            }
        }
    }
}

impl Default for TrialSession {
    fn default() -> Self {
        Self {
            flow: None,
            auto_use_allowed: true,
            cancelled_question: false,
            owner: None,
            pending_invalidation: None,
            retained: None,
            height: 0,
            dimensions: None,
            signature: String::new(),
            deadline: None,
            preference_job: None,
            preference_worker: None,
            enabling: false,
            disable_save_failed: false,
            permanent: false,
            saw_activation_wait: false,
        }
    }
}

pub(crate) fn observe<W: Write>(
    state: &mut InlineState,
    owner: &str,
    progress: EcsTrialProgress,
    output: &mut W,
) -> io::Result<()> {
    if state
        .agent_run
        .active
        .as_ref()
        .is_none_or(|run| run.request.id != owner)
    {
        return Ok(());
    }
    let previous_capture = capture_id(&state.auth.ecs_trial);
    if state.auth.ecs_trial.flow.is_none() {
        if !matches!(
            progress.phase.as_str(),
            "preparing" | "waiting_activation" | "credits_preparing"
        ) {
            return Ok(());
        }
        state.auth.ecs_trial.saw_activation_wait = progress.phase == "waiting_activation";
        state.auth.ecs_trial.owner = Some(owner.into());
        state.auth.ecs_trial.retained = state
            .agent_run
            .active
            .as_ref()
            .map(|run| run.request.clone());
        state.auth.ecs_trial.flow = Some(TrialFlow::new(progress.clone()));
    } else {
        if state.auth.ecs_trial.owner.as_deref() != Some(owner) {
            return Ok(());
        }
        let Some(flow) = state.auth.ecs_trial.flow.as_mut() else {
            return Ok(());
        };
        if !flow.observe(progress.clone()) {
            return Ok(());
        }
    }
    if progress.phase == "waiting_activation" {
        state.auth.ecs_trial.saw_activation_wait = true;
    }
    let phase = state.auth.ecs_trial.flow.as_ref().map(|flow| flow.phase);
    if progress.local_terminated && phase == Some(TrialPhase::Settings) {
        state.auth.ecs_trial.deadline = None;
    }
    if phase == Some(TrialPhase::Closed) {
        state.auth.ecs_trial.pending_invalidation = previous_capture.clone();
    }
    if phase == Some(TrialPhase::Waiting)
        && matches!(progress.phase.as_str(), "ready" | "failed" | "terminated")
    {
        state.auth.ecs_trial.pending_invalidation = previous_capture;
        clear(state, output)?;
        if progress.phase == "ready" {
            state.auth.ecs_trial.flow = None;
            state.auth.ecs_trial.retained = None;
            if state.auth.ecs_trial.saw_activation_wait {
                notice(
                    state,
                    output,
                    "✓ 已开通，继续回答…",
                    "Activated; continuing your answer…",
                )?;
            }
        } else {
            state.auth.ecs_trial.cancelled_question = true;
            if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
                flow.phase = TrialPhase::Failed;
            }
        }
        return Ok(());
    }
    redraw(state, output)
}

pub(crate) fn pending_capture(state: &InlineState) -> Option<RawInputCapture> {
    let flow = state.auth.ecs_trial.flow.as_ref()?;
    let id = capture_id(&state.auth.ecs_trial)?;
    if matches!(flow.phase, TrialPhase::Closed | TrialPhase::Failed) {
        return None;
    }
    Some(RawInputCapture::TrialWaiting {
        id,
        settings: (flow.phase == TrialPhase::Settings).then_some(flow.selected),
        allow_settings: flow.phase == TrialPhase::Waiting,
    })
}

fn capture_id(trial: &TrialSession) -> Option<String> {
    let flow = trial.flow.as_ref()?;
    Some(format!(
        "trial-{}-{}-{}-{:?}",
        trial.owner.as_deref().unwrap_or_default(),
        flow.progress.flow_id,
        flow.progress.revision,
        flow.phase
    ))
}

fn send_action(state: &mut InlineState, action: EcsTrialAction) -> bool {
    let Some(flow) = state.auth.ecs_trial.flow.as_ref() else {
        return false;
    };
    state.agent_run.active.as_ref().is_some_and(|run| {
        state.auth.ecs_trial.owner.as_deref() == Some(&run.request.id)
            && run.handle.send_trial_control(&flow.progress, action)
    })
}

pub(crate) fn consume<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    for event in events {
        if event.component.as_deref() != Some("card") {
            continue;
        }
        let Some(id) = capture_id(&state.auth.ecs_trial) else {
            continue;
        };
        if event.message.as_deref() == Some("trial_control") {
            let Some((target, action)) = event
                .input
                .as_deref()
                .and_then(|input| input.rsplit_once(':'))
            else {
                continue;
            };
            if target != id
                || state
                    .auth
                    .ecs_trial
                    .flow
                    .as_ref()
                    .is_some_and(|flow| flow.phase == TrialPhase::Closed)
            {
                continue;
            }
            let in_settings = state
                .auth
                .ecs_trial
                .flow
                .as_ref()
                .is_some_and(|flow| flow.phase == TrialPhase::Settings);
            if in_settings && matches!(action, "focus_0" | "focus_1") {
                if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
                    flow.selected = usize::from(action == "focus_1");
                }
            } else if in_settings && matches!(action, "session" | "permanent") {
                state.auth.ecs_trial.permanent = action == "permanent";
                state.auth.ecs_trial.auto_use_allowed = false;
                cancel(state);
            } else if in_settings && action == "back" {
                if state
                    .auth
                    .ecs_trial
                    .flow
                    .as_ref()
                    .is_some_and(TrialFlow::can_resume)
                {
                    if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
                        flow.progress.revision = flow.progress.revision.saturating_add(1);
                        flow.phase = TrialPhase::Waiting;
                        flow.progress.phase = "waiting_activation".into();
                    }
                    if !send_action(state, EcsTrialAction::Resume) {
                        cancel(state);
                    }
                } else {
                    cancel(state);
                }
            } else if action == "settings"
                && state
                    .auth
                    .ecs_trial
                    .flow
                    .as_ref()
                    .is_some_and(|flow| flow.phase == TrialPhase::Waiting)
            {
                if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
                    flow.settings();
                }
                state.auth.ecs_trial.deadline = Some(Instant::now() + Duration::from_secs(2));
                if !send_action(state, EcsTrialAction::Pause) {
                    state.auth.ecs_trial.auto_use_allowed = false;
                    stop_transport(state);
                }
            } else if action == "cancel" {
                cancel(state);
            }
            redraw(state, output)?;
        }
    }
    Ok(())
}

fn stop_transport(state: &mut InlineState) {
    if let Some(run) = state.agent_run.active.as_ref() {
        if state.auth.ecs_trial.owner.as_deref() == Some(&run.request.id) {
            run.handle.cancel();
        }
    }
}

fn cancel(state: &mut InlineState) {
    state.auth.ecs_trial.cancelled_question = true;
    if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
        flow.progress.revision = flow.progress.revision.saturating_add(1);
        flow.phase = TrialPhase::Cancelling;
    }
    let _ = send_action(state, EcsTrialAction::Cancel);
    state.auth.ecs_trial.deadline = Some(Instant::now() + Duration::from_secs(2));
}

pub(crate) fn poll<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let preference_result = state
        .auth
        .ecs_trial
        .preference_job
        .as_ref()
        .and_then(|job| match job.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(AuthConfigureFailure {
                message: "preference worker disconnected".into(),
                code: None,
                provider_updated: false,
            })),
            Err(mpsc::TryRecvError::Empty) => None,
        });
    if let Some(result) = preference_result {
        state.auth.ecs_trial.preference_job = None;
        if let Some(worker) = state.auth.ecs_trial.preference_worker.take() {
            let _ = worker.join();
        }
        if let Err(error) = &result {
            tracing::warn!(error=%error.message,"ECS trial preference save failed");
        }
        clear(state, output)?;
        if state.auth.ecs_trial.enabling {
            if result.is_ok() || result.as_ref().is_err_and(|error| error.provider_updated) {
                state.auth.ecs_trial.auto_use_allowed = true;
                state.auth.ecs_trial.disable_save_failed = false;
                super::runtime::clear_observed_model_after_provider_change(state);
            }
            if result.as_ref().is_err_and(|error| error.provider_updated) {
                super::completion::finish_partial_configuration(state, output)?;
            } else {
                notice(
                    state,
                    output,
                    if result.is_ok() {
                        "✓ 已选用操作系统控制台模型服务，直接提问即可。"
                    } else {
                        "未能切换模型服务，请用 /auth 重试。"
                    },
                    if result.is_ok() {
                        "Selected OS Console model service. You can ask a question now."
                    } else {
                        "Unable to switch model service; retry with /auth."
                    },
                )?;
            }
        } else {
            state.auth.ecs_trial.disable_save_failed = result.is_err();
            notice(
                state,
                output,
                if result.is_ok() {
                    "✓ 以后不再提示免配置接入。问题已保留，可用 /auth 配置模型或重新启用。"
                } else {
                    "未能保存，已仅对本次会话生效。问题已保留，可用 /auth 重试。"
                },
                if result.is_ok() {
                    "Automatic OS Console access disabled. Your question is retained."
                } else {
                    "Unable to save; disabled for this session only. Your question is retained."
                },
            )?;
        }
        state.auth.ecs_trial.enabling = false;
        state.auth.ecs_trial.permanent = false;
        crate::slash::prompt::write_shell_prompt(state, output)?;
    }
    let AdapterInstance::CoshCore(core) = adapter else {
        return Ok(());
    };
    if state
        .auth
        .ecs_trial
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        if state
            .auth
            .ecs_trial
            .flow
            .as_ref()
            .is_some_and(|flow| flow.phase == TrialPhase::Pausing)
        {
            state.auth.ecs_trial.auto_use_allowed = false;
        }
        stop_transport(state);
        state.auth.ecs_trial.deadline = None;
    }
    let idle = state.agent_run.active.is_none() && core.trial_runtime_idle();
    let disconnected = idle
        && state
            .auth
            .ecs_trial
            .flow
            .as_ref()
            .is_some_and(|flow| flow.phase == TrialPhase::Waiting);
    if idle
        && state
            .auth
            .ecs_trial
            .flow
            .as_ref()
            .is_some_and(|flow| flow.phase == TrialPhase::Failed)
    {
        if let Some(code) = observe_terminal(&mut state.auth.ecs_trial) {
            let message = crate::agent::display::display_agent_error(
                &format!("provider_access:{code}"),
                &state.i18n(),
            );
            notice(state, output, &message, &message)?;
        }
    }
    if idle {
        let previous_capture = capture_id(&state.auth.ecs_trial);
        if let Some(flow) = state.auth.ecs_trial.flow.as_mut() {
            if flow.phase == TrialPhase::Pausing {
                flow.progress.local_terminated = true;
                flow.progress.resume_allowed = false;
                flow.progress.dispatch_state = "unknown".into();
                flow.phase = TrialPhase::Settings;
                state.auth.ecs_trial.deadline = None;
            } else if matches!(flow.phase, TrialPhase::Cancelling | TrialPhase::Waiting) {
                flow.phase = TrialPhase::Closed;
                state.auth.ecs_trial.pending_invalidation = previous_capture;
                state.auth.ecs_trial.cancelled_question = true;
            }
        }
    }
    if idle
        && state
            .auth
            .ecs_trial
            .flow
            .as_ref()
            .is_some_and(|flow| flow.phase == TrialPhase::Closed)
    {
        clear(state, output)?;
        state.auth.ecs_trial.flow = None;
        state.auth.ecs_trial.deadline = None;
        state.auth.ecs_trial.owner = None;
        crate::slash::prompt::write_shell_prompt(state, output)?;
        if state.auth.ecs_trial.permanent {
            begin_preference(core.clone(), false, state);
        } else {
            notice(
                state,
                output,
                if disconnected {
                    "模型服务连接已中断。问题已保留，可重新提问或用 /auth 配置模型。"
                } else if state.auth.ecs_trial.auto_use_allowed {
                    "已取消，问题已保留。"
                } else {
                    "✓ 本次会话不再提示免配置接入。问题已保留，可用 /auth 配置模型。"
                },
                if disconnected {
                    "Model service connection closed. Your question is retained; ask again or use /auth to configure a model."
                } else {
                    "Cancelled. Your question is retained; use /auth to configure a model."
                },
            )?;
        }
    }
    if idle
        && state.auth.ecs_trial.flow.is_none()
        && state.auth.ecs_trial.cancelled_question
        && state.auth.ecs_trial.owner.is_some()
    {
        state.auth.ecs_trial.owner = None;
        crate::slash::prompt::write_shell_prompt(state, output)?;
    }
    redraw(state, output)
}

pub(super) fn begin_preference(core: CoshCoreAdapter, auto_use: bool, state: &mut InlineState) {
    let (sender, receiver) = mpsc::channel();
    state.auth.ecs_trial.enabling = auto_use;
    state.auth.ecs_trial.preference_job = Some(receiver);
    state.auth.ecs_trial.preference_worker = Some(std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            let result = core
                .registry_query_classified(
                    "auth",
                    if auto_use {
                        "activate_trial"
                    } else {
                        "set_trial_preference"
                    },
                    if auto_use {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!({"auto_use":false})
                    },
                )
                .map(|_| ())
                .map_err(auth_configure_failure);
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error.message.contains("runtime is busy"))
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            break result;
        };
        let _ = sender.send(result);
    }));
}

pub(super) fn notice<W: Write>(
    state: &InlineState,
    output: &mut W,
    zh: &str,
    en: &str,
) -> io::Result<()> {
    let text = if state.language == crate::config::Language::ZhCn {
        zh
    } else {
        en
    };
    for line in text.split('\n') {
        write!(output, "{}\r\n", line.strip_suffix('\r').unwrap_or(line))?;
    }
    output.flush()
}

pub(super) fn set_preference_from_auth<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    auto_use: bool,
) -> io::Result<()> {
    let AdapterInstance::CoshCore(core) = adapter else {
        return Ok(());
    };
    super::prompt::clear_active_auth_panel(state, output)?;
    state.auth.state = None;
    begin_preference(core.clone(), auto_use, state);
    notice(
        state,
        output,
        if auto_use {
            "正在切换模型服务…"
        } else {
            "正在保存提示设置…"
        },
        if auto_use {
            "Switching model service…"
        } else {
            "Saving prompt preferences…"
        },
    )
}

#[cfg(test)]
#[path = "trial_tests.rs"]
mod tests;

pub(crate) fn before_terminal_output<W: Write>(
    trial: &mut TrialSession,
    output: &mut W,
) -> io::Result<()> {
    super::trial_terminal::clear_session(trial, output)
}

pub(crate) fn observe_terminal(trial: &mut TrialSession) -> Option<String> {
    let flow = trial.flow.as_ref()?;
    let phase = flow.phase;
    if matches!(phase, TrialPhase::Waiting | TrialPhase::Failed) {
        let code = (phase == TrialPhase::Failed).then(|| {
            flow.progress
                .code
                .clone()
                .unwrap_or_else(|| "Sysom.ProviderFailure".into())
        });
        if phase == TrialPhase::Waiting {
            trial.pending_invalidation = capture_id(trial);
        }
        trial.flow = None;
        trial.cancelled_question = true;
        return code;
    }
    None
}
