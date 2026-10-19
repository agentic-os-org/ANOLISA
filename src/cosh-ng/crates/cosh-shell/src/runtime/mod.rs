pub(crate) mod approval_ledger;
pub(crate) mod approval_state;
pub(crate) mod cancel;
pub(crate) mod cli_args;
pub(crate) mod continuity;
pub(crate) mod controller;
pub(crate) mod details;
pub(crate) mod dispatcher;
#[cfg(test)]
mod dispatcher_tests;
pub(crate) mod doctor;
pub(crate) mod events;
pub(crate) mod evidence_delivery;
pub(crate) mod evidence_requests;
#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod evidence_requests_tests;
pub(crate) mod evidence_state;
pub(crate) mod hooks;
pub(crate) mod insight;
pub(crate) mod invocation;
pub(crate) mod logging;
pub(crate) mod mode;
#[cfg(test)]
mod mode_tests;
#[cfg(test)]
mod mvp_loop_tests;
pub(crate) mod prelude;
pub(crate) mod prompt_draft;
pub(crate) mod provider_cancellation_artifacts;
pub(crate) mod provider_tool_state;
pub(crate) mod question_terminal;
pub(crate) mod shell_evidence;
pub(crate) mod shell_handoff_state;
pub(crate) mod startup;
pub(crate) mod state;
mod state_prelude;
#[cfg(test)]
mod state_tests;
pub(crate) mod terminal;
pub(crate) mod trust_state;
