#[path = "adapter/public.rs"]
pub mod adapter;
#[path = "agent/public.rs"]
pub mod agent;
mod command;
#[cfg(test)]
#[path = "tools/command_risk_quoted_tests.rs"]
mod command_risk_quoted_tests;
#[allow(dead_code, unused_imports)]
mod config;
#[allow(dead_code, unused_imports)]
mod diagnostics;
#[allow(dead_code, unused_imports)]
#[path = "evidence/public.rs"]
mod evidence;
#[allow(dead_code, unused_imports)]
#[path = "hooks/public.rs"]
mod hooks;
#[cfg(test)]
#[path = "evidence/redaction_boundary_tests.rs"]
mod redaction_boundary_tests;

#[doc(hidden)]
#[path = "ui/worker_public.rs"]
pub mod __ui_worker;

#[cfg(test)]
#[path = "ui/card_kind_tests.rs"]
mod card_kind_tests;
#[cfg(test)]
#[path = "types/card_tests.rs"]
mod card_tests;
mod i18n;
mod input;
#[allow(dead_code)]
mod insight;
#[path = "journal/public.rs"]
pub mod journal;
#[path = "ledger/public.rs"]
pub mod ledger;
#[path = "parser/public.rs"]
pub mod parser;
#[cfg(test)]
#[path = "input/path_prompt_tests.rs"]
mod path_prompt_tests;
#[allow(dead_code)]
#[path = "question/public.rs"]
mod question;
#[cfg(test)]
#[path = "ui/question_presentation_tests.rs"]
mod question_presentation_tests;
// Worker-path UI tests are declared here rather than in `ui/public.rs` on
// purpose: `ui/public.rs` is loaded by both `lib.rs` and `main.rs`, so a test
// module registered there is compiled into two test binaries and counts twice
// against the lib/bin overlap ceiling in `check-test-inventory.sh`. Declaring
// them at the crate root keeps each test in exactly one target while the
// production modules stay shared.
#[cfg(test)]
#[path = "ui/approval_surface_tests.rs"]
mod approval_surface_tests;
#[path = "raw_input/public.rs"]
pub mod raw_input;
#[cfg(test)]
#[path = "tools/readonly_interceptor_tests.rs"]
mod readonly_interceptor_tests;
#[cfg(test)]
#[path = "tools/readonly_rules/traversal_tests.rs"]
mod readonly_traversal_tests;
#[cfg(test)]
#[path = "ui/render_surface_tests.rs"]
mod render_surface_tests;
#[path = "shell_host/public.rs"]
pub mod shell_host;
#[allow(dead_code)]
#[path = "slash/public.rs"]
mod slash;
#[cfg(test)]
#[path = "ui/surface_frame_tests.rs"]
mod surface_frame_tests;
#[allow(dead_code, unused_imports)]
mod tools;
#[path = "types/public.rs"]
pub mod types;
#[allow(dead_code, unused_imports)]
#[path = "ui/public.rs"]
mod ui;
#[cfg(test)]
#[path = "ui/worker_protocol/tests.rs"]
mod worker_protocol_tests;
#[cfg(test)]
#[path = "ui/wrap_tests.rs"]
mod wrap_tests;

pub use adapter::{AuthFieldInfo, AuthProviderInfo, AuthResponse};
pub use config::{
    language_config_status, load_config, parse_language_setting, resolve_language_setting,
    write_user_language_config, CoshConfig, Language, LanguageConfigStatus,
};
pub use i18n::{I18n, MessageId};
