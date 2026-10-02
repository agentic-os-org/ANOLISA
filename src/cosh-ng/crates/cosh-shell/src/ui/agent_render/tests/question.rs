use super::*;
use crate::ui::display_width;

#[test]
fn question_panel_renders_options_with_compact_instructions() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let options = vec![
        "Green".to_string(),
        "Blue".to_string(),
        "Custom".to_string(),
    ];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-1",
            question: "Choose a color for the next step",
            options: &options,
            selected_option: 1,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent question"), "{text}");
    assert!(!text.contains("Agent question q-1"), "{text}");
    assert!(text.contains("Choose a color for the next step"), "{text}");
    assert!(text.contains("Select one:"), "{text}");
    assert!(text.contains("[1] Green"), "{text}");
    assert!(text.contains("[2] Blue"), "{text}");
    assert!(text.contains("> [2] Blue"), "{text}");
    assert!(text.contains("Keys:"), "{text}");
    assert!(text.contains("Left/Right move"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert!(!text.contains("│Choose"), "{text}");
    assert!(!text.contains("│Options"), "{text}");
    assert!(!text.contains("Agent is asking for input"), "{text}");
    assert!(!text.contains("same Agent session"), "{text}");
    assert!(!text.contains("/answer"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn question_panel_uses_zh_labels_without_translating_options() {
    let renderer = RatatuiInlineRenderer::with_width(100).with_language(crate::Language::ZhCn);
    let options = vec!["Green".to_string(), "Blue".to_string()];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-1",
            question: "Choose a color for the next step",
            options: &options,
            selected_option: 1,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent 问题"), "{text}");
    assert!(text.contains("Choose a color for the next step"), "{text}");
    assert!(text.contains("选择一项:"), "{text}");
    assert!(text.contains("[1] Green"), "{text}");
    assert!(text.contains("> [2] Blue"), "{text}");
    assert!(text.contains("[3] 其他..."), "{text}");
    assert!(text.contains("按键:"), "{text}");
    assert!(text.contains("左/右移动"), "{text}");
    assert!(text.contains("Enter 发送"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn question_panel_renders_multiple_choice_toggles() {
    let renderer = RatatuiInlineRenderer::with_width(100);
    let options = vec![
        "Lint".to_string(),
        "Unit tests".to_string(),
        "Raw shell smoke".to_string(),
    ];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-1",
            question: "Choose checks to run",
            options: &options,
            selected_option: 1,
            selected_options: &[0, 1],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Multiple,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Choose checks to run"), "{text}");
    assert!(text.contains("Select one or more:"), "{text}");
    assert!(text.contains("[x] [1] Lint"), "{text}");
    assert!(text.contains("[x] [2] Unit tests"), "{text}");
    assert!(text.contains("[ ] [3] Raw shell smoke"), "{text}");
    assert!(text.contains("[4] Other..."), "{text}");
    assert!(text.contains("Space toggle"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert_rendered_width(&text, 100);
}

#[test]
fn question_panel_keeps_cjk_and_emoji_borders_aligned() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let options = vec![
        "分析 CPU 占用 🧪".to_string(),
        "分析内存占用并保留同一会话".to_string(),
        "只解释刚才失败的命令".to_string(),
    ];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-宽",
            question: "请选择下一步要分析的方向，允许补充自定义说明",
            options: &options,
            selected_option: 3,
            selected_options: &[0, 1],
            custom_answer: "重点看中文路径和表格边线",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Multiple,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent question"), "{text}");
    assert!(text.contains("Select one or more:"), "{text}");
    assert!(text.contains("[x] [1] 分析 CPU 占用 🧪"), "{text}");
    assert!(text.contains("[x] [2] 分析内存占用"), "{text}");
    assert!(text.contains("> [4] Answer: 重点看中文路径"), "{text}");
    assert!(text.contains("type answer"), "{text}");
    assert_rendered_width(&text, 54);
    assert_box_lines_aligned(&text, 54);
}

#[test]
fn question_panel_wraps_long_question_and_options_without_dropping_tail() {
    let renderer = RatatuiInlineRenderer::with_width(54);
    let options = vec![
        "Use the short safe command and keep the same provider session".to_string(),
        "Explain the failure first, then ask before any tool request".to_string(),
    ];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-2",
            question: "Choose how the Agent should continue after the failing command while preserving shell-first control",
            options: &options,
            selected_option: 1,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("after the"), "{text}");
    assert!(text.contains("failing command"), "{text}");
    assert!(text.contains("shell-first"), "{text}");
    assert!(text.contains("control"), "{text}");
    assert!(text.contains("same"), "{text}");
    assert!(text.contains("provider session"), "{text}");
    assert!(text.contains("│       provider session"), "{text}");
    assert!(text.contains("│       any tool request"), "{text}");
    assert!(text.contains("before"), "{text}");
    assert!(text.contains("any tool request"), "{text}");
    assert!(text.contains("tool request"), "{text}");
    assert!(text.contains("Left/Right"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert!(!text.contains("session; no shell command runs"), "{text}");
    assert_rendered_width(&text, 54);
}

#[test]
fn question_panel_free_text_only_omits_fake_option_section() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-4",
            question: "Tell me the branch name to inspect",
            options: &[],
            selected_option: 0,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent question"), "{text}");
    assert!(!text.contains("Agent question q-4"), "{text}");
    assert!(
        text.contains("Tell me the branch name to inspect"),
        "{text}"
    );
    assert!(text.contains("Type answer"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert!(text.contains("Answer:"), "{text}");
    assert!(text.contains("Type your answer..."), "{text}");
    assert!(!text.contains("[1]"), "{text}");
    assert_rendered_width(&text, 90);
}

#[test]
fn question_panel_free_text_only_renders_input_value() {
    let renderer = RatatuiInlineRenderer::with_width(90).with_language(crate::Language::ZhCn);
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-4",
            question: "你的爱好是什么？",
            options: &[],
            selected_option: 0,
            selected_options: &[],
            custom_answer: "我的爱好是撸猫",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("回答: 我的爱好是撸猫"), "{text}");
    assert!(!text.contains("[1]"), "{text}");
    assert!(text.contains("输入回答"), "{text}");
    assert_rendered_width(&text, 90);
}

#[test]
fn question_panel_required_ghost_replaces_default_in_place() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let model = QuestionPanelModel {
        id: "q-4",
        question: "What should I inspect?",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::Required,
    };

    let text = renderer.question_panel_lines(model.clone()).join("\n");
    let cursor = renderer
        .question_cursor_placement(&model)
        .expect("free-text cursor");

    assert!(text.contains("Please enter an answer"), "{text}");
    assert!(!text.contains("Type your answer..."), "{text}");
    assert_eq!(cursor.column, 10);
}

#[test]
fn question_panel_whitespace_input_keeps_required_ghost_and_cursor_anchor() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let model = QuestionPanelModel {
        id: "q-space",
        question: "What should I inspect?",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "   ",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::Required,
    };

    let text = renderer.question_panel_lines(model.clone()).join("\n");
    assert!(text.contains("Please enter an answer"), "{text}");
    let cursor = renderer.question_cursor_placement(&model).unwrap();
    assert_eq!((cursor.row, cursor.column), (2, 10));
}

#[test]
fn question_cursor_tracks_the_end_of_real_input() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let model = QuestionPanelModel {
        id: "q-editing",
        question: "What should I inspect?",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "main",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::None,
    };

    let cursor = renderer.question_cursor_placement(&model).unwrap();
    assert_eq!((cursor.row, cursor.column), (2, 14));
    let text = renderer.question_panel_lines(model).join("\n");
    assert!(text.contains("Answer: main"), "{text}");
    assert!(!text.contains("Type your answer"), "{text}");
}

#[test]
fn question_cursor_uses_display_width_on_the_final_wrapped_row() {
    let renderer = RatatuiInlineRenderer::with_width(40);
    let model = QuestionPanelModel {
        id: "q-wrapped-cjk",
        question: "Continue?",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "你好🙂你好🙂你好🙂你好🙂你好🙂",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::None,
    };

    let cursor = renderer.question_cursor_placement(&model).unwrap();
    assert!(cursor.row > 2);
    let lines = renderer.question_panel_lines(model);
    let input_row = lines[cursor.row]
        .strip_suffix('│')
        .expect("styled panel border")
        .trim_end();
    assert_eq!(cursor.column, display_width(input_row));
    let text = lines.join("\n");
    assert_box_lines_aligned(&text, 40);
}

#[test]
fn custom_option_cursor_matches_long_unbroken_rendered_input() {
    for answer in [
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "你好世界你好世界你好世界你好世界你好世界",
        "🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂",
    ] {
        let renderer = RatatuiInlineRenderer::with_width(40);
        let options = vec!["Red".to_string()];
        let model = QuestionPanelModel {
            id: "q-custom-wrapped",
            question: "Choose or type",
            options: &options,
            selected_option: 1,
            selected_options: &[],
            custom_answer: answer,
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        };

        let cursor = renderer.question_cursor_placement(&model).unwrap();
        let lines = renderer.question_panel_lines(model);
        let instruction_row = lines
            .iter()
            .position(|line| line.contains("Keys:"))
            .expect("instruction row");
        assert_eq!(cursor.row + 1, instruction_row, "{answer:?}\n{lines:#?}");
        let input_row = lines[cursor.row]
            .strip_suffix('│')
            .expect("styled panel border")
            .trim_end();
        assert_eq!(
            cursor.column,
            display_width(input_row),
            "{answer:?}\n{lines:#?}"
        );
    }
}

#[test]
fn plain_tty_question_cursor_uses_plain_panel_columns() {
    let renderer = RatatuiInlineRenderer {
        width: 90,
        plain: true,
        styled: true,
        language: crate::Language::EnUs,
    };
    let model = QuestionPanelModel {
        id: "q-plain",
        question: "What should I inspect?",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::None,
    };
    let cursor = renderer.question_cursor_placement(&model).unwrap();
    assert_eq!((cursor.row, cursor.column), (2, 8));
    assert!(renderer.question_panel_lines(model)[2].starts_with("Answer: ["));
}

#[test]
fn question_panel_multiple_empty_feedback_replaces_heading() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let options = vec!["Lint".to_string(), "Unit tests".to_string()];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-5",
            question: "Choose checks",
            options: &options,
            selected_option: 0,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Multiple,
            input_feedback: QuestionInputFeedback::SelectionRequired,
        })
        .join("\n");

    assert!(
        text.contains("Select at least one option or enter an answer"),
        "{text}"
    );
    assert!(!text.contains("Select one or more:"), "{text}");
}

#[test]
fn question_panel_write_preserves_ratatui_styles_for_terminal_output() {
    let renderer = RatatuiInlineRenderer {
        width: 60,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let options = vec![
        "Approve\n    For teams and companies • Usage-based billing with dedicated capacity"
            .to_string(),
        "Deny\nsecond line".to_string(),
    ];
    let mut output = Vec::new();

    renderer
        .write_question_panel(
            &mut output,
            QuestionPanelModel {
                id: "q-1",
                question: "Choose an answer",
                options: &options,
                selected_option: 1,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::None,
            },
        )
        .expect("render question panel");

    let text = String::from_utf8(output).expect("utf8 panel");
    let clean = strip_ansi_escape(&text);
    assert!(text.contains("\x1b["), "{text:?}");
    assert!(clean.contains("Agent question"), "{clean}");
    assert!(!clean.contains("Agent question q-1"), "{clean}");
    assert!(clean.contains("[2]"), "{clean}");

    let gray_detail_rows = text
        .lines()
        .filter(|line| line.contains("\u{1b}[0;90m      "))
        .count();
    assert_eq!(gray_detail_rows, 2, "{text:?}");
    assert!(!text.contains("\u{1b}[0;90m      second line"), "{text:?}");
}

#[test]
fn question_panel_default_ghost_is_dim_gray() {
    let renderer = RatatuiInlineRenderer {
        width: 90,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let mut output = Vec::new();
    renderer
        .write_question_panel(
            &mut output,
            QuestionPanelModel {
                id: "q-default-style",
                question: "What should I inspect?",
                options: &[],
                selected_option: 0,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: true,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::None,
            },
        )
        .unwrap();
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("Type your answer..."), "{output:?}");
    assert!(output.contains("\u{1b}[0;2;90m"), "{output:?}");
}

#[test]
fn disabled_question_input_visual_has_no_ghost_or_cursor() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let model = QuestionPanelModel {
        id: "auth-secret",
        question: "Enter a secret",
        options: &[],
        selected_option: 0,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::Disabled,
    };

    assert!(renderer.question_cursor_placement(&model).is_none());
    let text = renderer.question_panel_lines(model).join("\n");
    assert!(!text.contains("Type your answer"), "{text}");
    assert!(!text.contains('›'), "{text}");
}

#[test]
fn question_panel_other_required_ghost_is_dim_yellow() {
    let renderer = RatatuiInlineRenderer {
        width: 100,
        plain: false,
        styled: true,
        language: crate::Language::EnUs,
    };
    let options = vec!["Green".to_string()];
    let mut output = Vec::new();
    renderer
        .write_question_panel(
            &mut output,
            QuestionPanelModel {
                id: "q-other-style",
                question: "Choose",
                options: &options,
                selected_option: 1,
                selected_options: &[],
                custom_answer: "   ",
                allow_free_text: true,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Required,
            },
        )
        .unwrap();
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("Please enter an answer"), "{output:?}");
    assert!(output.contains("\u{1b}[0;2;33m"), "{output:?}");
}

#[test]
fn plain_question_panel_keeps_compact_card_instructions() {
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let options = vec!["Green".to_string(), "Blue".to_string()];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-1",
            question: "Choose a color",
            options: &options,
            selected_option: 0,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent question"), "{text}");
    assert!(!text.contains("Agent question q-1"), "{text}");
    assert!(text.contains("[1] Green"), "{text}");
    assert!(text.contains("[3] Other..."), "{text}");
    assert!(text.contains("> [1] Green"), "{text}");
    assert!(text.contains("Left/Right move"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert!(!text.contains("Agent is asking for input"), "{text}");
    assert!(!text.contains("Effect:"), "{text}");
    assert!(!text.contains('╭'), "{text}");
}

#[test]
fn plain_question_panel_wraps_long_question_and_options() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let options = vec![
        "Use the short safe command and keep the same provider session".to_string(),
        "Explain the failure first, then ask before any tool request".to_string(),
    ];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-2",
            question: "Choose how the Agent should continue after the failing command while preserving shell-first control",
            options: &options,
            selected_option: 2,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("Agent question"), "{text}");
    assert!(!text.contains("Agent question q-2"), "{text}");
    assert!(
        text.contains("Choose how the Agent should continue after the"),
        "{text}"
    );
    assert!(
        text.contains("failing command while preserving shell-first"),
        "{text}"
    );
    assert!(text.contains("control"), "{text}");
    assert!(
        text.contains("  [1] Use the short safe command and keep the"),
        "{text}"
    );
    assert!(text.contains("      same provider session"), "{text}");
    assert!(
        text.contains("  [2] Explain the failure first, then ask"),
        "{text}"
    );
    assert!(text.contains("      before any tool request"), "{text}");
    assert!(text.contains("> [3] Other..."), "{text}");
    assert!(
        text.contains("Left/Right move | type answer | Enter"),
        "{text}"
    );
    assert!(text.contains("send"), "{text}");
    assert_rendered_width(&text, 50);
}

#[test]
fn question_panel_renders_custom_answer_as_selectable_option() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let options = vec!["Green".to_string(), "Blue".to_string()];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-3",
            question: "Choose a color or provide your own",
            options: &options,
            selected_option: 2,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("[1] Green"), "{text}");
    assert!(text.contains("[2] Blue"), "{text}");
    assert!(text.contains("[3] Other..."), "{text}");
    assert!(text.contains("> [3] Other..."), "{text}");
    assert!(text.contains("type answer"), "{text}");
    assert!(text.contains("Enter send"), "{text}");
    assert_rendered_width(&text, 90);
}

