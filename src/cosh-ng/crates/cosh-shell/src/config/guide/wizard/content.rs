//! One content source for the wizard pages and the static guide text.

use crate::i18n::{I18n, MessageId};
use crate::ui::RatatuiInlineRenderer;

pub(crate) const CONTENT_WIDTH: u16 = 76;

/// Home order is fixed: the quick guide first, then the default login shell.
pub(crate) const ENTRIES: [(MessageId, MessageId); 6] = [
    (
        MessageId::GuideWizardEntryStart,
        MessageId::GuideWizardStart,
    ),
    (
        MessageId::GuideWizardEntryLogin,
        MessageId::GuideWizardLoginBody,
    ),
    (MessageId::GuideWizardEntryAi, MessageId::GuideWizardAiBody),
    (
        MessageId::GuideWizardEntryHealth,
        MessageId::GuideWizardHealthBody,
    ),
    (
        MessageId::GuideWizardEntryLanguage,
        MessageId::GuideWizardLanguageBody,
    ),
    (
        MessageId::GuideWizardEntryTips,
        MessageId::GuideWizardTipsBody,
    ),
];

pub(crate) fn start_markdown(i18n: I18n) -> &'static str {
    i18n.t(MessageId::GuideWizardStart)
}

pub(crate) fn entry_label(i18n: I18n, index: usize) -> &'static str {
    i18n.t(ENTRIES[index].0)
}

fn settings_markdown(i18n: I18n) -> String {
    let mut document = String::new();
    for (label, body) in ENTRIES {
        document.push_str("## ");
        document.push_str(i18n.t(label));
        document.push_str("\n\n");
        document.push_str(i18n.t(body));
        document.push('\n');
    }
    document
}

fn render_plain(i18n: I18n, markdown: &str) -> String {
    RatatuiInlineRenderer::plain_with_width(CONTENT_WIDTH)
        .with_language(i18n.language())
        .markdown_text_lines(markdown)
        .join("\n")
}

fn body(i18n: I18n) -> String {
    let document = format!(
        "{}{}",
        settings_markdown(i18n),
        i18n.t(MessageId::GuideWizardPlainFooter),
    );
    render_plain(i18n, &document)
}

/// Static guide text: no ANSI, no key reads, no configuration access.
pub(crate) fn plain_text(i18n: I18n) -> String {
    format!(
        "{}\n\n{}\n",
        i18n.t(MessageId::GuideWizardTitle),
        body(i18n)
    )
}

pub(crate) fn usage_text(i18n: I18n) -> String {
    format!("{}\n{}\n", i18n.t(MessageId::GuideUsage), body(i18n))
}
