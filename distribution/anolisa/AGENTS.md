# AGENTS.md — anolisa

> Common Rust conventions (comments, module layout, dependency management, error handling, pre-commit checks, commit conventions) are defined in the [root AGENTS.md](../../AGENTS.md#3-rust-common-conventions). This file documents **anolisa-specific** additions only.

## Workspace Structure

The `anolisa` component uses a Cargo workspace. Refer to `Cargo.toml` for the current crate list and their `description` fields for responsibilities.

## Additional Comment Rules

### Invariants and protocol fields

- For serialization/protocol fields (`#[serde(...)]`, provider IDs, signatures, etc.), explain the field's role in the wire protocol and why it must be preserved or echoed.
- When using non-default serde attributes (`skip_serializing_if`, `flatten`, `untagged`, etc.), explain the motivation.

### Verification

- Run `cargo check` and `cargo doc --no-deps` before committing to ensure no broken intra-doc links.
- Public API crates may enable `#![warn(missing_docs)]` at the crate root to enforce coverage.
