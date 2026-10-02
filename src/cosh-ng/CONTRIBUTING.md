# Contributing Guide

[中文版](CONTRIBUTING_zh.md)

## Development Environment

| Requirement | Version |
|-------------|---------|
| Rust toolchain | stable (managed by `rust-toolchain.toml`) |
| Minimum Rust version | 1.88 |
| Components | rustfmt + clippy |
| Supported platforms | Linux (full); macOS (limited functionality) |

```bash
cd src/cosh-ng
rustup show   # Confirm toolchain is ready
```

## Build

```bash
# Full build (all workspace crates)
cargo build --workspace

# Release build
cargo build --workspace --release

# Build a specific binary
cargo build --bin cosh-cli
cargo build --bin cosh-core
cargo build --bin cosh-shell
```

## Validation

Use checks proportional to the change:

```bash
# Ordinary code changes: format and run the closest tests
cargo fmt --all -- --check
cargo test --locked -p cosh-platform test_detect  # example

# Public API or rustdoc changes
cargo doc --workspace --no-deps
```

Documentation-only changes need link, formatting, command, and bilingual parity
checks; they do not require Rust tests. Add targeted Clippy, integration tests,
or the shell layout audit only when relevant to the changed behavior.

Full local gates and persistent ECS validation are reserved for large or
cross-cutting code changes when the current task explicitly requests that
depth. Otherwise CI provides broad regression coverage:

```bash
scripts/run-test-gates.sh all    # full deterministic gate
cargo build --workspace --release --locked
crates/cosh-shell/scripts/check-layout.sh
```

See the [developer getting-started guide](../../docs/developer-guide/en/cosh-ng/getting-started.md)
for code ownership and test-target selection.

## Workspace Structure

```
cosh-ng/
├── Cargo.toml              # workspace configuration
├── rust-toolchain.toml     # stable + rustfmt + clippy
└── crates/
    ├── cosh-types/         # Pure types, zero side effects
    ├── cosh-platform/      # Platform abstraction (distro detection, backend routing)
    ├── cosh-cli/           # CLI entry
    ├── cosh-core/          # Agent core
    ├── cosh-shell/         # Interactive terminal
    ├── cosh-gateway-contracts/ # Side-effect-free Gateway contracts
    └── cosh-gateway/       # Gateway control-plane library foundations
```

## Dependency Management

- All dependency versions are declared in `[workspace.dependencies]`
- Sub-crates reference via `dep = { workspace = true }`
- Check for existing equivalent crates before adding new dependencies
- Major version upgrades are not allowed without discussion

## Code Standards

### Module Organization

Use Rust 2018+ recommended file layout, **do not use `mod.rs`**:

```
# Correct
src/extension.rs        # Parent module
src/extension/          # Child module directory
    config.rs
    manager.rs

# Wrong — do not use
src/extension/mod.rs
```

### Error Handling

| Scenario | Approach |
|----------|----------|
| Library crate | `thiserror` enum |
| Binary | `anyhow::Result` |
| Unreachable path | `unreachable!()` + comment |
| Prohibited | `unwrap()` / `expect()` / `panic!()` |

### Comments

- `///` for all pub items
- `//` only explains *why*, does not repeat type signatures
- First line is a standalone summary, imperative or noun phrase
- No `TODO` without owner, no commented-out old code

### Clippy

- Default deny all warnings
- When genuinely needed, use narrowest scope `#[allow(clippy::xxx)]` + comment explaining why

## Commit Standards

Format: `type(cosh-ng): [crate_scope] imperative description`

- Types: feat / fix / refactor / docs / test / ci / chore
- Scope: `cosh-ng`
- Crate scope: `[core]`, `[shell]`, `[cli,platform]`, or another precise list
- Within 50 characters, English, imperative mood, lowercase first letter, no period
- Requires a `Signed-off-by` trailer

```bash
git commit -s -m 'feat(cosh-ng): [core] add hook registry list'
```

## PR Process

1. Branch from latest main
2. Follow branch naming: `feature/cosh-ng/<short-desc>`
3. Ensure all applicable checks pass before pushing
4. PR title follows commit message format
5. Fill in every applicable PR template section, including risk, validation,
   documentation, and rollback
