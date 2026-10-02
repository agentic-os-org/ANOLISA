# Developing cosh-ng

[中文版](../../zh/cosh-ng/getting-started.md)

This guide gets a new contributor from checkout to a focused, validated change.
Read the repository `AGENTS.md`, `src/cosh-ng/AGENTS.md`, and this page before
editing code; those files contain constraints that are intentionally not
duplicated here.

## 1. Prepare the workspace

cosh-ng is a Linux-first Rust workspace that also builds on macOS. The minimum
Rust version is 1.74, and `rust-toolchain.toml` selects stable Rust with rustfmt
and Clippy.

```bash
cd src/cosh-ng
rustup show
cargo build --workspace
```

Do not install packages, change services, or run mutating `cosh-cli` commands on
the development host. Use unit tests, mocks, `--dry-run`, or an explicitly
isolated environment.

## 2. Understand the runtime boundary

There are five crates but three user-facing processes:

| Area | Start reading | Boundary |
|---|---|---|
| Structured OS operations | `crates/cosh-cli/src/main.rs` | Clap to `cosh-platform` to JSON envelope |
| Agent runtime | `crates/cosh-core/src/main.rs` | JSONL/registry input to provider, tools, and session state |
| Interactive terminal | `crates/cosh-shell/src/main.rs` | terminal input, PTY events, cards, and a child cosh-core process |
| Shared platform code | `crates/cosh-platform/src/lib.rs` | distro, package, service, audit, checkpoint adapters |
| Wire and output types | `crates/cosh-types/src/lib.rs` | side-effect-free contracts |

`cosh-shell` does not link to the other workspace crates. It launches
`cosh-core` and communicates over the versioned JSONL/control protocol. That
process boundary is a compatibility contract, not an implementation detail.

See [Architecture](architecture.md) for ownership and data flow.

## 3. Find the owner before editing

For `cosh-shell`, new production behavior belongs under an existing owner
directory; do not add implementation files directly under `src/`.

| Change | Primary owner | Typical test target |
|---|---|---|
| PTY, OSC, bash/zsh integration | `shell_host/` | `shell_host` |
| Input routing and multiline entry | `raw_input/`, `input/`, `slash/` | `raw_cli` or `logic` |
| Agent lifecycle and event policy | `agent/` | `logic` |
| Core adapter/control messages | `adapter/` | `protocol` |
| Approval and question cards | `approval/`, `question/`, `ui/` | `raw_cli` |
| Hooks | `hooks/` | library tests or `logic` |
| Runtime orchestration/state mutation | `runtime/` | library tests, then relevant integration target |
| Agent tools and risk rules | `tools/` | library tests and adversarial regressions |

Run the layout audit after moving or adding shell code:

```bash
crates/cosh-shell/scripts/check-layout.sh
```

## 4. Use the narrowest feedback loop

```bash
# Shared types/platform/CLI
cargo test --locked -p cosh-types
cargo test --locked -p cosh-platform
cargo test --locked -p cosh-cli --test cli_integration

# Core
cargo test --locked -p cosh-core --lib
cargo test --locked -p cosh-core --test jsonl_protocol

# Shell: fast logic before process-heavy tests
cargo test --locked -p cosh-shell --lib
cargo test --locked -p cosh-shell --test logic
cargo test --locked -p cosh-shell --test protocol
```

Choose `raw_cli` when the behavior spawns `cosh-shell`, renders cards, or
crosses the provider handoff. Choose `shell_host` for PTY, OSC, termios,
foreground programs, or native bash/zsh behavior.

## 5. Validate the final change

Match validation to the change:

- Documentation-only changes: check links, Markdown formatting, commands, and
  bilingual parity. Rust tests and builds are unnecessary.
- Ordinary code changes: run formatting and the tests closest to the changed
  crate or behavior. Add targeted Clippy or integration checks when they can
  catch a relevant failure.
- Large or cross-cutting code changes: run full local gates, persistent ECS, or
  manual-grade validation only when the current task explicitly requests that
  depth. Otherwise CI owns broad regression coverage.

When public API or rustdoc changes, also run:

```bash
cargo doc --workspace --no-deps
```

See [Testing](testing.md) for target selection and optional gate profiles.

## 6. Keep contracts explicit

- Every `cosh-cli` result uses `CoshResponse<T>` and a stable exit status.
- Never reorder ws-ckpt protocol enum variants without coordinating the daemon.
- A cosh-core protocol change must update protocol types, both producer and
  consumer, fixtures, and protocol tests together.
- Security allow rules must tokenize first, reject shell metacharacters, and
  fail closed. Add tab, newline, and unspaced-metacharacter regressions.
- Tests must not depend on a real LLM provider or mutate host system state.
- Do not weaken assertions, inventory floors, or registered layout debt to make
  a check pass.

## Where to go next

- [Testing strategy](testing.md)
- [Adding a CLI command](adding-commands.md)
- [Adding a distribution](adding-distros.md)
- [IPC protocols](ipc-protocol.md)
- [Security heuristics](security-heuristics.md)
- [Component contribution rules](../../../../src/cosh-ng/CONTRIBUTING.md)
