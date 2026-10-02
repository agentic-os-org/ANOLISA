//! Rendering and terminal-safe display projections for the Task launch form.

use std::io::Write;

use crate::config::Language;
use crate::runtime::prelude::{
    QuestionInputFeedback, QuestionPanelModel, QuestionPanelPresentation, QuestionSelectionMode,
    RatatuiInlineRenderer,
};
use crate::runtime::state::InlineState;
use crate::ui::OPTION_DETAIL_SEPARATOR;

use super::{
    capture_id, RuntimeCapability, RuntimeSecurityPosture, TaskCheckpoint, TaskFormPhase,
    TaskLaunchForm, MAX_RENDERED_GOAL_BYTES,
};

pub(super) fn render_current_form<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(form) = state.task_form.active.as_ref() else {
        return Ok(());
    };
    let id = capture_id(form);
    let question = question_text(form, state.language);
    let rendered_goal = safe_goal_preview(&form.goal);
    let runtime_options = form
        .capabilities
        .ready_runtimes()
        .into_iter()
        .enumerate()
        .filter_map(|(index, runtime)| {
            form.capabilities
                .runtime_capability(runtime)
                .map(|capability| {
                    runtime_option(capability, index == form.selected, state.language)
                })
        })
        .collect::<Vec<_>>();
    let checkpoint_options = form
        .capabilities
        .checkpoint_options()
        .into_iter()
        .map(|checkpoint| checkpoint_option(checkpoint, state.language))
        .collect::<Vec<_>>();
    let confirm_options = confirm_options(state.language);
    let (options, allow_free_text, custom_answer, feedback): (
        &[String],
        bool,
        &str,
        QuestionInputFeedback,
    ) = match form.phase {
        TaskFormPhase::Goal => (&[], true, &rendered_goal, form.goal_feedback),
        TaskFormPhase::Runtime => (&runtime_options, false, "", QuestionInputFeedback::Disabled),
        TaskFormPhase::Checkpoint => (
            &checkpoint_options,
            false,
            "",
            QuestionInputFeedback::Disabled,
        ),
        TaskFormPhase::Confirm => (&confirm_options, false, "", QuestionInputFeedback::Disabled),
    };
    let model = QuestionPanelModel {
        id: &id,
        question: &question,
        options,
        selected_option: form.selected,
        selected_options: &[],
        custom_answer,
        allow_free_text,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: feedback,
    };
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let cursor_row = renderer
        .active_question_cursor_placement(&model)
        .map(|placement| placement.row);
    let width = renderer.panel_standard_width();
    let height = renderer.write_question_panel_with_presentation(
        output,
        model,
        task_presentation(form.phase, state.language),
    )?;
    state.questions.active_panel_id = Some(id);
    state.questions.active_panel_height = height;
    state.questions.active_panel_cursor_row = cursor_row;
    state.questions.active_panel_width = Some(width);
    Ok(())
}

pub(super) fn question_text(form: &TaskLaunchForm, language: Language) -> String {
    let english = language == Language::EnUs;
    let mut lines = vec![phase_heading(form.phase, english).to_owned()];
    match form.phase {
        TaskFormPhase::Goal => lines.push(if english {
            "Describe the outcome this durable Task should achieve.".to_owned()
        } else {
            "描述这个持久 Task 需要达成的结果。".to_owned()
        }),
        TaskFormPhase::Runtime => {
            lines.push(if english {
                "Choose a ready Runtime. Core is the default when it is ready.".to_owned()
            } else {
                "选择已就绪的 Runtime。Core 就绪时为默认选项。".to_owned()
            });
            for capability in &form.capabilities.runtimes {
                if let Some(reason) = capability.readiness.reason() {
                    lines.push(if english {
                        format!(
                            "Unavailable · {}: {}",
                            capability.runtime.label(language),
                            safe_goal_preview(reason)
                        )
                    } else {
                        format!(
                            "不可用 · {}：{}",
                            capability.runtime.label(language),
                            safe_goal_preview(reason)
                        )
                    });
                }
            }
        }
        TaskFormPhase::Checkpoint => {
            lines.push(if english {
                "Choose the pre-Runtime workspace baseline policy.".to_owned()
            } else {
                "选择 Runtime 启动前的工作区基线策略。".to_owned()
            });
            lines.push(baseline_scope_notice(english).to_owned());
            if let Some(reason) = form.capabilities.checkpoint.reason() {
                lines.push(if english {
                    format!(
                        "Checkpoint provider unavailable: {}. Auto records an explicit downgrade; On is hidden.",
                        safe_goal_preview(reason)
                    )
                } else {
                    format!(
                        "Checkpoint Provider 不可用：{}。自动模式会明确记录降级；开启选项已隐藏。",
                        safe_goal_preview(reason)
                    )
                });
            }
        }
        TaskFormPhase::Confirm => lines.extend(review_lines(form, language)),
    }
    lines.join("\n")
}

