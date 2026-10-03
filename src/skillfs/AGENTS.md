# AGENTS.md — SkillFS

> Common Rust conventions (comments, module layout, dependency management, error handling, pre-commit checks, commit conventions) are defined in the [root AGENTS.md](../../AGENTS.md#3-rust-common-conventions). This file documents **SkillFS-specific** additions only.

---

## 1. Workspace Layout

```text
crates/
  skillfs-core/   parser / store / views / compiler / env / watcher
  skillfs-fuse/   FUSE filesystem layer
  skillfs-cli/    `skillfs` binary (mount / classify / validate / list)
docs/specs/       implementation specifications
docs/skills/      the bundled agent skill (skillfs-mount)
scripts/          build.sh, test.sh
```

Each crate's `Cargo.toml` carries a one-line `description = "..."` that
matches the table above; keep them in sync.

---

## 2. Module Exception

The `tests/common/mod.rs` file in `crates/skillfs-core/tests/common/mod.rs` is the **only** allowed `mod.rs` — cargo's official convention for sharing helpers across integration tests.

---

## 3. Dependency Exceptions

The two per-crate version literals below are deliberate. Do not "tidy them up" into `[workspace.dependencies]`:

- `crates/skillfs-core/Cargo.toml :: notify = { version = "7", features = ["macos_kqueue"] }` — macOS-specific feature, single consumer.
- `crates/skillfs-cli/Cargo.toml :: clap = { version = "4", features = ["derive"] }` — CLI-only; no reason to pollute the workspace.

Any new exception must be justified in the PR description.

---

## 4. Error Handling Exceptions

The three pre-existing `unwrap()` usages — in `compiler.rs`, `parser.rs`, and `cli/main.rs` — each carry a one-line justification proving the failure case is impossible. New code must do the same; `unreachable!()` with a comment is preferred over bare `unwrap()`.

Existing named error types: `FuseError` / `ParseError` / `WatcherError`.

---

## 5. Additional Pre-submission Check

In addition to the standard Rust checks (see root AGENTS.md §3.5), SkillFS requires:

```bash
scripts/test.sh   # e2e FUSE mount; skips itself if fuse3 / /dev/fuse is missing
```

This FUSE smoke test is **required** when changing filesystem-layer code.
