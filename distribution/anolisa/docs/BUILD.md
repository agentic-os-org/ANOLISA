# ANOLISA CLI Build Guide

> 中文版: [BUILD_zh.md](BUILD_zh.md)

## Prerequisites

- Rust >= 1.93 (project uses edition 2024)
- Working directory: `distribution/anolisa/` (Cargo workspace root)

```bash
cd distribution/anolisa
```

### Rustup Toolchain Source

This workspace pins Rust `1.93.1` via `rust-toolchain.toml` — the newest
Rust that Alibaba Cloud Linux 4 packages, so source and RPM builds agree.
When using rustup-managed `cargo`, the toolchain is selected automatically
and downloaded if missing.

Before building, verify that `cargo`, `rustc`, and `rustdoc` come from the
same rustup toolchain:

```bash
which cargo
cargo -Vv
rustc -Vv
rustdoc -Vv
```

If your configured rustup source cannot provide Rust `1.93.1`, configure a
working source or preinstall the toolchain on the build machine.

For bash/zsh:

```bash
export RUSTUP_DIST_SERVER=<rustup-dist-server>
export RUSTUP_UPDATE_ROOT=<rustup-update-root>
```

For fish:

```fish
set -x RUSTUP_DIST_SERVER <rustup-dist-server>
set -x RUSTUP_UPDATE_ROOT <rustup-update-root>
```

---

## Local Development

```bash
# Compile only
cargo build -p anolisa-cli

# Compile and run
cargo run -p anolisa-cli -- env
cargo run -p anolisa-cli -- list
cargo run -p anolisa-cli -- enable agent-observability --dry-run

# Run tests
cargo test -p anolisa-core
cargo test --workspace
```

Output: `target/debug/anolisa`

---

## Production Build

```bash
cargo build --release -p anolisa-cli
```

Output structure:

```
target/release/anolisa          # main binary (symbol table retained, DWARF stripped)
target/release/anolisa.dwp      # split DWARF debug info (Linux)
target/release/anolisa.dSYM/    # debug info on macOS
```

Ship only the main binary; archive `.dwp` / `.dSYM` for coredump analysis:

```bash
# Place .dwp next to the binary, GDB discovers it automatically
gdb ./anolisa core.12345

# Or specify explicitly
gdb -s anolisa.dwp ./anolisa core.12345
```

---

## Cross-compilation (Linux x86_64 target)

```bash
# Add target
rustup target add x86_64-unknown-linux-gnu

# Cross-compile (requires matching linker, e.g. x86_64-linux-gnu-gcc)
cargo build --release -p anolisa-cli --target x86_64-unknown-linux-gnu
```

Output: `target/x86_64-unknown-linux-gnu/release/anolisa`

---

## Quick Reference

| Scenario | Command | Output |
|----------|---------|--------|
| Quick run | `cargo run -p anolisa-cli -- <subcmd>` | — |
| Debug build | `cargo build -p anolisa-cli` | `target/debug/anolisa` |
| Release build | `cargo build --release -p anolisa-cli` | `target/release/anolisa` |
