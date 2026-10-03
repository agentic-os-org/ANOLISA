# cosh-shell

`cosh-shell` is the shell-first interactive surface for `cosh-ng`.

It runs a managed PTY shell, intercepts Agent-oriented input, renders inline cards with Ratatui, and coordinates provider adapters, approvals, questions, hooks and shell evidence.

## Current Boundaries

- Recommendations are display-only.
- Shell commands typed by the user run in the foreground PTY.
- User-approved fallback shell handoff is being consolidated behind a typed foreground PTY handoff.
- Auto read-only execution must remain host-visible and auditable.
- Legacy slash commands and old governance wording are compatibility paths, not the target product model.

See `../../docs/specs/shell-architecture-optimization/` for the current architecture optimization SDD.

## Input-Wait Handling

When an agent-approved foreground command sits in a kernel-evidenced input wait (password prompt, pager, plain stdin read on the session tty), the shell shows an inline hint card and — after `shell.input_wait_timeout_secs` (default 120, `0` disables) — interrupts the foreground group and reports the wait to the provider. Fullscreen TUIs and pipeline reads are exempt. See the [configuration guide](../../docs/user-guide/en/user-entrypoint/cosh-ng/configuration.md#cosh-shell-configuration).

## Build And Check

```bash
cargo check --package cosh-shell --all-targets
cargo test --package cosh-shell --lib -- --test-threads=1
cargo clippy --package cosh-shell --all-targets -- -D warnings
```

## Raw Shell

```bash
cargo run --package cosh-shell -- raw --shell bash
cargo run --package cosh-shell -- raw --shell zsh
```

## Validation

```bash
scripts/e2e/cosh-shell-audit-raw-cli --out /tmp/cosh-shell-audit-raw-cli
```

Full release validation also requires provider, manual TTY and residual-process gates documented in `../../docs/specs/shell-e2e-validation/`.
