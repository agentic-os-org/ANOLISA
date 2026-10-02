# Testing cosh-ng

[中文版](../../zh/cosh-ng/testing.md)

cosh-ng uses layered deterministic tests. Start at the cheapest layer that can
prove the behavior, then widen coverage in proportion to process, PTY, wire, or
security risk. Do not use exact test counts as documentation; inventory floors
change as the implementation grows.

## Fast feedback

Run from `src/cosh-ng`:

```bash
cargo test --locked -p cosh-types
cargo test --locked -p cosh-platform
cargo test --locked -p cosh-cli --test cli_integration
cargo test --locked -p cosh-core --lib
cargo test --locked -p cosh-shell --lib
```

Use a test-name filter while iterating:

```bash
cargo test --locked -p cosh-core session_recovery
cargo test --locked -p cosh-shell --test logic slash_registry
```

## Shell integration layers

| Target | Put a test here when it proves | Typical cost |
|---|---|---|
| `--lib` | Private pure logic or a lightweight component | Lowest |
| `--test logic` | Public multi-module behavior without process transport | Low |
| `--test protocol` | Adapter/control serialization and state transitions | Low to medium |
| `--test raw_cli` | A spawned shell binary, cards, provider handoff, or scripted raw input | Medium |
| `--test shell_host` | PTY, OSC, termios, native shell, or foreground-program behavior | Highest default layer |

Examples:

```bash
cargo test --locked -p cosh-shell --test logic
cargo test --locked -p cosh-shell --test protocol -- --test-threads=4
cargo test --locked -p cosh-shell --test raw_cli <test-name> -- --exact
cargo test --locked -p cosh-shell --test shell_host -- --test-threads=4
```

Do not put real-provider, visual, or manual-terminal checks into the default
Cargo gate. Such validation must be explicitly requested and reported
separately from deterministic behavior.

## Core integration targets

Core tests are organized by contract rather than one monolithic suite:

| Target | Contract |
|---|---|
| `jsonl_protocol` | Headless message and streaming behavior |
| `registry_protocol` | Skills, extensions, auth, and registry actions |
| `tool_approval` | Tool decision protocol |
| `session_recovery` | Persisted conversation lifecycle |
| `compaction_lifecycle` | Manual and automatic compaction |
| `oauth_mcp` | MCP OAuth control flow |
| `sls_integration` | Export integration with deterministic fixtures |
| `sigint` | Process interruption behavior |

Run the target closest to the change, then the complete core package when the
change affects shared runtime state.

## Canonical gates

The repository scripts avoid duplicate lib/bin executions and audit test/layout
inventory:

```bash
scripts/run-test-gates.sh fast         # local iteration and focused handoff
scripts/run-test-gates.sh integration  # all process/protocol integration targets
scripts/run-test-gates.sh all          # canonical deterministic suite
scripts/run-test-gates.sh heavy        # selected ignored manual-grade cases
```

`scripts/check-test-inventory.sh` enforces regression floors and ignored-test
ceilings. `scripts/check-test-necessity.sh` checks whether a change that needs a
test has one. `crates/cosh-shell/scripts/check-layout.sh` audits source and test
placement. Do not lower these baselines in a feature or fix merely to pass CI.

## Broader local gates

For ordinary code changes, stop after the formatter and tests closest to the
changed behavior. Run the complete local gate only for large or cross-cutting
code changes when the task explicitly asks for that depth; otherwise CI owns
broad regression coverage.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
scripts/run-test-gates.sh all
cargo build --workspace --release
```

Add `cargo doc --workspace --no-deps` when changing public API or rustdoc.
Documentation-only changes need link, formatting, command, and bilingual parity
checks rather than Rust tests.

## Test design rules

- Use temporary directories and test-only path overrides; never depend on a
  developer's real home, config, keyring, or session store.
- Mock providers and transports. A network credential is not a test fixture.
- Verify the public boundary: JSON envelope, JSONL message, terminal output,
  filesystem permission, exit status, or protocol bytes.
- For safety fixes, include the benign control case and the adversarial input
  that previously bypassed the gate.
- Keep PTY timing bounded and wait on observable state instead of arbitrary
  sleeps.
- Never remove assertions, ignore tests, or broaden timeouts without explaining
  the behavioral reason.

The optional `e2e/run.py` runner validates installed launchers and real PTY
paths under named profiles. It is a later system gate, not a substitute for the
scoped Cargo tests above.
