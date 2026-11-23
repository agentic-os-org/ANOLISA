use std::io::{self, Write};

use crate::diagnostics::health::{
    sorted_try_items, HealthMessageId, HealthScanReport, HealthTryItem, HealthTryKind,
};
use crate::ui::{display_width, wrap_plain_line, RatatuiInlineRenderer};
use crate::Language;

const LIST_INDENT: &str = "  ";
const ACTIVE_MARKER: &str = "› ";
const IDLE_MARKER: &str = "  ";

pub(crate) fn session_health_questions(
    report: &HealthScanReport,
    language: Language,
) -> Vec<SuggestionItem> {
    let i18n = crate::I18n::new(language);
    crate::diagnostics::health::select_try_items(
        report,
        &report.try_items,
        crate::diagnostics::health::AdviceSurface::Cosh,
        3,
    )
    .into_iter()
    .filter_map(|item| {
        session_question(item, &i18n).map(|question| SuggestionItem {
            id: item.id.clone(),
            question,
        })
    })
    .collect()
}

fn session_question(item: &HealthTryItem, i18n: &crate::I18n) -> Option<String> {
    if item.kind != HealthTryKind::AskAgent {
        return None;
    }
    let prompt_id = item.prompt_id?;
    let session_id = match prompt_id {
        HealthMessageId::HealthPromptAnalyzeMemoryPressure => {
            crate::MessageId::HealthSessionPromptAnalyzeMemoryPressure
        }
        HealthMessageId::HealthPromptCheckSwapPressure => {
            crate::MessageId::HealthSessionPromptCheckSwapPressure
        }
        HealthMessageId::HealthPromptCheckRecentOom => {
            crate::MessageId::HealthSessionPromptCheckRecentOom
        }
        HealthMessageId::HealthPromptInspectDiskUsage => {
            crate::MessageId::HealthSessionPromptInspectDiskUsage
        }
        HealthMessageId::HealthPromptInspectServiceStatus => {
            crate::MessageId::HealthSessionPromptInspectServiceStatus
        }
        HealthMessageId::HealthPromptInspectHighLoad => {
            crate::MessageId::HealthSessionPromptInspectHighLoad
        }
        HealthMessageId::HealthPromptInspectProcessMemory => {
            crate::MessageId::HealthSessionPromptInspectProcessMemory
        }
        other => other.to_i18n(),
    };
    let args: Vec<(&str, &str)> = item
        .prompt_args
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut question = i18n.format(session_id, &args);
    if session_id == crate::MessageId::HealthSessionPromptInspectDiskUsage {
        if let Some(mount) = item
            .prompt_args
            .get("mount")
            .filter(|mount| !mount.is_empty())
        {
            question = format!("{mount}: {question}");
        }
    }
    if question.is_empty() || question.len() > 2048 || question.chars().any(char::is_control) {
        return None;
    }
    Some(question)
}

