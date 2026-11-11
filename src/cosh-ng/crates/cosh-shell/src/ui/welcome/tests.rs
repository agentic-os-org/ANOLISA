use super::*;
use crate::ui::agent_render::display_width;
use crate::ui::wrap::strip_ansi_escape;
use crate::Language;

fn model(compact: bool) -> WelcomeModel {
    WelcomeModel {
        compact,
        context: if compact {
            Vec::new()
        } else {
            vec!["Shell: bash · approval: auto".into(), "cwd: ~".into()]
        },
        capability: "Run commands or ask AI questions.".into(),
        notice: None,
        extra: Vec::new(),
    }
}

#[test]
fn welcome_gives_the_two_entries_one_shared_card_with_different_density() {
    let renderer = RatatuiInlineRenderer::with_width(80);
    let ordinary = renderer.welcome_lines(model(false));
    let compact = renderer.welcome_lines(model(true));
    assert_eq!(ordinary.len(), 11, "{ordinary:?}");
    assert_eq!(compact.len(), 3, "{compact:?}");
    assert_eq!(ordinary.get(7), compact.first());
    assert!(ordinary[..6].iter().all(|line| !line.contains('│')));
    assert_eq!(ordinary.last(), compact.last());
    for line in LOGO {
        assert!(ordinary.iter().any(|row| row.contains(line)));
    }
    assert!(!compact.join("\n").contains('█'));
}

#[test]
fn welcome_keeps_long_information_and_help_inside_actual_columns() {
    for width in [20, 32, 40, 60, 80, 160] {
        let renderer = RatatuiInlineRenderer::with_width(width).within_columns(width);
        let mut content = model(false);
        content.context.push(
            "cwd: ~/workspace/customer-services/observability/production-agent/integration".into(),
        );
        content.notice = Some("信任模式：允许的工具请求会自动执行。".into());
        content
            .extra
            .push("This startup check has a long description that must survive wrapping.".into());
        let lines = renderer.welcome_lines(content);
        assert!(
            lines
                .iter()
                .all(|line| display_width(line) <= usize::from(width.min(80))),
            "{width}: {lines:?}"
        );
        let flat = lines.join("").replace(['│', ' '], "");
        assert!(
            flat.contains(
                "~/workspace/customer-services/observability/production-agent/integration"
            ),
            "{lines:?}"
        );
        assert!(
            flat.contains("信任模式：允许的工具请求会自动执行。"),
            "{lines:?}"
        );
        assert!(
            flat.contains("Thisstartupcheckhasalongdescriptionthatmustsurvivewrapping."),
            "{lines:?}"
        );
        assert!(lines.last().unwrap().contains("/help"));
    }
}

#[test]
fn welcome_preserves_bilingual_color_and_plain_text_fallback() {
    for language in [Language::ZhCn, Language::EnUs] {
        let mut renderer = RatatuiInlineRenderer::with_width(80).with_language(language);
        renderer.styled = true;
        let rich = renderer.welcome_lines(model(false)).join("\n");
        assert!(rich.contains("\x1b["), "{rich}");
        assert!(strip_ansi_escape(&rich).contains(match language {
            Language::ZhCn => "帮助： /help",
            Language::EnUs => "Help: /help",
        }));
        renderer.plain = true;
        let plain = renderer.welcome_lines(model(false)).join("\n");
        assert!(
            !plain.contains("\x1b[") && !plain.contains('█') && !plain.contains('╭'),
            "{plain}"
        );
        assert!(plain.contains("Cosh AI Shell") && plain.contains("/help"));
    }
}

#[test]
fn welcome_combines_short_context_and_starts_with_capability() {
    let lines = RatatuiInlineRenderer::with_width(80).welcome_lines(model(false));
    assert!(
        lines
            .get(8)
            .is_some_and(|line| line.contains("Run commands or ask AI questions.")),
        "{lines:?}"
    );
    assert!(
        lines
            .get(9)
            .is_some_and(|line| line.contains("Shell: bash · approval: auto · cwd: ~")),
        "{lines:?}"
    );
}

#[test]
fn welcome_keeps_a_long_directory_on_its_own_line() {
    let mut content = model(false);
    content.context[1] =
        "cwd: /workspace/customer-services/observability/production/integration".into();
    let lines = RatatuiInlineRenderer::with_width(80).welcome_lines(content);
    let metadata = lines
        .iter()
        .find(|line| line.contains("Shell: bash"))
        .unwrap();
    assert!(!metadata.contains("cwd:"), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("cwd: /workspace")),
        "{lines:?}"
    );
}

#[test]
fn welcome_uses_default_foreground_for_readable_metadata() {
    let mut renderer = RatatuiInlineRenderer::with_width(80);
    renderer.styled = true;
    let lines = renderer.welcome_lines(model(false));
    let metadata = lines
        .iter()
        .find(|line| line.contains("Shell: bash"))
        .unwrap();
    assert!(!metadata.contains(";90m"), "{metadata:?}");
    assert!(!metadata.contains(";37m"), "{metadata:?}");
    assert!(lines.last().unwrap().contains("/help"));
}

#[test]
fn ordinary_welcome_has_complete_letter_shapes_and_one_separator() {
    let lines = RatatuiInlineRenderer::with_width(80).welcome_lines(model(false));
    let expected = [
        "  ██████╗  ██████╗  ███████╗ ██╗  ██╗",
        " ██╔════╝ ██╔═══██╗ ██╔════╝ ██║  ██║",
        " ██║      ██║   ██║ ███████╗ ███████║",
        " ██║      ██║   ██║ ╚════██║ ██╔══██║",
        " ╚██████╗ ╚██████╔╝ ███████║ ██║  ██║",
        "  ╚═════╝  ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
    ];
    for (row, expected) in expected.iter().enumerate() {
        assert_eq!(lines[row].trim_end(), format!(" {expected}"));
    }
    assert!(lines[6].trim().is_empty());
    assert!(lines[7].contains("Cosh AI Shell"));
}

#[test]
fn narrow_welcome_uses_its_title_instead_of_a_squeezed_logo() {
    for width in [20, 32, 40] {
        let renderer = RatatuiInlineRenderer::with_width(width).within_columns(width);
        let text = renderer.welcome_lines(model(false)).join("\n");
        assert!(!text.contains('█'), "{text}");
        assert!(text.contains("Cosh AI Shell"), "{text}");
    }
}

#[test]
fn ordinary_welcome_preserves_the_original_blue_cyan_gradient() {
    let mut renderer = RatatuiInlineRenderer::with_width(80);
    renderer.styled = true;
    let lines = renderer.welcome_lines(model(false));
    for (line, color) in lines.iter().take(6).zip([33, 33, 39, 39, 117, 117]) {
        assert!(line.contains(&format!("38;5;{color}")), "{line:?}");
    }
    let compact = renderer.welcome_lines(model(true)).join("\n");
    assert!(!compact.contains('█') && !compact.contains("38;5;117"));
}
