//! Python-backed package terminal integration; excluded from Python-free packaging.

#[path = "packages_pty/confirmation.rs"]
mod confirmation;
#[path = "packages/support.rs"]
mod support;
#[path = "packages_pty/terminal.rs"]
mod terminal;
