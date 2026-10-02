pub mod context_window;
pub(crate) mod model;
pub(crate) mod output_policy;
mod output_text;
mod prelude;
mod redaction;
pub(crate) mod request;
pub(crate) mod stream;

pub(crate) use context_window::{
    build_context_window, build_related_history_index, context_blocks_from_entries,
    format_context_prompt, format_context_prompt_with_access, format_context_prompt_with_policy,
    provider_safe_command_fact_line, provider_safe_command_facts, redact_provider_command_text,
    terminal_output_id, ContextEntry, ContextWindowConfig, ProviderCommandFacts,
    RelatedHistoryConfig, ShellEvidenceAccess,
};
pub(crate) use model::{evidence_capture_status_for_block, EvidenceCaptureStatus};
pub(crate) use output_policy::output_excerpt_status_for_block;
pub(crate) use output_text::{
    clean_terminal_control_sequences, redact_sensitive_output, ControlSequenceScanner,
};
pub(crate) use redaction::{redact_sensitive_text, truncate_provider_authored_command_text};
