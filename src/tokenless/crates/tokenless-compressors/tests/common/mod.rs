//! Shared independent oracles and lossless verification helpers for BuildLog integration tests.

// Each integration test binary in `tests/*.rs` exercises only its own format's oracle.
#![allow(dead_code)]

pub mod build_log_harness;
pub mod ctest_oracle;
pub mod gradle_oracle;
pub mod maven_oracle;