fn phase_heading(phase: TaskFormPhase, english: bool) -> &'static str {
    match (phase, english) {
        (TaskFormPhase::Goal, true) => "Create persistent Task · Step 1 of 4 · Goal",
        (TaskFormPhase::Runtime, true) => "Create persistent Task · Step 2 of 4 · Runtime",
        (TaskFormPhase::Checkpoint, true) => "Create persistent Task · Step 3 of 4 · Checkpoint",
        (TaskFormPhase::Confirm, true) => "Create persistent Task · Step 4 of 4 · Review",
        (TaskFormPhase::Goal, false) => "创建持久 Task · 第 1/4 步 · 目标",
        (TaskFormPhase::Runtime, false) => "创建持久 Task · 第 2/4 步 · Runtime",
        (TaskFormPhase::Checkpoint, false) => "创建持久 Task · 第 3/4 步 · Checkpoint",
        (TaskFormPhase::Confirm, false) => "创建持久 Task · 第 4/4 步 · 检查",
    }
}

pub(super) fn task_presentation(
    phase: TaskFormPhase,
    language: Language,
) -> QuestionPanelPresentation<'static> {
    let english = language == Language::EnUs;
    let title = if english {
        "Persistent Task"
    } else {
        "持久 Task"
    };
    let prefix = if english {
        "Task keys · "
    } else {
        "Task 快捷键 · "
    };
    let instruction = match (phase, english) {
        (TaskFormPhase::Goal, true) => "Enter continue · Esc cancel · Ctrl+C cancel",
        (TaskFormPhase::Confirm, true) => {
            "Enter activate selected action · Esc back · Ctrl+C cancel"
        }
        (_, true) => "Enter continue · Esc back · Ctrl+C cancel",
        (TaskFormPhase::Goal, false) => "Enter 继续 · Esc 取消 · Ctrl+C 取消",
        (TaskFormPhase::Confirm, false) => "Enter 执行所选操作 · Esc 返回 · Ctrl+C 取消",
        (_, false) => "Enter 继续 · Esc 返回 · Ctrl+C 取消",
    };
    QuestionPanelPresentation::new(title, prefix, instruction)
}

fn runtime_option(capability: &RuntimeCapability, selected: bool, language: Language) -> String {
    let english = language == Language::EnUs;
    let readiness = match (english, selected) {
        (true, true) => "Ready · Selected",
        (true, false) => "Ready",
        (false, true) => "已就绪 · 已选择",
        (false, false) => "已就绪",
    };
    format!(
        "{}{}{} · {}",
        capability.runtime.label(language),
        OPTION_DETAIL_SEPARATOR,
        readiness,
        runtime_authority_summary(capability.security, english)
    )
}

fn checkpoint_option(checkpoint: TaskCheckpoint, language: Language) -> String {
    let english = language == Language::EnUs;
    let detail = match (checkpoint, english) {
        (TaskCheckpoint::Auto, true) => {
            "Default · Create a launch baseline and a barrier before each approved Runtime effect when the provider is available; otherwise record an explicit downgrade."
        }
        (TaskCheckpoint::On, true) => {
            "Require a proven launch baseline and pre-approval barriers; the Runtime does not start and effects are not released without checkpoint evidence."
        }
        (TaskCheckpoint::Off, true) => {
            "Start the Runtime without a baseline or pre-approval checkpoint barriers."
        }
        (TaskCheckpoint::Auto, false) => {
            "默认 · Provider 可用时创建启动基线，并在每个获批 Runtime 操作前创建屏障快照；不可用时明确记录降级。"
        }
        (TaskCheckpoint::On, false) => {
            "必须取得经验证的启动基线和审批前屏障；没有快照证据时不启动 Runtime，也不放行操作。"
        }
        (TaskCheckpoint::Off, false) => "不创建启动基线或审批前屏障，直接启动 Runtime。",
    };
    format!(
        "{}{}{}",
        checkpoint.label(language),
        OPTION_DETAIL_SEPARATOR,
        detail
    )
}

pub(super) fn confirm_options(language: Language) -> Vec<String> {
    match language {
        Language::EnUs => vec![
            format!(
                "Submit persistent Task{OPTION_DETAIL_SEPARATOR}Record it durably with the selected launch policy."
            ),
            format!("Cancel{OPTION_DETAIL_SEPARATOR}Leave without submitting a Task."),
        ],
        Language::ZhCn => vec![
            format!(
                "提交持久 Task{OPTION_DETAIL_SEPARATOR}按所选启动策略持久记录。"
            ),
            format!("取消{OPTION_DETAIL_SEPARATOR}退出且不提交 Task。"),
        ],
    }
}

