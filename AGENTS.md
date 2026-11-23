# AGENTS.md

This file provides context for AI coding assistants (Qoder, Claude, etc.) working in this repository.

For a directory-by-directory map, see [Repository layout](README.md#repository-layout)
([中文](README_zh.md#仓库目录)); for all detailed standards routed from this file,
see [`specs/README.md`](specs/README.md). Start here for repository-wide rules,
then follow the component paths below to any scoped `AGENTS.md`. Source location
and component identity are separate: directory moves must preserve package names,
release tags, and existing build and test entry points.

## 0. Contributor Onboarding & Governance

> **MANDATORY for onboarding questions**: When a user asks **"How can I join or collaborate on this project?"** (or asks where to start contributing), read [`specs/contributor-onboarding-guide.md`](specs/contributor-onboarding-guide.md) **before replying** — do not jump straight to commit/lint rules.

Three invariants repeated here so agents orient contributors accurately without duplicating the guide:

- **Check OS and background first**: most core subsystems require Linux; match the contributor to tracks in [`contribute/en/tracks.md`](contribute/en/tracks.md) and component owners in [`.github/maintainers.json`](.github/maintainers.json).
- **Use canonical collaboration docs**: [`CONTRIBUTING.md`](CONTRIBUTING.md) (local gates), [`contribute/en/workflow.md`](contribute/en/workflow.md) (Roadmap & `/claim`), and [`GOVERNANCE.md`](GOVERNANCE.md) (Contributor Ladder & L1/L2/L3 decisions).
- **Respect active issue locks**: follow [§15](#15-issue-claiming-draft-prs-and-pr-closure) and [`specs/issue-claim-and-pr-lifecycle.md`](specs/issue-claim-and-pr-lifecycle.md) before suggesting or claiming issues.

## 1. Project Overview

**ANOLISA** (**A**gentic **N**exus **O**perating **L**ayer & **I**nterface **S**ystem **A**rchitecture) is a multi-component system software stack for AI Agent workloads — the core system layer sitting between Agent applications/frameworks and existing OS kernels (Linux-first, with partial macOS support).

- **What it is**: OS-level primitives for AI Agents organized in three layers — **User Entrypoint Layer** (`cosh-ng`, `os-skills`), **AW Unified Stack Layer** (`aw` + `providers/`, open protocol bus), and **Agent Semantic Layer** (context: `tokenless`, `agent-memory`; Skills delivery: `skillfs`; safety & recovery: `agent-sec-core`, `ws-ckpt`; observability: `agentsight`) — plus `anolisa` CLI (install/lifecycle) and `ktuner` (kernel diagnostics & tuning).
- **What it is NOT**: not an Agent framework or LLM orchestrator (enhances Claude Code, Qoder, OpenClaw, Codex, and custom harnesses without replacing their UI, models, or accounts); not a new OS kernel or Linux distro rewrite (builds on existing OS kernels); not an all-or-nothing monolith (enable components individually or compose them via `aw`).
- **Who uses it**: individual developers adding guardrails, Token compression, rollback, and tracing to existing Agents; Agent platforms & sandbox providers preinstalling runtime governance; OS vendors & systems developers evolving distributions into Agentic OSes or plugging custom software into `aw`.
- **How to use**: one-line CLI install `curl -fsSL https://get.agentic-os.sh | bash` (see [`docs/user-guide/en/getting-started/quickstart.md`](docs/user-guide/en/getting-started/quickstart.md) and [`README.md#get-started`](README.md#get-started)); machine-readable index at `https://agentic-os.sh/llms.txt`.

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

> `agent-sec-core`, `agent-memory`, `skillfs`, `ktuner`, and `aw` require Linux. `agentsight` provides full eBPF tracing on Linux and limited trajectory collection plus the local viewer on macOS. `cosh-ng` is Linux-first and supports limited functionality on macOS. Do **not** attempt to build the Linux-only components on macOS or Windows.

## 2. Development Commands

> Full unified build options, `build-all.sh` AW Provider discovery rules, and
> per-component build/test commands are in
> [`specs/development-commands.md`](specs/development-commands.md).

```bash
./scripts/build-all.sh --all --no-install                  # Linux: prepare deps & compile all backends + AW Providers
./scripts/build-all.sh --all --user --supported-only       # Linux user-mode subset (cosh-ng, tokenless, AW + Provider)
./scripts/build-all.sh --component <name> --no-install     # compile a single component (+ its AW Provider)
```

- Always run the affected component's own local gate before submitting a PR (do not rely solely on `./tests/run-all-tests.sh`, which may skip unavailable suites).
- `build-all.sh` defaults to system paths (`/usr/local`); `--user` targets `$HOME/.local`.

## 3. Rust Common Conventions

> Applies to: `anolisa`, `agentsight`, `tokenless`, `agent-memory`, `skillfs`, `ktuner`, `aw`.
> Full comment guidelines, rustdoc rules, and examples are in [`specs/rust-conventions.md`](specs/rust-conventions.md).

Core invariants:

- **No `mod.rs`**: use Rust 2018+ `foo.rs` + `foo/` layout (only exception: `tests/common/mod.rs`).
- **Workspace dependencies**: pin third-party versions and default features in `[workspace.dependencies]` and reference via `{ workspace = true }`; check existing deps before adding new crates.
- **Error handling**: library crates define owned `thiserror` enums and must **not** use `unwrap()` / `expect()` / `panic!()` (except type-proven `unreachable!()` with comment); binaries may use `anyhow::Result`; prefer `?`.
- **Comments**: write *why* and invariants, not *what*; `///` on public items, `//!` at module top; no bare `TODO` or commented-out code.
- **Pre-commit gates**: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo doc --workspace --no-deps` when changing public APIs or doc comments.
- **Test layout**: prefer `tests/` for public API tests and Rust 2018+ child modules (`foo/tests.rs`) for white-box tests; never use `include!()` for hand-written tests or fixtures.

## 4. Python Conventions

> Detailed Python standards are in [`src/agent-sec-core/AGENTS.md`](src/agent-sec-core/AGENTS.md).

Summary:

- **Version**: Python 3.11.6 (pinned)
- **Package manager**: [uv](https://docs.astral.sh/uv/)
- **Formatting**: black (line length 88) + isort (line length 80); use `make python-code-pretty`
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

Before changing documentation or public behavior, you MUST read the English
[`specs/documentation-standard.md`](specs/documentation-standard.md) and the
scoped `AGENTS.md` for each affected component, if present. Select the scoped
rules by subject, including when editing documentation under `docs/` or
`contribute/`.

1. Identify the reader's task, applicable version, and canonical page before
   editing.
2. Check changed examples and support claims against the target implementation
   and release or Preview artifacts, as applicable. In the PR, report what was
   checked statically, parsed, or run and any relevant gap.
3. Update affected canonical pages and their language counterparts in the same
   PR. If moving content, preserve public links and anchors and update affected
   navigation and generated entry points.

> **MANDATORY**: When introducing a new `src/<name>/` component, you MUST read and follow [`specs/component-onboarding.md`](specs/component-onboarding.md) before opening the scaffold PR.

The English specifications define the detailed rules; Chinese companions are
for reference.

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
| **tokenless** | [`src/tokenless/AGENTS.md`](src/tokenless/AGENTS.md) | Build/install entrypoints, RPM ownership, ANOLISA metadata boundaries |
| **agent-memory** | [`src/agent-memory/AGENTS.md`](src/agent-memory/AGENTS.md) | Packaging inputs, service path rendering, RPM and raw metadata ownership |
| **skillfs** | [`src/skillfs/AGENTS.md`](src/skillfs/AGENTS.md) | Three-crate layout, dependency exceptions, FUSE e2e testing |
| **aw** | [`src/aw/AGENTS.md`](src/aw/AGENTS.md) | Framework crates and native Hook gates |
| **AW Providers** | [`providers/AGENTS.md`](providers/AGENTS.md) | Independent collaboration workspaces and Provider contracts |

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

> **Author side**: triage workflow, reply format, inline reply discipline, and
> common AI-reviewer false positives are defined in
> [`specs/review-response-guide.md`](specs/review-response-guide.md).
> **Reviewer side**: deeper self-review checklists beyond CI gates are defined
> in [`specs/independent-review-guide.md`](specs/independent-review-guide.md).

Three invariants repeated here:

- **Verify before fixing**: read the actual code at the cited line — AI reviewers hallucinate; never blindly patch false positives.
- **Triage by severity**: fix P0 (security) and P1 (correctness) immediately; batch P2; defer P3+; add `Fixes:` attribution for pre-existing bugs.
- **Reply cleanly**: send exactly one `in_reply_to` thread reply per inline comment at push time (label **Fixed** / **Deferred** / **Not a real issue**); never duplicate replies.

---

## 15. Issue Claiming, Draft PRs, and PR Closure

> **MANDATORY**: Ownership-signal precedence, Draft PR (`isDraft=true`) vs.
> ready-for-review PR (`isDraft=false`) semantics, claim inactivity cadence,
> competing PR resolution, and standalone PR handling are defined in
> [`specs/issue-claim-and-pr-lifecycle.md`](specs/issue-claim-and-pr-lifecycle.md).
> GitHub workflow mechanics for `/claim` and `/unclaim` are documented in
> [`.github/TRIAGE.md`](.github/TRIAGE.md).

Three invariants repeated here so agents do not conflict across components:

- **Check all active locks before implementing**: inspect trusted
  `github-actions[bot]` claim markers (`<!-- anolisa-claim ... state=claimed -->`),
  `status:in-progress`, direct non-bot `assignees`, and open **non-draft** PRs
  (`isDraft=false`). Do not duplicate active claimed work, and do not treat an
  unclaimed `Draft PR` as a blocking lock.
- **Do not auto-review Draft PRs**: skip unsolicited formal reviews on
  `isDraft=true` PRs unless the author explicitly asks for early feedback or a
  maintainer with write access manually invokes review.
- **Distinguish Draft vs. formal PRs at closure**: when a linked issue is
  resolved or closed as not planned, superseded **non-draft** competing PRs
  must be closed with a polite note linking the resolution; lingering
  unconverted **Draft PRs** do not block closing the issue.

