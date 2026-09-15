# AGENTS.md

This file provides guidance to AI coding assistants when working with code in this repository.

## Project Overview

cosh-ng (Computable Operating System Harness) is a deterministic Agent-OS interface. It provides a `cosh-cli` binary for structured JSON output:
- **CLI mode** (`cosh-cli <subsystem> <action>`): structured JSON output for AI Agents

## Build & Test Commands

```bash
cargo build --workspace          # Build all crates
cargo test --workspace           # Run all tests (unit + integration)
cargo test --package cosh-cli --test cli_integration   # Integration tests only
cargo test --package cosh-platform   # Platform crate unit tests only
cargo test --package cosh-types      # Types crate unit tests only
```

### cosh-shell Testing Strategy

cosh-shell 的 PTY 集成测试较慢（每个 spawn 子进程）。开发时使用分层策略，避免跑全量：

```bash
# 开发时：只跑单元测试（0.1s）
cargo test --package cosh-shell --lib

# 验证逻辑：跑 logic target
cargo test --package cosh-shell --test logic

# 验证协议：跑 protocol target
cargo test --package cosh-shell --test protocol

# 验证单个集成测试（0.5-2s）
cargo test --package cosh-shell --test raw_cli <test_name> -- --exact

# 验证 shell host 改动（用并行加速）
cargo test --package cosh-shell --test shell_host -- --test-threads=4

# 阶段验收才跑全量（并行）
cargo test --package cosh-shell -- --test-threads=4
```

cosh-shell 测试布局规则：

- `src/` 只放 private 纯逻辑或轻量 component tests。
- public API 多模块逻辑测试进入 logic layer，目标 target 是 `logic`。
- adapter/control protocol 测试进入 protocol layer，目标 target 是 `protocol`。
- spawn `cosh-shell` binary、scripted raw shell、approval/question card、provider handoff 进入 `raw_cli` layer。
- PTY shell host、OSC、termios、foreground/native shell 行为进入 `shell_host` layer。
- 真实 provider、manual TTY、视觉/体验验证不混入默认 cargo test gate。

布局审计入口：

```bash
crates/cosh-shell/scripts/check-layout.sh
```

该脚本必须保持通过；新增或迁移代码不能增加新的 violation group。脚本中的 registered debt 只表示迁移债务被 inventory 追踪，不代表最终验收已完成。

Prerequisites: Linux (or macOS for limited functionality), Rust 1.88+. pkg/svc commands need root/sudo. Checkpoint commands need a running ws-ckpt daemon.

## Architecture

8-crate workspace. Dependency direction: `cosh-cli` / `cosh-core` → `cosh-platform` → `cosh-types`; `cosh-shell` is standalone (no internal crate deps). `cosh-gateway` depends only on the side-effect-free `cosh-gateway-contracts` leaf among internal crates. `cosh-gateway-app` is the installed composition root and may connect Gateway policy/storage to platform side-effect adapters.

- **cosh-types**: Pure types, zero side effects. Defines `CoshResponse<T>` envelope, `CoshError` (with error codes, recoverable flag, hint), and ws-ckpt IPC protocol types.
- **cosh-platform**: Platform abstraction layer. Distro detection from `/etc/os-release`, package manager routing (dnf/apt/zypper/brew), systemd service adapter, ws-ckpt daemon Unix socket IPC client.
- **cosh-cli**: CLI entry point (binary: `cosh-cli`). 4 command domains: `pkg`, `svc`, `checkpoint`, `audit`. All output is JSON via `CoshResponse<T>`. Uses clap derive for argument parsing.
- **cosh-core**: Unified agent core (binary: `cosh-core`). Headless JSONL backend + LLM provider integration (OpenAI-compat, SysOM/Aliyun). Includes hooks, tools, skills, extensions, and config management. Interactive TUI mode is declared but not yet implemented.
- **cosh-shell**: AI-augmented interactive shell (binary: `cosh-shell`). PTY wrapper over bash/zsh with OSC marker-based command boundary detection, streaming AI analysis (Claude/Qwen adapters), inline card rendering (ratatui), tool approval control protocol.
- **cosh-gateway-contracts**: Side-effect-free Gateway Task, Runtime, Capability, identity, and error contracts. It must not own storage, processes, transports, providers, or OS execution.
- **cosh-gateway**: Gateway control-plane library: authenticated local Unix daemon/client, durable Task and governance storage, Runtime scheduling and supervision, restart reconciliation, private core transport, ACP v1 codec/bridge, and local once-only permission evidence.
- **cosh-gateway-app**: Installed `cosh-gateway` binary and composition root. It owns CLI/serve wiring and concrete governed OS adapters so the Gateway library retains its contracts-only internal dependency boundary. See the [composition design note](docs/design/gateway-app-composition.md) for callers, API/test boundaries, alternatives, and revisit conditions.

