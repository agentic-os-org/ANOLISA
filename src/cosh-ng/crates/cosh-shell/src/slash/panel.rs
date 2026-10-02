use crate::runtime::prelude::*;

pub(super) fn render_notice_panel<W: Write>(
    output: &mut W,
    title: &str,
    body: Vec<String>,
    footer: Option<&str>,
) -> std::io::Result<()> {
    RatatuiInlineRenderer::for_terminal().write_slash_notice_panel(
        output,
        NoticePanelModel {
            title,
            body,
            footer,
        },
    )
}
