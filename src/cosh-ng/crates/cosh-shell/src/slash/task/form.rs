//! Interactive launch form for durable Gateway Tasks.

use std::collections::HashSet;
use std::io::Write;

use serde_json::Value;
use uuid::Uuid;

use crate::raw_input::RawInputCapture;
use crate::runtime::dispatcher::stable_event_key;
use crate::runtime::prelude::{QuestionInputFeedback, ShellEvent, ShellEventKind};
use crate::runtime::question_terminal::clear_active_question_panel;
use crate::runtime::state::InlineState;
use crate::slash::panel::render_notice_panel;

mod navigation;
mod render;
#[cfg(test)]
mod tests;

use navigation::{
    checkpoint_index, form_option_count, parse_id_text, parse_id_value, runtime_index,
};
#[cfg(test)]
use render::{question_text, security_summary};
use render::{render_current_form, safe_goal_preview};

const CONFIRM_OPTION_COUNT: usize = 2;
const CONFIRM_CANCEL_INDEX: usize = 1;
const MAX_RENDERED_GOAL_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskRuntime {
    Core,
    Codex,
}

impl TaskRuntime {
    pub(super) fn gateway_argument(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Codex => "codex",
        }
    }

    fn label(self, language: crate::config::Language) -> &'static str {
        match self {
            Self::Core => "Core (cosh-core)",
            Self::Codex if language == crate::config::Language::ZhCn => "Codex（ACP）",
            Self::Codex => "Codex (ACP)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CapabilityReadiness {
    Ready,
    Unavailable(String),
}

