use std::io::Write;

use crate::config::{detect_language_from_env, read_guidance_language};
use crate::i18n::{I18n, MessageId};

pub(crate) fn run_cli() -> i32 {
    let language = read_guidance_language().unwrap_or_else(|_| detect_language_from_env());
    let i18n = I18n::new(language);
    i32::from(
        std::io::stdout()
            .lock()
            .write_all(i18n.t(MessageId::CoshRootHelp).as_bytes())
            .is_err(),
    )
}
