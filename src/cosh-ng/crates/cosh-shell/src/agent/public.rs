#[path = "display.rs"]
mod display;

#[path = "composer.rs"]
// The library facade only renders metadata; the binary owns capture and validation.
#[allow(dead_code)]
pub(crate) mod composer;

#[path = "governance.rs"]
mod governance;

#[cfg(test)]
#[path = "governance_tests.rs"]
mod governance_tests;

pub use governance::{govern_agent_events, govern_agent_events_with_language, GovernanceOutput};