impl CapabilityReadiness {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeCapability {
    runtime: TaskRuntime,
    readiness: CapabilityReadiness,
    security: RuntimeSecurityPosture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeSecurityPosture {
    delegated_local_authority: bool,
    gateway_brokered_effects: bool,
    checkpoint_is_baseline_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskCapabilities {
    workspace: String,
    workspace_scope_digest: String,
    runtimes: Vec<RuntimeCapability>,
    checkpoint: CapabilityReadiness,
}

impl TaskCapabilities {
    #[cfg(test)]
    fn ready() -> Self {
        Self {
            workspace: "current workspace".to_owned(),
            workspace_scope_digest:
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            runtimes: vec![
                RuntimeCapability {
                    runtime: TaskRuntime::Core,
                    readiness: CapabilityReadiness::Ready,
                    security: RuntimeSecurityPosture {
                        delegated_local_authority: true,
                        gateway_brokered_effects: false,
                        checkpoint_is_baseline_only: false,
                    },
                },
                RuntimeCapability {
                    runtime: TaskRuntime::Codex,
                    readiness: CapabilityReadiness::Ready,
                    security: RuntimeSecurityPosture {
                        delegated_local_authority: true,
                        gateway_brokered_effects: false,
                        checkpoint_is_baseline_only: false,
                    },
                },
            ],
            checkpoint: CapabilityReadiness::Ready,
        }
    }

    fn runtime_readiness(&self, runtime: TaskRuntime) -> Option<&CapabilityReadiness> {
        self.runtime_capability(runtime)
            .map(|capability| &capability.readiness)
    }

    fn runtime_capability(&self, runtime: TaskRuntime) -> Option<&RuntimeCapability> {
        self.runtimes
            .iter()
            .find(|capability| capability.runtime == runtime)
    }

    fn ready_runtimes(&self) -> Vec<TaskRuntime> {
        self.runtimes
            .iter()
            .filter(|capability| capability.readiness.is_ready())
            .map(|capability| capability.runtime)
            .collect()
    }

    fn checkpoint_options(&self) -> Vec<TaskCheckpoint> {
        if self.checkpoint.is_ready() {
            vec![
                TaskCheckpoint::Auto,
                TaskCheckpoint::On,
                TaskCheckpoint::Off,
            ]
        } else {
            vec![TaskCheckpoint::Auto, TaskCheckpoint::Off]
        }
    }

    pub(super) fn workspace_scope_digest(&self) -> &str {
        &self.workspace_scope_digest
    }
}

pub(super) fn parse_task_capabilities(value: &Value) -> Result<TaskCapabilities, String> {
    if value.get("event").and_then(Value::as_str) != Some("task_capabilities") {
        return Err("Gateway returned an unexpected capabilities event".to_owned());
    }
    if value.get("launch_schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("Gateway returned an unsupported Task launch schema".to_owned());
    }
    if value.get("default_approval").and_then(Value::as_str) != Some("allow_all") {
        return Err("Gateway does not advertise the required allow_all policy".to_owned());
    }
    let workspace = value
        .get("default_workspace")
        .and_then(Value::as_object)
        .ok_or_else(|| "Gateway capabilities omitted default_workspace".to_owned())?;
    let workspace_scope_digest = workspace
        .get("scope_digest")
        .and_then(Value::as_str)
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| "Gateway capabilities returned an invalid workspace digest".to_owned())?
        .to_owned();
    let workspace = workspace
        .get("display_name")
        .and_then(Value::as_str)
        .or_else(|| workspace.get("scope_digest").and_then(Value::as_str))
        .filter(|workspace| !workspace.is_empty())
        .ok_or_else(|| "Gateway capabilities returned an invalid default_workspace".to_owned())?
        .to_owned();

    let runtime_values = value
        .get("runtimes")
        .and_then(Value::as_array)
        .ok_or_else(|| "Gateway capabilities omitted runtimes".to_owned())?;
    let mut runtimes = Vec::with_capacity(runtime_values.len());
    for runtime_value in runtime_values {
        let runtime = match runtime_value.get("runtime").and_then(Value::as_str) {
            Some("core") => TaskRuntime::Core,
            Some("codex") => TaskRuntime::Codex,
            Some(other) => return Err(format!("Gateway advertised unknown Runtime {other}")),
            None => return Err("Gateway Runtime capability omitted runtime".to_owned()),
        };
        if runtimes
            .iter()
            .any(|capability: &RuntimeCapability| capability.runtime == runtime)
        {
            return Err(format!(
                "Gateway advertised duplicate Runtime {}",
                runtime.gateway_argument()
            ));
        }
        runtimes.push(RuntimeCapability {
            runtime,
            readiness: parse_readiness(runtime_value.get("readiness"))?,
            security: parse_security(runtime_value.get("security"))?,
        });
    }
    for required in [TaskRuntime::Core, TaskRuntime::Codex] {
        if !runtimes
            .iter()
            .any(|capability| capability.runtime == required)
        {
            return Err(format!(
                "Gateway capabilities omitted Runtime {}",
                required.gateway_argument()
            ));
        }
    }

    Ok(TaskCapabilities {
        workspace,
        workspace_scope_digest,
        runtimes,
        checkpoint: parse_readiness(value.get("checkpoint"))?,
    })
}

fn parse_security(value: Option<&Value>) -> Result<RuntimeSecurityPosture, String> {
    let value = value
        .and_then(Value::as_object)
        .ok_or_else(|| "Gateway Runtime capability omitted security posture".to_owned())?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("Gateway Runtime security posture omitted {name}"))
    };
    Ok(RuntimeSecurityPosture {
        delegated_local_authority: field("delegated_local_authority")?,
        gateway_brokered_effects: field("gateway_brokered_effects")?,
        checkpoint_is_baseline_only: field("checkpoint_is_baseline_only")?,
    })
}

