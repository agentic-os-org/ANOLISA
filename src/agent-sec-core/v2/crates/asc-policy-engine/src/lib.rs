//! Product Policy-template compiler for the frozen Canonical Policy IR profile.
//!
//! The first product batch intentionally compiles only
//! `prevent_file_deletion`. Adding another authored template kind changes the
//! compiler contract and requires a new golden fixture plus direct Adapter
//! conformance evidence.

#![forbid(unsafe_code)]

mod template_compiler;

pub use template_compiler::PolicyTemplateCompiler;
