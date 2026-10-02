#[allow(dead_code)]
#[path = "mod.rs"]
mod implementation;

pub use implementation::read_shell_events;

pub(crate) use implementation::{redacted_shell_events, write_shell_events, ShellEventJournal};