pub(crate) fn suggestion_surface_payload(
    items: &[SuggestionItem],
    width: u16,
    height: u16,
) -> crate::raw_input::SuggestionSurfacePayload {
    let base = RatatuiInlineRenderer::for_terminal();
    let mut renderer = RatatuiInlineRenderer::with_width(width);
    renderer.styled = base.styled;
    renderer.plain = base.plain;
    let renderer = renderer.with_language(
        crate::config::read_guidance_language()
            .unwrap_or_else(|_| crate::config::detect_language_from_env()),
    );
    let variants = (0..items.len().max(1))
        .map(|active| {
            renderer.suggestion_list_lines(SuggestionListModel {
                items,
                active,
                interactive: true,
            })
        })
        .collect();
    let labels = items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, item.question))
        .collect();
    let ids = items.iter().map(|item| item.id.clone()).collect();
    let rows = renderer.suggestion_list_row_count(
        SuggestionListModel {
            items,
            active: 0,
            interactive: true,
        },
        1,
    );
    crate::raw_input::SuggestionSurfacePayload {
        variants,
        labels,
        ids,
        rows,
        width,
        height,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SuggestionItem {
    pub(crate) id: String,
    pub(crate) question: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SuggestionListModel<'a> {
    pub(crate) items: &'a [SuggestionItem],
    pub(crate) active: usize,
    pub(crate) interactive: bool,
}

impl RatatuiInlineRenderer {
    pub(crate) fn write_suggestion_list<W: Write>(
        &self,
        output: &mut W,
        model: SuggestionListModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.suggestion_list_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub(crate) fn suggestion_list_lines(&self, model: SuggestionListModel<'_>) -> Vec<String> {
        if model.items.is_empty() {
            return Vec::new();
        }
        let width = self.panel_standard_width() as usize;
        let mut lines = vec![self.suggestion_header()];
        let multiple = model.items.len() > 1;
        for (index, item) in model.items.iter().enumerate() {
            let prefix = if multiple {
                let marker = if model.interactive && index == model.active {
                    ACTIVE_MARKER
                } else {
                    IDLE_MARKER
                };
                format!("{LIST_INDENT}{marker}{}. ", index + 1)
            } else {
                LIST_INDENT.to_string()
            };
            let active = !multiple || index == model.active;
            lines.extend(self.suggestion_question_lines(&item.question, &prefix, active, width));
        }
        if model.interactive {
            lines.extend(self.suggestion_footer_lines(multiple, width));
        }
        lines
    }

    pub(crate) fn suggestion_list_row_count(
        &self,
        model: SuggestionListModel<'_>,
        prompt_rows: usize,
    ) -> usize {
        if model.items.is_empty() {
            return 0;
        }
        self.suggestion_list_lines(model).len() + 1 + prompt_rows
    }

    fn suggestion_header(&self) -> String {
        let label = self.i18n().t(crate::MessageId::SuggestionListTitle);
        if self.styled {
            format!("\x1b[36;1m┃ Cosh AI\x1b[0m  {label}")
        } else {
            format!("┃ Cosh AI  {label}")
        }
    }

    fn suggestion_question_lines(
        &self,
        question: &str,
        prefix: &str,
        active: bool,
        width: usize,
    ) -> Vec<String> {
        let prefix_width = display_width(prefix);
        let content_width = width.saturating_sub(prefix_width).max(1);
        let continuation = " ".repeat(prefix_width);
        wrap_plain_line(question, content_width)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let lead = if index == 0 { prefix } else { &continuation };
                let body = if self.styled && !active {
                    format!("\x1b[90m{line}\x1b[0m")
                } else {
                    line
                };
                format!("{lead}{body}")
            })
            .collect()
    }

    fn suggestion_footer_lines(&self, multiple: bool, width: usize) -> Vec<String> {
        let i18n = self.i18n();
        let mut groups = vec![
            i18n.t(crate::MessageId::SuggestionShortcutInsert),
            i18n.t(crate::MessageId::SuggestionShortcutAsk),
        ];
        if multiple {
            groups.insert(0, i18n.t(crate::MessageId::SuggestionShortcutCycle));
        }
        let mut lines = Vec::new();
        let mut current = String::from(LIST_INDENT);
        for group in groups {
            let candidate = if display_width(&current) > display_width(LIST_INDENT) {
                format!("{current} · {group}")
            } else {
                format!("{current}{group}")
            };
            if display_width(&candidate) > width
                && display_width(&current) > display_width(LIST_INDENT)
            {
                lines.push(current);
                current = format!("{LIST_INDENT}{group}");
            } else {
                current = candidate;
            }
        }
        lines.push(current);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fits(
        renderer: &RatatuiInlineRenderer,
        model: SuggestionListModel<'_>,
        prompt_rows: usize,
        terminal_rows: usize,
    ) -> bool {
        terminal_rows > 0 && renderer.suggestion_list_row_count(model, prompt_rows) <= terminal_rows
    }

    fn items(count: usize) -> Vec<SuggestionItem> {
        (0..count)
            .map(|index| SuggestionItem {
                id: format!("candidate-{index}"),
                question: format!("帮我核对问题 {index} 的来源与影响"),
            })
            .collect()
    }

    #[test]
    fn single_question_has_no_number_or_marker() {
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(Language::ZhCn);
        let owned = items(1);
        let lines = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: true,
        });
        assert!(lines[1].starts_with("  帮我核对问题 0"));
        assert!(!lines[1].contains("1."));
        assert!(!lines[1].contains('›'));
        assert!(lines.last().unwrap().contains("Tab 填入"));
        assert!(!lines.last().unwrap().contains("Shift+Tab"));
    }

    #[test]
    fn multiple_questions_number_rows_and_mark_only_the_active_one() {
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(Language::ZhCn);
        let owned = items(3);
        let lines = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 1,
            interactive: true,
        });
        assert!(lines[1].starts_with("    1. "));
        assert!(lines[2].starts_with("  › 2. "));
        assert!(lines[3].starts_with("    3. "));
        assert_eq!(lines.iter().filter(|line| line.contains('›')).count(), 1);
        assert!(lines.last().unwrap().contains("Shift+Tab 切换"));
    }

    #[test]
    fn narrow_width_wraps_with_hanging_indent_and_keeps_identifiers() {
        let renderer = RatatuiInlineRenderer::with_width(40).with_language(Language::ZhCn);
        let owned = vec![SuggestionItem {
            id: "service".into(),
            question: "帮我确认 inventory-worker-background-sync.service 实际加载的配置文件".into(),
        }];
        let lines = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: true,
        });
        let question_lines: Vec<&String> = lines.iter().skip(1).take(lines.len() - 2).collect();
        assert!(question_lines.len() > 1);
        assert!(question_lines[0].starts_with("  帮我确认"));
        for line in &question_lines[1..] {
            assert!(line.starts_with("  "));
            assert!(!line.starts_with("   "));
        }
        let joined: String = question_lines
            .iter()
            .map(|line| line.trim_start().to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("inventory-worker-background-sync.service"));
        for line in &lines {
            assert!(display_width(line) <= 40, "{line}");
        }
    }

    #[test]
    fn plain_output_has_no_escape_sequences() {
        let mut renderer = RatatuiInlineRenderer::with_width(80).with_language(Language::EnUs);
        renderer.styled = true;
        let owned = items(2);
        let styled = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: true,
        });
        assert!(styled.iter().any(|line| line.contains("\x1b[90m")));
        renderer.styled = false;
        let plain = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: true,
        });
        assert!(plain.iter().all(|line| !line.contains('\x1b')));
    }

    #[test]
    fn row_count_includes_prompt_and_separator_and_gates_live_region() {
        let renderer = RatatuiInlineRenderer::with_width(40).with_language(Language::ZhCn);
        let owned = items(3);
        let model = SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: true,
        };
        let rows = renderer.suggestion_list_row_count(model, 2);
        assert_eq!(rows, renderer.suggestion_list_lines(model).len() + 3);
        assert!(fits(&renderer, model, 2, rows));
        assert!(!fits(&renderer, model, 2, rows - 1));
        assert!(!fits(&renderer, model, 2, 0));
        let empty = SuggestionListModel {
            items: &[],
            active: 0,
            interactive: true,
        };
        assert_eq!(renderer.suggestion_list_row_count(empty, 2), 0);
        assert!(renderer.suggestion_list_lines(empty).is_empty());
    }

    #[test]
    fn three_max_length_questions_overflow_a_40x24_viewport() {
        let items: Vec<SuggestionItem> = (0..3)
            .map(|index| SuggestionItem {
                id: format!("p{index}"),
                question: "x".repeat(512),
            })
            .collect();
        let payload = suggestion_surface_payload(&items, 40, 24);
        assert!(payload.rows + 1 > 24, "rows={}", payload.rows);
        assert_eq!(payload.variants.len(), 3);
        assert_eq!(payload.ids.len(), 3);
    }

    #[test]
    fn read_only_lists_omit_interaction_shortcuts() {
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(Language::ZhCn);
        let owned = items(2);
        let lines = renderer.suggestion_list_lines(SuggestionListModel {
            items: &owned,
            active: 0,
            interactive: false,
        });
        assert!(lines.iter().all(|line| !line.contains("Tab")));
        assert!(lines.iter().all(|line| !line.contains('›')));
    }
}
