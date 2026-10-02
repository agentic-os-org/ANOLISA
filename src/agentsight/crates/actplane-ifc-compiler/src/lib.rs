// SPDX-License-Identifier: MIT
// Copyright (c) 2026 eunomia-bpf org.

//! ActPlane IFC policy compiler.
//!
//! This crate parses the ActPlane policy language and lowers it to the fixed
//! kernel ABI blob consumed by the eBPF enforcement engine. It does not load
//! eBPF programs, manage runtime domains, or provide CLI behavior.

pub mod dsl;

pub use dsl::{
    Compiled, RuleMeta, RuleSourceMeta, ast, compile, compile_str, compile_str_with_labels, lower,
    parse,
};

/// AgentSight ABI guard: compiled CConfig blob size, cross-checked against
/// ebpf-ifc-engine::EXPECTED_CONFIG_BLOB_SIZE in agentsight-enforcer.
pub use dsl::lower::COMPILED_CONFIG_BLOB_SIZE;