fn review_lines(form: &TaskLaunchForm, language: Language) -> Vec<String> {
    let english = language == Language::EnUs;
    let security = form
        .capabilities
        .runtime_capability(form.runtime)
        .map(|capability| security_summary(capability.security, english))
        .unwrap_or_else(|| {
            if english {
                "Runtime authority unavailable".to_owned()
            } else {
                "Runtime 权限信息不可用".to_owned()
            }
        });
    if english {
        vec![
            "Goal".to_owned(),
            format!("  {}", safe_goal_preview(&form.goal)),
            "Launch".to_owned(),
            format!("  Runtime: {}", form.runtime.label(language)),
            format!(
                "  Workspace: {}",
                safe_goal_preview(&form.capabilities.workspace)
            ),
            format!(
                "  Checkpoint: {}{}",
                form.checkpoint.label(language),
                checkpoint_downgrade_notice(form, english)
            ),
            format!("  {}", baseline_scope_notice(english)),
            "Authority".to_owned(),
            "  Approval: allow_all · supported operations are resolved automatically and durably audited"
                .to_owned(),
            format!("  {security}"),
        ]
    } else {
        vec![
            "目标".to_owned(),
            format!("  {}", safe_goal_preview(&form.goal)),
            "启动".to_owned(),
            format!("  Runtime：{}", form.runtime.label(language)),
            format!(
                "  工作区：{}",
                safe_goal_preview(&form.capabilities.workspace)
            ),
            format!(
                "  Checkpoint：{}{}",
                form.checkpoint.label(language),
                checkpoint_downgrade_notice(form, english)
            ),
            format!("  {}", baseline_scope_notice(english)),
            "权限".to_owned(),
            "  审批：allow_all · 自动处理支持的操作，并持久记录审计事件".to_owned(),
            format!("  {security}"),
        ]
    }
}

fn baseline_scope_notice(english: bool) -> &'static str {
    if english {
        "Checkpoint scope · the launch baseline and approval barriers cover workspace files only; they do not protect network, credentials, cloud resources, or other external effects."
    } else {
        "Checkpoint 范围 · 启动基线和审批前屏障仅覆盖工作区文件；不保护网络、凭据、云资源或其他外部效应。"
    }
}

pub(super) fn security_summary(posture: RuntimeSecurityPosture, english: bool) -> String {
    let authority = match (english, posture.delegated_local_authority) {
        (true, true) => "delegated local service-user authority",
        (true, false) => "no delegated local service-user authority",
        (false, true) => "委托本地服务用户权限",
        (false, false) => "不委托本地服务用户权限",
    };
    let effects = match (english, posture.gateway_brokered_effects) {
        (true, true) => "COSH-owned effects are Gateway-executed",
        (true, false) => {
            "Runtime-native effects use the service user's authority after Gateway approval"
        }
        (false, true) => "COSH 自有操作由 Gateway 执行",
        (false, false) => "Runtime 原生操作经 Gateway 审批后使用服务用户权限执行",
    };
    let checkpoint = match (english, posture.checkpoint_is_baseline_only) {
        (true, true) => "checkpoint is only a pre-Runtime workspace baseline",
        (true, false) => "checkpoint adds a barrier before each approved Runtime effect",
        (false, true) => "Checkpoint 仅是 Runtime 启动前的工作区基线",
        (false, false) => "Checkpoint 会在每个获批 Runtime 操作前增加屏障快照",
    };
    format!("{authority}; {effects}; {checkpoint}")
}

fn runtime_authority_summary(posture: RuntimeSecurityPosture, english: bool) -> String {
    let authority = match (english, posture.delegated_local_authority) {
        (true, true) => "delegated local service-user authority",
        (true, false) => "no delegated local service-user authority",
        (false, true) => "委托本地服务用户权限",
        (false, false) => "不委托本地服务用户权限",
    };
    let effects = match (english, posture.gateway_brokered_effects) {
        (true, true) => "COSH-owned effects are Gateway-executed",
        (true, false) => "Runtime-native effects require Gateway approval",
        (false, true) => "COSH 自有操作由 Gateway 执行",
        (false, false) => "Runtime 原生操作需要 Gateway 审批",
    };
    format!("{authority}; {effects}")
}

fn checkpoint_downgrade_notice(form: &TaskLaunchForm, english: bool) -> String {
    if form.checkpoint != TaskCheckpoint::Auto {
        return String::new();
    }
    let Some(reason) = form.capabilities.checkpoint.reason() else {
        return String::new();
    };
    if english {
        format!(
            " · Gateway records an explicit downgrade: {}",
            safe_goal_preview(reason)
        )
    } else {
        format!(" · Gateway 会明确记录降级：{}", safe_goal_preview(reason))
    }
}

pub(super) fn safe_goal_preview(goal: &str) -> String {
    let sanitized = super::super::safe_text(goal);
    if sanitized.len() <= MAX_RENDERED_GOAL_BYTES {
        return sanitized;
    }
    let mut boundary = MAX_RENDERED_GOAL_BYTES;
    while !sanitized.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    let mut preview = sanitized[..boundary].to_owned();
    preview.push('…');
    preview
}
