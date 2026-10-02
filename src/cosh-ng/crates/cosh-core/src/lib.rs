//! cosh-core library re-exports for examples and tests.

pub mod provider;
// The library target only exposes provider constructors; the binary consumes the
// remaining redaction boundaries for hooks and sessions.
#[allow(dead_code)]
mod redaction;
