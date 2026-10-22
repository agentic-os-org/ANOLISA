//! Python-backed package terminal integration; excluded from Python-free packaging.

#[path = "packages/support.rs"]
mod support;
#[path = "packages_pty/terminal.rs"]
mod terminal;
