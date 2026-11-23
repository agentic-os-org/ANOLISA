mod prepare;
pub(crate) use prepare::{run_worker, WORKER_FLAG};

use crate::agent::run::ActiveAgentRun;
use crate::raw_input::PromptSnapshot;
use crate::runtime::prelude::*;

#[derive(Default)]
pub(crate) struct InvitationState {
    pending: bool,
    attempted: bool,
    shown: bool,
    prompt: Option<PromptSnapshot>,
    preparation: Option<prepare::Preparation>,
    preparation_context: Option<(Language, String)>,
}

impl InvitationState {
    pub(crate) fn observe_prompt(&mut self, prompt: Option<&PromptSnapshot>) {
        if !self.attempted && self.preparation.is_none() {
            if let Some(prompt) = prompt {
                self.prompt = Some(prompt.clone());
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }
}

pub(crate) fn note_completed_run(state: &mut InlineState, run: &ActiveAgentRun) {
    if state.startup_session.login_invitation.attempted
        || !run.markdown_stream.has_rendered_body()
        || state.agent_run.active.is_some()
        || !state.agent_run.queued_requests.is_empty()
        || !state.agent_run.held_events.is_empty()
        || state.agent_run.pending_turn_extension.is_some()
        || crate::agent::events::state_has_pending_interaction(state)
    {
        return;
    }
    let terminal = run.governed_events.iter().rev().find(|event| {
        matches!(
            event.event,
            AgentEvent::AgentCompleted { .. }
                | AgentEvent::AgentFailed { .. }
                | AgentEvent::AgentCancelled { .. }
        )
    });
    if terminal.is_some_and(|event| matches!(event.event, AgentEvent::AgentCompleted { .. })) {
        state.startup_session.login_invitation.pending = true;
    }
}

use crate::config::guide::{ClaimDecision, ClaimRequest, GuideStore, InviteKind, RuntimeState};

struct Prepared {
    store: GuideStore,
    runtime: RuntimeState,
    generation: String,
    text: String,
}

fn claim_offer(
    store: GuideStore,
    runtime: RuntimeState,
    language: Language,
    host: &str,
    session: &str,
    now_ms: u64,
) -> Option<Prepared> {
    store.initialize_runtime(&runtime).ok()?;
    let snapshot = store.snapshot(Some(&runtime)).ok()?;
    let invitation = crate::ui::guidance::default_login(language);
    let ClaimDecision::Claimed(claim) = store
        .claim(
            Some(&runtime),
            &snapshot,
            ClaimRequest {
                kind: InviteKind::DefaultLogin,
                host,
                problem: &invitation.problem,
                session,
            },
            now_ms,
        )
        .ok()?
    else {
        return None;
    };
    Some(Prepared {
        store,
        runtime,
        generation: claim.generation,
        text: invitation.text,
    })
}

fn eligible_account(status: &super::LoginStatus) -> bool {
    !status.conflict && !status.managed && super::account::supported_bash(&status.current_shell)
}

fn prepare_offer(language: Language, session: &str) -> Option<Prepared> {
    let status = super::query_status(None).ok()?;
    if !eligible_account(&status) {
        return None;
    }
    super::account::verify_bash(&status.current_shell).ok()?;
    super::capability::verify().ok()?;
    let host = super::account::read_trusted(std::path::Path::new("/etc/machine-id"), 128).ok()?;
    let host = std::str::from_utf8(&host).ok()?.trim();
    if host.is_empty() || !host.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let (store, runtime) = crate::config::guide::open_user_state().ok()?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    claim_offer(store, runtime, language, host, session, now_ms)
}

pub(crate) fn observe_user_activity(state: &mut InlineState, events: &[ShellEvent]) {
    if state.startup_session.login_invitation.pending
        && events.iter().any(|event| {
            event.kind == ShellEventKind::CommandStarted
                || (event.kind == ShellEventKind::UserInputIntercepted
                    && !(event.component.as_deref() == Some("shell_prompt_ready")
                        || event.component.as_deref() == Some("shell_input")
                            && event.message.as_deref() == Some("input empty")))
        })
    {
        cancel(state);
    }
}

fn cancel(state: &mut InlineState) {
    let invitation = &mut state.startup_session.login_invitation;
    invitation.pending = false;
    invitation.attempted = true;
    invitation.preparation.take();
    invitation.preparation_context.take();
    invitation.prompt.take();
}

fn idle_allowed(state: &InlineState) -> bool {
    !(state.shell_exited
        || state.control.shell_busy()
        || state.personalization.ai_disabled
        || state.personalization.shell_input_active
        || state.agent_run.active.is_some()
        || !state.agent_run.queued_requests.is_empty()
        || !state.agent_run.held_events.is_empty()
        || state.agent_run.pending_turn_extension.is_some()
        || crate::auth::runtime::has_auth_operation(state)
        || crate::runtime::controller::pending_card_capture(state).is_some()
        || state.control.shell_handoff().has_active_handoff()
        || state.control.shell_handoff().pending_front().is_some()
        || crate::slash::session::compaction_pending_or_active(state))
}

pub(crate) fn render_at_idle<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !state.startup_session.login_invitation.pending
        || state.startup_session.login_invitation.attempted
    {
        return Ok(());
    }
    if !idle_allowed(state) {
        cancel(state);
        return Ok(());
    }
    if !state.shell_at_prompt {
        if state.startup_session.login_invitation.preparation.is_some() {
            cancel(state);
        }
        return Ok(());
    }
    let invitation = &mut state.startup_session.login_invitation;
    if invitation.preparation.is_none() {
        let Some(prompt) = invitation.prompt.as_ref() else {
            return Ok(());
        };
        if prompt.with_ready(|| ()).is_none() {
            return Ok(());
        }
        let Some(session) = state.shell_session_id.as_deref() else {
            cancel(state);
            return Ok(());
        };
        match prepare::Preparation::start(state.language, session) {
            Ok(preparation) => {
                invitation.preparation = Some(preparation);
                invitation.preparation_context = Some((state.language, session.into()));
            }
            Err(_) => cancel(state),
        }
        return Ok(());
    }
    if invitation
        .preparation_context
        .as_ref()
        .is_some_and(|(language, session)| {
            *language != state.language
                || Some(session.as_str()) != state.shell_session_id.as_deref()
        })
    {
        cancel(state);
        return Ok(());
    }
    let Some(result) = invitation
        .preparation
        .as_mut()
        .and_then(prepare::Preparation::poll)
    else {
        return Ok(());
    };
    let prompt = invitation.prompt.take();
    cancel(state);
    let (Some(receipt), Some(prompt)) = (result, prompt) else {
        return Ok(());
    };
    let Ok((store, runtime)) = crate::config::guide::open_user_state() else {
        return Ok(());
    };
    let prepared = Prepared {
        store,
        runtime,
        generation: receipt.generation,
        text: crate::ui::guidance::default_login(state.language).text,
    };
    commit_prepared(state, output, prompt, prepared)
}

#[cfg(test)]
fn render_with<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    prepare: impl FnOnce(Language, &str) -> Option<Prepared>,
) -> std::io::Result<()> {
    if !state.startup_session.login_invitation.pending
        || state.startup_session.login_invitation.attempted
    {
        return Ok(());
    }
    if !idle_allowed(state) {
        cancel(state);
        return Ok(());
    }
    if !state.shell_at_prompt {
        return Ok(());
    }
    let prompt = state.startup_session.login_invitation.prompt.take();
    cancel(state);
    let Some(prompt) = prompt else {
        return Ok(());
    };
    let Some(session) = state.shell_session_id.as_deref() else {
        return Ok(());
    };
    let Some(prepared) = prepare(state.language, session) else {
        return Ok(());
    };
    commit_prepared(state, output, prompt, prepared)
}

fn commit_prepared<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    prompt: PromptSnapshot,
    prepared: Prepared,
) -> std::io::Result<()> {
    if !idle_allowed(state) || !state.shell_at_prompt {
        return Ok(());
    }
    let Ok(snapshot) = prepared.store.snapshot(Some(&prepared.runtime)) else {
        return Ok(());
    };
    if !snapshot.preferences.guide_enabled
        || snapshot.generation != prepared.generation
        || !matches!(prepared.runtime.read(), Ok(Some(runtime)) if runtime.enabled)
    {
        return Ok(());
    }
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let title = if state.language == Language::ZhCn {
        "默认登录 Shell"
    } else {
        "Default login Shell"
    };
    let result = prompt.with_ready(|| -> std::io::Result<()> {
        write!(output, "\r\x1b[2K\r\n")?;
        renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title,
                body: prepared.text.lines().map(str::to_owned).collect(),
                footer: None,
            },
        )?;
        output.flush()
    });
    if let Some(result) = result {
        result?;
        state.startup_session.login_invitation.shown = true;
        crate::slash::prompt::write_shell_prompt(state, output)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "invitation/tests.rs"]
mod tests;
