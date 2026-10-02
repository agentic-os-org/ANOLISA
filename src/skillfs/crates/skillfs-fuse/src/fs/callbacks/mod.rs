//! FUSE callback bodies, grouped by semantic operation.
//!
//! Per the refactor contract, `impl Filesystem for SkillFs` itself stays
//! in `fs.rs` as a single block — Rust does not allow splitting a
//! trait impl across files. Each trait method there is a thin wrapper
//! that delegates to a `pub(in crate::fs) fn <name>_impl` inherent method
//! defined in one of the files in this directory.

mod dir;
mod link;
mod meta;
mod mutate;
mod read;
mod write;
mod xattr;