### cosh-shell Code Organization

每个 Issue 或任务只允许修改其 triage、已批准 design 或执行 spec 明确授权的文件；不得顺手扩大范围。
涉及 `crates/cosh-shell/` 的修改仍须遵守下列组织、owner 与布局约束。

长期 owner 约定：

- UI owner 使用 `ui/`；`agent_render/` 只允许作为短期兼容 facade。
- Hook owner 使用 `hooks/`；`hook_engine/` 合并入 `hooks/`。
- Linux memory hook 收敛到 `hooks/linux_memory/`。
- `context_window` -> `evidence/context_window.rs`。
- `exit_classify` -> `command/exit_classify.rs`。
- `governance` -> `agent/governance.rs`。
- `interactive` -> `shell_host/line_interactive.rs`。
- `hook_types` 拆分到 `types/hooks.rs` 和 `hooks/model.rs`。

新增 cosh-shell 代码时：

- 不新增 root `crates/cosh-shell/src/*.rs` implementation 文件。
- 不新增未登记的 `lib.rs pub mod` 或 root re-export。
- `src/` production code 不新增 `cosh_shell::...` self-crate public path；使用 `crate::...` 或 owner module path。
- 不向超过 1000 行的 production 文件追加新功能；超过 700 行的 production 文件需要 owner note、拆分计划或 waiver。
- `hooks` 不直接拥有 agent 启动或 runtime state mutation；通过 runtime command/event 边界交接。

## Key Design Constraints

- **ws-ckpt IPC wire format**: Uses bincode with 4-byte LE length prefix framing. Enum variant order in `WsCkptRequest`/`WsCkptResponse`/`WsCkptErrorCode` is the binary wire contract — **never reorder variants** without coordinating with the ws-ckpt daemon.
- **Unified JSON envelope**: Every CLI command returns `CoshResponse<T>` with `ok`, `data`/`error`, and `meta` fields. Exit code 0 = success, 1 = failure.
- **Cross-distro routing**: `Distro::detect()` reads `/etc/os-release` and routes to the correct package manager. Adding a new distro means adding a variant to the `Distro` enum in `cosh-platform/src/detect.rs` and updating the `pkg_manager()` method.
- **CLI helpers**: `print_success()`, `print_failure()`, `build_meta()` in `cosh-cli/src/main.rs` handle all JSON serialization and exit codes — command modules return `i32` exit codes.

## Diagnostic Logging

Log levels default to `info`; the default level chain is `COSH_LOG` > `RUST_LOG` > TOML `[logging] level` > `info`. `tracing` events land in `~/.copilot-shell/logs/cosh-{shell,core}.log.<date>`.

### Info principle

Log only process/session-level lifecycle and state transitions (start/exit/restart, core spawn, recovery, config/auth fallback, provider switch) — events that occur a handful of times per session. Each entry carries the key identifiers (pid, session_id, reason), never the raw prompt. Per-input and per-iteration events (routing decisions, inner-loop turns, stream chunks) never log at info: they belong at debug.

