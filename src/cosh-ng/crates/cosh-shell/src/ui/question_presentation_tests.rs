use crate::types::QuestionSelectionMode;
use crate::ui::agent_render::{QuestionInputFeedback, QuestionPanelModel, RatatuiInlineRenderer};
use crate::ui::QuestionPanelPresentation;

#[test]
fn question_presentation_override_changes_only_card_chrome() {
    let options = vec!["Continue".to_owned(), "Cancel".to_owned()];
    for renderer in [
        RatatuiInlineRenderer::with_width(64),
        RatatuiInlineRenderer::plain_with_width(64),
    ] {
        let model = QuestionPanelModel {
            id: "q-presentation",
            question: "Choose the next action",
            options: &options,
            selected_option: 0,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::Disabled,
        };
        let default_lines = renderer.question_panel_lines(model.clone());
        assert_eq!(
            default_lines,
            renderer.question_panel_lines_with_presentation(model.clone(), Default::default())
        );

        let overridden = renderer
            .question_panel_lines_with_presentation(
                model,
                QuestionPanelPresentation::new(
                    "Persistent Task",
                    "Task keys · ",
                    "Enter activate · Esc back · Ctrl+C cancel",
                ),
            )
            .join("\n");
        assert!(overridden.contains("Persistent Task"), "{overridden}");
        assert!(overridden.contains("Task keys"), "{overridden}");
        assert!(overridden.contains("Enter activate"), "{overridden}");
        assert!(!overridden.contains("Agent question"), "{overridden}");
        assert!(!overridden.contains("Enter send"), "{overridden}");
        assert!(
            overridden.contains("Choose the next action"),
            "{overridden}"
        );
        assert!(overridden.contains("[1] Continue"), "{overridden}");
    }
}
