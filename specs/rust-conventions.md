# ANOLISA Rust Common Conventions

> Canonical Rust coding, layout, dependency, error-handling, and pre-commit
> rules routed from [`AGENTS.md`](../AGENTS.md) §3.
> Applies to these Rust components: `anolisa`, `agentsight`, `tokenless`,
> `agent-memory`, `skillfs`, `ktuner`, `aw`.

## 1. Comment Guidelines

Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and the official style guide. Write comments that help readers understand intent faster — not comments that paraphrase code.

**Comment types and placement:**

- `//!` **module-level docs**: at the top of a file/module — one or two sentences describing what the module does and when to use it.
- `///` **doc comments**: required on all public (`pub`) items — structs, enums, traits, functions, methods, significant fields, and variants. These appear in `cargo doc`.
- `//` **inline comments**: only where the implementation needs to explain *why* something is done a certain way.
- Do not pile `///` on private, self-explanatory helper functions.

**Write "why", not "what":**

- Type names, field names, and function names already say *what*; comments should explain *why* and document *invariants*.
  - Good: `// Serialize as untagged because most providers omit the type field`
  - Bad: `// This is an enum representing assistant content`
- Document **invariants**, **preconditions**, **side effects**, and **protocol contracts**.
- Never repeat facts already obvious from the signature, type, or naming.

**Brevity first:**

- If one line suffices, do not write two. Trivial setters need no comment or at most a single sentence.
- Avoid polite filler: no "This function returns …". Start with an imperative or noun phrase: "Returns …", "Builds …".
- First line is a standalone summary; expand after a blank line if needed.

**Conventional rustdoc sections** (use when they add value):

- `# Errors` — for functions returning `Result`: list failure conditions.
- `# Panics` — for functions that can panic: list trigger conditions.
- `# Safety` — for `unsafe fn`: state invariants the caller must uphold.
- `# Examples` — typical usage in ```` ```rust ```` blocks, runnable by `cargo test --doc`.

**Prohibited patterns:**

- No bare `TODO` without owner and context.
- No commented-out old code — use git history.
- No timestamps, author names, or changelog-style comments — VCS handles that.
- No "fixes issue #123" in comments — put that in the PR description.
- No restating the type signature in comments.

## 2. Module Organization: no `mod.rs`

Use the Rust 2018+ recommended layout: parent modules are `.rs` files with matching directories for child modules. Never create a `mod.rs`; flag any encountered during code review.

Rationale: avoids identically-named `mod.rs` files; makes editor tabs more readable; aligns with `rustfmt` and `cargo new` defaults.

**Exception**: `tests/common/mod.rs` — cargo's official convention for sharing helpers across integration tests.

## 3. Dependency Management

- All third-party dependencies declare their version in `[workspace.dependencies]`; crates reference them via `dep_name = { workspace = true }` — never pin versions in sub-crates.
- Before adding a dependency, grep `Cargo.toml` to check whether an equivalent crate already exists (e.g. do not add `simd-json` when `serde_json` is already present).
- Do not bump a declared dependency's major version without discussion.
- Feature flags are enabled centrally in the workspace declaration; sub-crates should not repeat `features = [...]` unless genuinely extending them.

## 4. Error Handling

- **Library crates**: define named `enum` error types with `thiserror`. Each crate owns its error enum and wraps upstream errors via `#[from]` — do not reuse error enums across crate boundaries.
- **Binaries**: may use `anyhow::Result` for ergonomic error propagation.
- Library code must **not** use `unwrap()` / `expect()` / `panic!()` unless a comment proves the condition is guaranteed unreachable by the type system (prefer `unreachable!()` with an explanation).
- Error messages target developers: include failure context and relevant variable values; avoid "something went wrong" style messages.
- Prefer `?` propagation; do not rewrite `?`-eligible code with `match` + immediate `return Err(...)`.

## 5. Pre-commit Checks

Every Rust component must pass these before committing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps   # required when changing public API or doc comments
```

- Clippy warnings are denied by default. To allow a specific lint, use `#[allow(clippy::xxx)]` at the narrowest scope with a comment explaining why.
- Never comment out tests or remove assertions to pass checks — find and fix the root cause.

## 6. Test Module Organization

- **Unit vs. integration placement**: prefer integration tests under `tests/` whenever behavior can be verified through the public API alone (alongside runnable `# Examples` doctests for public usage); reserve white-box `#[cfg(test)] mod tests` for private internals and module-scoped invariants.
- **No `include!()` for hand-written tests or fixtures**: reserve `include!()` for build-generated code under `OUT_DIR` (e.g., eBPF skeletons). `cargo fmt --all` does not traverse `include!()`, allowing silent formatting drift past CI and weakening IDE analysis.
- **White-box unit tests**: keep small suites inline in `#[cfg(test)] mod tests { use super::*; ... }`; split larger suites into standard Rust 2018+ child modules (`foo.rs` declares `#[cfg(test)] mod tests;` backed by `foo/tests.rs`, further split into `foo/tests/<topic>.rs`; crate roots `lib.rs` / `main.rs` use `src/tests.rs`). Avoid `#[path]` for ordinary unit tests. `use super::*;` is the permitted glob-import exception inside test submodules, and attaching `foo/tests.rs` does not change `foo.rs`'s role as a leaf implementation module.
- **Shared test helpers by Cargo compilation boundary**:
  - Within one crate's unit tests (`src/`): use `#[cfg(test)] mod test_support;` (or the parent `foo/tests.rs`) rather than chaining multiple `include!()` files in one `mod tests` block.
  - Across integration tests (`tests/*.rs`): use `tests/common/mod.rs` (see §2 exception), `#[path = "support/..."]`, or single-entry integration test modules.
  - Across `src/` unit tests, `tests/` integration tests, or sibling crates (where `#[cfg(test)]` is not visible across Cargo compilation boundaries): gate shared mocks, fakes, or fixtures behind an additive bare-named `test-util` feature (enabled only in `[dev-dependencies]`, or placed in a dev-only helper crate); for small read-only helpers outside `src/` (e.g., standalone test oracles), `#[path = "..."] mod ...;` is also acceptable instead of `include!()`.
