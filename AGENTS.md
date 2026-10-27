# AGENTS.md

This file provides context for AI coding assistants (Qoder, Claude, etc.) working in this repository.

For a directory-by-directory map, see [Repository layout](README.md#repository-layout)
([中文](README_zh.md#仓库目录)). Start here for contribution rules, then follow the
component paths below to any scoped `AGENTS.md`. Source location and component
identity are separate: directory moves must preserve package names, release tags,
and existing build and test entry points.

## 1. Project Overview

**ANOLISA** is a monorepo for an Agentic OS — a server-side operating layer designed for AI agent workloads.

| Component | Path | Tech | Platform |
|-----------|------|------|----------|
| **cosh-ng** | `src/cosh-ng/` | Rust | Linux (full); macOS (limited functionality) |
| **agent-sec-core** | `src/agent-sec-core/` | Rust + Python | Linux only |
| **agentsight** | `src/agentsight/` | Rust (eBPF) | Linux (full); macOS (trajectory/serve only) |
| **tokenless** | `src/tokenless/` | Rust | Linux (full); macOS x64/arm64 (CLI binaries + adapters, via npm); Windows x64 (native CLI + QwenWork adapter) |
| **agent-memory** (`memory`) | `src/agent-memory/` | Rust | Linux only |
| **os-skills** | `src/os-skills/` | Python / Shell | All |
| **anolisa** | `distribution/anolisa/` | Rust | Linux + macOS (arm64) |
| **SkillFS** (`skillfs`) | `src/skillfs/` | Rust / FUSE | Linux only |
| **ws-ckpt** | `src/ws-ckpt/` | Rust + TypeScript | Linux only |
| **ktuner** | `src/ktuner/` | Rust | Linux only |
| **aw** | `src/aw/` | Rust | Linux only |

> `agent-sec-core`, `agent-memory`, `skillfs`, `ktuner`, and `aw` require Linux. `agentsight` provides full eBPF tracing on Linux and limited trajectory collection plus the local viewer on macOS. `cosh-ng` is Linux-first and supports limited functionality on macOS. Do **not** attempt to build the Linux-only components on macOS or Windows. (tokenless ships macOS CLI binaries and framework adapters via npm, but the binaries are cross-compiled **from Linux** — building tokenless on macOS is still unsupported.)

## 2. Development Commands

```bash
# Unified build (Linux — select maintained components explicitly)
./scripts/build-all.sh --component sec-core --component tokenless  # deps, build, and user install
./scripts/build-all.sh --component sec-core --no-install           # install deps and build, skip component install
./scripts/build-all.sh --component sec-core --ignore-deps          # pre-provisioned hosts: skip dependency setup and runtime verification

# Partial convenience test suites (may skip unavailable suites)
./tests/run-all-tests.sh --filter sec     # agent-sec-core only
./tests/run-all-tests.sh --filter sight   # agentsight only
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory

# cosh-ng (Linux full; macOS limited functionality, per-component)
cd src/cosh-ng
cargo build --workspace
cargo fmt --all -- --check
# Select the closest targeted test from src/cosh-ng/CONTRIBUTING.md.
# Full gates require a large/cross-cutting change and an explicit request.

# agent-sec-core (Linux only; Python 3.11.6 + uv, per-component)
cd src/agent-sec-core
make build-all
uv run --project agent-sec-cli python --version  # must report 3.11.6
make test         # Python + Rust sandbox + OpenClaw plugin tests

# agentsight (Linux full eBPF; macOS trajectory/serve only, per-component)
cd src/agentsight
# Linux
make build-all
make lint
make test

# macOS
make build-mac

# os-skills
cd src/os-skills   # Skill definitions are static assets, no compilation needed

# tokenless (per-component)
cd src/tokenless
make build       # tokenless + RTK + OpenClaw plugin
make lint
make test        # Rust + hooks + integration + adapters

# agent-memory (Linux only, per-component)
cd src/agent-memory
make build       # cargo build --release --locked
make fmt-check
make lint
make test        # cargo test --locked
make smoke       # end-to-end MCP stdio smoke test

# anolisa (per-component)
cd distribution/anolisa
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked

# ws-ckpt (Linux only, per-component)
cd src/ws-ckpt
make build       # cargo build --release + openclaw plugin
make test        # cargo test --workspace

# SkillFS (Linux only, per-component)
cd src/skillfs
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/test.sh   # FUSE smoke test; skips itself if fuse3 or /dev/fuse is unavailable

# ktuner (Linux only, per-component)
cd src/ktuner
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test

# aw (Linux only, per-component; toolchain pinned by src/aw/rust-toolchain.toml)
python3 src/aw/scripts/check.py   # from the repository root: CI behavior tests,
                                  # fmt, clippy, locked tests, digest vectors, rustdoc
```

## 3. Rust Common Conventions

> Applies to these Rust components: `anolisa`, `agentsight`, `tokenless`, `agent-memory`, `skillfs`, `ktuner`, `aw`.

### 3.1 Comment Guidelines

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

### 3.2 Module Organization: no `mod.rs`

Use the Rust 2018+ recommended layout: parent modules are `.rs` files with matching directories for child modules. Never create a `mod.rs`; flag any encountered during code review.

Rationale: avoids identically-named `mod.rs` files; makes editor tabs more readable; aligns with `rustfmt` and `cargo new` defaults.

**Exception**: `tests/common/mod.rs` — cargo's official convention for sharing helpers across integration tests.

### 3.3 Dependency Management

- All third-party dependencies declare their version in `[workspace.dependencies]`; crates reference them via `dep_name = { workspace = true }` — never pin versions in sub-crates.
- Before adding a dependency, grep `Cargo.toml` to check whether an equivalent crate already exists (e.g. do not add `simd-json` when `serde_json` is already present).
- Do not bump a declared dependency's major version without discussion.
- Feature flags are enabled centrally in the workspace declaration; sub-crates should not repeat `features = [...]` unless genuinely extending them.

### 3.4 Error Handling

- **Library crates**: define named `enum` error types with `thiserror`. Each crate owns its error enum and wraps upstream errors via `#[from]` — do not reuse error enums across crate boundaries.
- **Binaries**: may use `anyhow::Result` for ergonomic error propagation.
- Library code must **not** use `unwrap()` / `expect()` / `panic!()` unless a comment proves the condition is guaranteed unreachable by the type system (prefer `unreachable!()` with an explanation).
- Error messages target developers: include failure context and relevant variable values; avoid "something went wrong" style messages.
- Prefer `?` propagation; do not rewrite `?`-eligible code with `match` + immediate `return Err(...)`.

### 3.5 Pre-commit Checks

Every Rust component must pass these before committing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps   # required when changing public API or doc comments
```

- Clippy warnings are denied by default. To allow a specific lint, use `#[allow(clippy::xxx)]` at the narrowest scope with a comment explaining why.
- Never comment out tests or remove assertions to pass checks — find and fix the root cause.

## 4. Python Conventions

> Detailed Python standards are in [`src/agent-sec-core/AGENTS.md`](src/agent-sec-core/AGENTS.md).

Summary:

- **Version**: Python 3.11.6 (pinned)
- **Package manager**: [uv](https://docs.astral.sh/uv/)
- **Formatting**: black + isort (`line-length = 100`)
- **Linting**: [ruff](https://docs.astral.sh/ruff/) (F, E, W, I, TID252, ANN, S-subset, etc.)
- **Type annotations**: required on all function parameters and return types
- **Imports**: absolute only (`from agent_sec_cli.xxx import yyy`); no relative imports
- **Testing**: pytest; tests live in `tests/` not inside package directories

## 5. TypeScript Conventions

Follow each active package's `package.json`, `tsconfig.json`, and local lint
configuration. TypeScript is used by component plugins, adapters, and `website/`;
their build and test entry points differ. Use the component's documented commands
and its declared package scripts rather than assuming repository-wide npm targets.

## 6. Commit Message Rules

> **MANDATORY**: All commit message rules — subject format, body requirements,
> trailers, scope list and inference, atomicity — are defined in
> [`specs/commit-message-standard.md`](specs/commit-message-standard.md).
> You MUST read that file before composing a commit message. Non-compliance
> will be rejected in review.

Two invariants repeated here because they fail most often: **scope is
mandatory** (CI errors when it is missing), and `Signed-off-by` MUST use the
contributor's own Git identity. This section intentionally does not duplicate
the spec.

## 7. Branch Naming

> Recommended convention — not enforced for fork contributors.

```
feature/<scope>/<short-desc>    e.g. feature/cosh/json-output
fix/<scope>/<short-desc>        e.g. fix/sec-core/sandbox-escape
hotfix/<scope>/<short-desc>     e.g. hotfix/skill/broken-load
release/<scope>/vX.Y            e.g. release/cosh/v2.1
```

## 8. PR Description

Use [`.github/pull_request_template.md`](.github/pull_request_template.md) as the base template. Key rules:

- **Description**: 2–5 sentences — what changed, why, key implementation decision
- **Related Issue**: `closes #<n>` or `no-issue: <reason>`
- **Risk and compatibility**: explain checked public, privileged, contract, or migration risks
- **Validation**: commands, environment, scope (unit/integration/manual), and edge cases
- **Documentation and rollback**: record documentation updates and rollback guidance
- PR title follows commit message format: `type(scope): description`

## 9. Documentation Rules

> **MANDATORY**: All documentation rules — file naming, bilingual conventions, CHANGELOG format, file placement, user-guide standards — are defined in [`specs/documentation-standard.md`](specs/documentation-standard.md). You MUST read that file before creating, renaming, or modifying any documentation file. Non-compliance will be rejected in review.

> **MANDATORY**: When introducing a new `src/<name>/` component, you MUST read and follow [`specs/component-onboarding.md`](specs/component-onboarding.md) before opening the scaffold PR.

This section intentionally does not duplicate the spec. Do NOT invent documentation rules from memory or prior context — the spec is the single source of truth.

## 10. Code Standards (General)

- All code and comments must be in **English**
- Do not hide errors or risks — make them visible and actionable
- Every change should not only implement the desired functionality but also improve codebase quality

## 11. Scoped Module Rules

Components with complex architectures maintain their own AGENTS.md for module-specific conventions. **Read the relevant scoped file before contributing to that component.**

| Component | Scoped Rules | Focus |
|-----------|-------------|-------|
| **agentsight** | [`src/agentsight/AGENTS.md`](src/agentsight/AGENTS.md) | eBPF probes, data pipeline architecture, module map, FFI constraints, API endpoints |
| **agent-sec-core** | [`src/agent-sec-core/AGENTS.md`](src/agent-sec-core/AGENTS.md) | Python environment, ruff/black rules, hermes-plugin, capability system |
| **anolisa** | [`distribution/anolisa/AGENTS.md`](distribution/anolisa/AGENTS.md) | Workspace structure, crate responsibilities |
| **cosh-ng** | [`src/cosh-ng/AGENTS.md`](src/cosh-ng/AGENTS.md) | 5-crate workspace, security heuristics, PTY testing strategy |
| **skillfs** | [`src/skillfs/AGENTS.md`](src/skillfs/AGENTS.md) | Three-crate layout, dependency exceptions, FUSE e2e testing |
| **aw** | [`src/aw/AGENTS.md`](src/aw/AGENTS.md) | Framework crates and native Hook gates |
| **AW Providers** | [`providers/AGENTS.md`](providers/AGENTS.md) | Independent collaboration workspaces and Provider contracts |

AW documentation under `docs/` and `contribute/` also follows the audience and
writing rules in [`src/aw/AGENTS.md`](src/aw/AGENTS.md#documentation-audiences-and-writing).

## 11.1 File Placement & Documentation Structure

> **MANDATORY**: See [`specs/documentation-standard.md`](specs/documentation-standard.md) §2–§4 for the complete file placement rules, bilingual naming convention, and component-level file requirements. Do NOT rely on cached or memorized rules — read the spec file directly.

## 12. User Guide Documentation Standards

> **MANDATORY**: See [`specs/documentation-standard.md`](specs/documentation-standard.md) §4.6 for user-guide writing standards including installation priority, content boundaries, framing principles, and bilingual language rules. You MUST comply — skipping this spec and writing docs from assumptions is a blocking review issue.

---

## 13. Commit Discipline

> **MANDATORY**: Rebase preparation, fix attribution (`Fixes:` / `Supplements:` /
> `git commit --fixup`), the version bump rule, CHANGELOG timing, and the
> pre-commit format check are defined in
> [`specs/commit-message-standard.md`](specs/commit-message-standard.md) §6–§8.

One rule repeated here because it is broken most often: **never** create a
standalone "fix" commit for something introduced earlier in the same PR
branch — amend or fixup it into the originating commit. This section
intentionally does not duplicate the spec.

---

## 14. Responding to Review

> **Recommended**: Reviewer-side methodology — deeper self-review checklists beyond CI gates — is defined in [`specs/independent-review-guide.md`](specs/independent-review-guide.md). This section covers the author side: triaging and responding to findings.

### Triage before acting

AI reviewers (qoderai, qoder) generate findings automatically. Before fixing:

1. **Verify the claim**: Read the actual code at the cited line. AI reviewers hallucinate — they may reference wrong line numbers, misread logic, or flag non-issues.
2. **Assess severity**: Only P0 (security) and P1 (correctness) warrant immediate code changes. P2 (style/docs) can be batched. P3+ can be deferred.
3. **Check if pre-existing**: If the issue existed before this PR, it needs `Fixes:` attribution to the original commit. If it's a design limitation (not a bug), reply with rationale rather than patching.

### Response format

When replying to review findings:

- **Fixed**: State what was done and which commit it landed in.
- **Deferred**: Explain why it's out of scope for this PR and what the plan is.
- **Not a real issue**: Explain why the finding is incorrect or not applicable. Cite specific code/design rationale.

Never blindly accept and fix every AI finding. Some are false positives, some are architectural suggestions that require design discussion.

### Inline reply discipline

- Every inline review comment MUST receive exactly one thread reply via `in_reply_to`.
- Reply at the same time as the code fix push — never push silently.
- For stale comments resolved by a newer revision: reply "Resolved in latest revision" (one sentence).
- Never duplicate replies on already-answered threads.
- Before replying, query the API to identify which comments lack a response — avoid both omissions and repetition.

### Common false positive patterns

- "X is not tested" when X has a default impl that returns Err (intentional no-op)
- "Missing error handling" when the `?` operator already propagates
- "Config field unused" when it's reserved for a future phase
- "Version mismatch" referencing stale file contents from a previous push