#[test]
fn question_panel_renders_custom_answer_input_value() {
    let renderer = RatatuiInlineRenderer::with_width(90);
    let options = vec!["Green".to_string(), "Blue".to_string()];
    let text = renderer
        .question_panel_lines(QuestionPanelModel {
            id: "q-3",
            question: "Choose a color or provide your own",
            options: &options,
            selected_option: 2,
            selected_options: &[],
            custom_answer: "红色",
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
        })
        .join("\n");

    assert!(text.contains("> [3] Answer: 红色"), "{text}");
    assert!(!text.contains("Custom answer"), "{text}");
    assert_rendered_width(&text, 90);
}

#[test]
fn question_answer_panel_renders_same_session_receipt() {
    let renderer = RatatuiInlineRenderer::with_width(56);
    let text = renderer
        .question_answer_panel_lines(QuestionAnswerPanelModel {
            id: "q-1",
            question: "Choose a color for the next step while keeping shell-first control",
            answer: "蓝色",
            message: "Sent to Agent; no command ran.",
        })
        .join("\n");

    assert!(text.contains("Answer"), "{text}");
    assert!(!text.contains("Answer sent"), "{text}");
    assert!(text.contains("Answer: 蓝色"), "{text}");
    assert!(!text.contains("Sent to Agent"), "{text}");
    assert!(!text.contains("no command ran"), "{text}");
    assert!(!text.contains("same Agent session"), "{text}");
    assert!(!text.contains("no command was executed"), "{text}");
    assert!(!text.contains("Question:"), "{text}");
    assert!(!text.contains("shell-first control"), "{text}");
    assert!(!text.contains("│Answer"), "{text}");
    assert!(!text.contains('┌'), "{text}");
    assert!(!text.contains('└'), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(!text.contains("q-1 Choose a color"), "{text}");
    assert_rendered_width(&text, 56);
}

#[test]
fn plain_question_answer_panel_wraps_long_receipt() {
    let renderer = RatatuiInlineRenderer::plain_with_width(50);
    let text = renderer
        .question_answer_panel_lines(QuestionAnswerPanelModel {
            id: "q-3",
            question: "Choose how the Agent should continue while preserving shell-first control",
            answer: "Run only the smallest safe check and keep the same provider session",
            message: "Sent to Agent; no command ran.",
        })
        .join("\n");

    assert!(text.contains("Answer"), "{text}");
    assert!(!text.contains("Answer sent"), "{text}");
    assert!(
        text.contains("Answer: Run only the smallest safe check and"),
        "{text}"
    );
    assert!(
        text.contains("        keep the same provider session"),
        "{text}"
    );
    assert!(!text.contains("Sent to Agent"), "{text}");
    assert!(!text.contains("no command ran"), "{text}");
    assert!(
        !text.contains("Continuing the same Agent session"),
        "{text}"
    );
    assert!(!text.contains("no command was executed"), "{text}");
    assert!(!text.contains("Question:"), "{text}");
    assert!(!text.contains("preserving shell-first control"), "{text}");
    assert!(!text.contains('╭'), "{text}");
    assert_rendered_width(&text, 50);
}
