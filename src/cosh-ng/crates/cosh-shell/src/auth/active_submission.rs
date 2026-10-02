//! Active-run auth submission without a premature persistence confirmation.

use std::collections::HashSet;
use std::io::{self, Write};

use crate::runtime::prelude::{Language, NoticePanelModel, RatatuiInlineRenderer};

pub(super) fn finish_active_submission<W: Write>(
    result: Result<(), String>,
    auth_id: &str,
    completed_ids: &mut HashSet<String>,
    language: Language,
    output: &mut W,
) -> io::Result<()> {
    if result.is_err() {
        let renderer = RatatuiInlineRenderer::for_terminal().with_language(language);
        renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title: "Auth failed",
                body: vec![
                    "Unable to send credentials to cosh-core.".to_string(),
                    "Run /auth again after the current run finishes.".to_string(),
                ],
                footer: None,
            },
        )?;
    }

    completed_ids.insert(auth_id.to_string());
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_submission_does_not_claim_credentials_were_saved() {
        let mut output = Vec::new();
        let mut completed_ids = HashSet::new();

        finish_active_submission(
            Ok(()),
            "auth-1",
            &mut completed_ids,
            Language::EnUs,
            &mut output,
        )
        .expect("finish active submission");

        assert!(output.is_empty());
        assert!(completed_ids.contains("auth-1"));
    }
}