### Warn/error principle

Log once at the choke-point where a subsystem's errors converge — but that single line must carry the full failure origin: format the error chain (`{:?}`), not just Display. If the origin is not recoverable from the choke-point line, fix the error type to carry context instead of adding more log sites. Intermediate layers log at debug. Errors that only flow into the UI event channel must be dual-written to `tracing` — the TUI owns stderr, so the log file is the only durable post-mortem evidence.

## Security Heuristics

When writing safety gates that auto-approve commands, don't pattern-match substrings of the *raw* command — shell metas don't need spaces, and Tab/newline are word separators. Tokenize first (split on whitespace including `\t`/`\n`/`\r`), reject metacharacters anywhere (`;` `|` `&` `>` `<` `$` `` ` `` `(` `)` `{` `}`), then dispatch on tokens. When in doubt, fall through to user approval rather than auto-allow. New regression tests must cover Tab-separated, newline-separated, and unspaced-meta variants. Reference: `crates/cosh-shell/src/tools/readonly_rules/`.

## Debugging Guidelines

- **No host mutation outside isolated environments**: Unless explicitly running inside a container, VM, or other isolated environment, never execute operations that modify host system state (installing/removing packages, changing system config, managing systemd services, etc.).
- **Require a rollback plan before execution**: Before performing any debugging operation with side effects, explicitly list the steps and their corresponding rollback steps. Every operation must be reversible.
- **Roll back all side effects after debugging**: Any system changes produced during debugging (temp files, env vars, service state changes, etc.) must be fully reverted to the original state once debugging is complete.
- **Prefer `--dry-run`**: cosh pkg/svc commands support `--dry-run` — always use it first to verify behavior without actual execution.

## Adding a New CLI Command Domain

1. Create `crates/cosh-cli/src/cmd/<domain>.rs` with a `<Domain>Commands` enum (clap Subcommand) and a `pub fn run(...)` returning `i32`
2. Add the domain to the `Commands` enum in `cosh-cli/src/main.rs`
3. Add return types to `cosh-types/src/`
4. Add platform logic to `cosh-platform/src/`
5. Add integration tests in `crates/cosh-cli/tests/cli_integration.rs`

## Production-Readiness Checklist

Don't trust development reports — verify before merging:

- `cargo test --workspace` — count must match the report.
- `cargo clippy --workspace --all-targets` — `--all-targets` is non-negotiable; the default omits test code, where most lint debt accumulates. "0 warnings" claims without `--all-targets` are misleading.
- `cargo build --workspace --release` — release profile catches optimization-only issues.
- For every "hardened against X" claim, write a PoC that *would have* triggered X and verify it now fails closed. Substring-based safety lists in particular need adversarial review.

## Commit Message Conventions

Strict [Conventional Commits](https://www.conventionalcommits.org/):

- Format: `type(cosh-ng): [crate_scope] imperative subject`.
- `type` is limited to `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, and `chore`. **Do not use `harden:` / `cleanup:`** — map them to a standard type.
- `[crate_scope]` names the affected crate or crates, such as `[core]`, `[shell]`, or `[cli,platform]`.
- The complete subject line is imperative, has no trailing period, and subject 不超过 50 字符；body 每行不超过 100 字符。

## Git History Hygiene

When consolidating many commits via `rebase -i`:

- **`-X theirs` silently drops content** in reorder+squash scenarios. When commit A and a later commit B both touch overlapping regions, `theirs` may keep only one side. Prefer letting conflicts pause the rebase, or verify with `git diff <new> <backup> --stat` afterward.
- When restoring lost content via `edit` + `git commit --amend`, target the **last commit that touches the file** in the new ordering, not the most thematically relevant commit. Earlier amends get re-overwritten by subsequent cherry-picks.
- Fold matching test commits into their parent feat/fix so reviewers see code + tests as one unit.