fn parse_readiness(value: Option<&Value>) -> Result<CapabilityReadiness, String> {
    let value = value
        .and_then(Value::as_object)
        .ok_or_else(|| "Gateway capability omitted readiness".to_owned())?;
    match value.get("status").and_then(Value::as_str) {
        Some("ready") => Ok(CapabilityReadiness::Ready),
        Some("unavailable") => value
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
            .map(|reason| CapabilityReadiness::Unavailable(reason.to_owned()))
            .ok_or_else(|| "Gateway unavailable capability omitted its reason".to_owned()),
        Some(other) => Err(format!("Gateway returned unknown readiness {other}")),
        None => Err("Gateway capability omitted readiness status".to_owned()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskCheckpoint {
    Auto,
    On,
    Off,
}

impl TaskCheckpoint {
    pub(super) fn gateway_argument(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    fn label(self, language: crate::config::Language) -> &'static str {
        match (self, language) {
            (Self::Auto, crate::config::Language::ZhCn) => "自动",
            (Self::On, crate::config::Language::ZhCn) => "开启",
            (Self::Off, crate::config::Language::ZhCn) => "关闭",
            (Self::Auto, crate::config::Language::EnUs) => "Auto",
            (Self::On, crate::config::Language::EnUs) => "On",
            (Self::Off, crate::config::Language::EnUs) => "Off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskFormPhase {
    Goal,
    Runtime,
    Checkpoint,
    Confirm,
}

#[derive(Debug)]
pub(crate) struct TaskLaunchForm {
    id: String,
    phase: TaskFormPhase,
    goal: String,
    runtime: TaskRuntime,
    checkpoint: TaskCheckpoint,
    selected: usize,
    goal_feedback: QuestionInputFeedback,
    capabilities: TaskCapabilities,
}

#[derive(Debug, Default)]
pub(crate) struct TaskFormState {
    active: Option<TaskLaunchForm>,
    handled_events: HashSet<String>,
}

pub(super) fn open_task_form<W: Write>(
    state: &mut InlineState,
    initial_goal: String,
    output: &mut W,
) -> std::io::Result<bool> {
    if state.task_form.active.is_some() {
        return Ok(true);
    }
    let capabilities = match super::task_capabilities() {
        Ok(capabilities) => capabilities,
        Err(error) => {
            let (title, footer) = match state.language {
                crate::config::Language::EnUs => (
                    "Task unavailable",
                    "Unable to read Gateway launch capabilities.",
                ),
                crate::config::Language::ZhCn => ("Task 暂不可用", "无法读取 Gateway 启动能力。"),
            };
            render_notice_panel(output, title, vec![safe_goal_preview(&error)], Some(footer))?;
            return Ok(false);
        }
    };
    open_task_form_with_capabilities(state, initial_goal, capabilities, output)
}

fn open_task_form_with_capabilities<W: Write>(
    state: &mut InlineState,
    initial_goal: String,
    capabilities: TaskCapabilities,
    output: &mut W,
) -> std::io::Result<bool> {
    let ready_runtimes = capabilities.ready_runtimes();
    if ready_runtimes.is_empty() {
        let (title, summary) = match state.language {
            crate::config::Language::EnUs => {
                ("Task unavailable", "No Task Runtime is currently ready.")
            }
            crate::config::Language::ZhCn => ("Task 暂不可用", "当前没有已就绪的 Task Runtime。"),
        };
        let mut body = vec![summary.to_owned()];
        body.extend(capabilities.runtimes.iter().filter_map(|capability| {
            capability
                .readiness
                .reason()
                .map(|reason| match state.language {
                    crate::config::Language::EnUs => format!(
                        "{}: {}",
                        capability.runtime.label(state.language),
                        safe_goal_preview(reason)
                    ),
                    crate::config::Language::ZhCn => format!(
                        "{}：{}",
                        capability.runtime.label(state.language),
                        safe_goal_preview(reason)
                    ),
                })
        }));
        render_notice_panel(output, title, body, None)?;
        return Ok(false);
    }
    let runtime = if capabilities
        .runtime_readiness(TaskRuntime::Core)
        .is_some_and(CapabilityReadiness::is_ready)
    {
        TaskRuntime::Core
    } else {
        ready_runtimes[0]
    };
    state.task_form.active = Some(TaskLaunchForm {
        id: format!("task-form-{}", Uuid::new_v4()),
        phase: TaskFormPhase::Goal,
        goal: initial_goal,
        runtime,
        checkpoint: TaskCheckpoint::Auto,
        selected: 0,
        goal_feedback: QuestionInputFeedback::None,
        capabilities,
    });
    render_current_form(state, output)?;
    Ok(true)
}

pub(crate) fn pending_task_form_capture(state: &InlineState) -> Option<RawInputCapture> {
    let form = state.task_form.active.as_ref()?;
    let id = capture_id(form);
    match form.phase {
        TaskFormPhase::Goal => Some(RawInputCapture::TextQuestion {
            id,
            initial_text: form.goal.clone(),
            secret: false,
        }),
        TaskFormPhase::Runtime => Some(RawInputCapture::Question {
            id,
            option_count: form.capabilities.ready_runtimes().len(),
            selected: form.selected,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
        TaskFormPhase::Checkpoint => Some(RawInputCapture::Question {
            id,
            option_count: form.capabilities.checkpoint_options().len(),
            selected: form.selected,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
        TaskFormPhase::Confirm => Some(RawInputCapture::Question {
            id,
            option_count: CONFIRM_OPTION_COUNT,
            selected: form.selected,
            allow_free_text: false,
            multiple: false,
            secret: false,
        }),
    }
}

pub(crate) fn render_task_form_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if state.task_form.active.is_none() {
        return Ok(());
    }
    for (index, event) in events.iter().enumerate() {
        if event.kind != ShellEventKind::UserInputIntercepted
            || event.component.as_deref() != Some("card")
        {
            continue;
        }
        let event_index = event_index_base + index;
        let key = stable_event_key("task-form", event_index, event);
        if !state.task_form.handled_events.insert(key) {
            continue;
        }

        match event.message.as_deref() {
            Some("focus") => {
                let Some((id, selected)) = parse_id_value(event) else {
                    continue;
                };
                if !event_targets_form(state, &id) {
                    continue;
                }
                if let Some(form) = state.task_form.active.as_mut() {
                    form.selected = selected.min(form_option_count(form).saturating_sub(1));
                }
                redraw_form(state, output)?;
            }
            Some("input") => {
                let Some((id, text)) = parse_id_text(event) else {
                    continue;
                };
                if !event_targets_form(state, &id) {
                    continue;
                }
                if let Some(form) = state.task_form.active.as_mut() {
                    form.goal = text;
                    form.goal_feedback = QuestionInputFeedback::None;
                }
                redraw_form(state, output)?;
            }
            Some("answer") => {
                reserve_question_answer_event(state, event_index, event);
                let answer = event.input.as_deref().unwrap_or_default();
                advance_form(state, answer, output)?;
            }
            Some("question_submit_empty") => {
                let Some(id) = event.input.as_deref().map(str::trim) else {
                    continue;
                };
                if !event_targets_form(state, id) {
                    continue;
                }
                reserve_question_answer_event(state, event_index, event);
                advance_form(state, "", output)?;
            }
            Some("question_cancel") => {
                let Some(id) = event.input.as_deref().map(str::trim) else {
                    continue;
                };
                if event_targets_form(state, id) {
                    step_back_or_cancel(state, output)?;
                }
            }
            Some("question_abort") => {
                let Some(id) = event.input.as_deref().map(str::trim) else {
                    continue;
                };
                if event_targets_form(state, id) {
                    cancel_form(state, output)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn reserve_question_answer_event(state: &mut InlineState, index: usize, event: &ShellEvent) {
    // Generic Question/TextQuestion answers do not carry their capture id.
    // Claim the event before QuestionConsumer runs so it cannot report a
    // spurious "No pending question" for a Task-owned form answer.
    state
        .questions
        .handled_answers
        .insert(stable_event_key("question-answer", index, event));
}

fn advance_form<W: Write>(
    state: &mut InlineState,
    answer: &str,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(phase) = state.task_form.active.as_ref().map(|form| form.phase) else {
        return Ok(());
    };
    match phase {
        TaskFormPhase::Goal => {
            let goal = answer.trim();
            if goal.is_empty() || goal.len() > super::MAX_TASK_GOAL_BYTES {
                if let Some(form) = state.task_form.active.as_mut() {
                    form.goal_feedback = if goal.is_empty() {
                        QuestionInputFeedback::Required
                    } else {
                        QuestionInputFeedback::Invalid
                    };
                }
                return redraw_form(state, output);
            }
            if let Some(form) = state.task_form.active.as_mut() {
                form.goal = goal.to_owned();
                form.phase = TaskFormPhase::Runtime;
                form.selected = runtime_index(&form.capabilities, form.runtime);
            }
        }
        TaskFormPhase::Runtime => {
            if let Some(form) = state.task_form.active.as_mut() {
                let runtimes = form.capabilities.ready_runtimes();
                let Some(runtime) = runtimes.get(form.selected).copied() else {
                    return redraw_form(state, output);
                };
                form.runtime = runtime;
                form.phase = TaskFormPhase::Checkpoint;
                form.selected = checkpoint_index(&form.capabilities, form.checkpoint);
            }
        }
        TaskFormPhase::Checkpoint => {
            if let Some(form) = state.task_form.active.as_mut() {
                let checkpoints = form.capabilities.checkpoint_options();
                let Some(checkpoint) = checkpoints.get(form.selected).copied() else {
                    return redraw_form(state, output);
                };
                form.checkpoint = checkpoint;
                form.phase = TaskFormPhase::Confirm;
                form.selected = 0;
            }
        }
        TaskFormPhase::Confirm => {
            if state
                .task_form
                .active
                .as_ref()
                .is_some_and(|form| form.selected == CONFIRM_CANCEL_INDEX)
            {
                return cancel_form(state, output);
            }
            let Some(form) = state.task_form.active.take() else {
                return Ok(());
            };
            clear_active_question_panel(state, output)?;
            super::render_submission_progress(state, output)?;
            output.flush()?;
            let result = super::submit_task(
                &form.goal,
                form.runtime,
                form.checkpoint,
                form.capabilities.workspace_scope_digest(),
            );
            super::render_submission_result(result, state, output)?;
            state.trigger_pty_prompt = true;
            return Ok(());
        }
    }
    redraw_form(state, output)
}

fn step_back_or_cancel<W: Write>(state: &mut InlineState, output: &mut W) -> std::io::Result<()> {
    let Some(phase) = state.task_form.active.as_ref().map(|form| form.phase) else {
        return Ok(());
    };
    if phase == TaskFormPhase::Goal {
        return cancel_form(state, output);
    }
    if let Some(form) = state.task_form.active.as_mut() {
        match phase {
            TaskFormPhase::Runtime => {
                form.phase = TaskFormPhase::Goal;
                form.selected = 0;
            }
            TaskFormPhase::Checkpoint => {
                form.phase = TaskFormPhase::Runtime;
                form.selected = runtime_index(&form.capabilities, form.runtime);
            }
            TaskFormPhase::Confirm => {
                form.phase = TaskFormPhase::Checkpoint;
                form.selected = checkpoint_index(&form.capabilities, form.checkpoint);
            }
            TaskFormPhase::Goal => {}
        }
    }
    redraw_form(state, output)
}

fn cancel_form<W: Write>(state: &mut InlineState, output: &mut W) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    state.task_form.active = None;
    let (title, body) = match state.language {
        crate::config::Language::EnUs => (
            "Task creation cancelled",
            "No persistent Task was submitted.",
        ),
        crate::config::Language::ZhCn => ("已取消创建 Task", "没有提交持久 Task。"),
    };
    render_notice_panel(output, title, vec![body.to_owned()], None)?;
    state.trigger_pty_prompt = true;
    Ok(())
}

fn redraw_form<W: Write>(state: &mut InlineState, output: &mut W) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    render_current_form(state, output)
}

fn capture_id(form: &TaskLaunchForm) -> String {
    let phase = match form.phase {
        TaskFormPhase::Goal => "goal",
        TaskFormPhase::Runtime => "runtime",
        TaskFormPhase::Checkpoint => "checkpoint",
        TaskFormPhase::Confirm => "confirm",
    };
    format!("{}-{phase}", form.id)
}

fn event_targets_form(state: &InlineState, id: &str) -> bool {
    state
        .task_form
        .active
        .as_ref()
        .is_some_and(|form| capture_id(form) == id)
}
