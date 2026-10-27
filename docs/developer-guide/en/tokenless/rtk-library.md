# RTK library API maintenance

[中文版](../../zh/tokenless/rtk-library.md)

This guide covers the library API maintained as a patch to RTK v0.49.0.
Tokenless currently invokes and distributes standalone RTK; its CLI does not
link this library.

## Public API

The patch adds `lib.rs` with `rewrite(&str) -> RewriteOutcome`,
`execute(&[OsString]) -> Result<i32, RtkError>`, `VERSION`, and the necessary
result/error types.

`rewrite` preserves native `rtk` command text, configuration, permission rules
and recovery-read receipts. The caller maps `Allow`, `Ask`, `Deny` and
`Passthrough` to output and exit codes. A rewrite does not authorize execution.

`execute` accepts arguments without the program name and preserves command stdio
and exit status. Help, argument errors and execution errors return to the caller.
It does not read the host's command line or replace its signal handlers. The host
owns deadlines, process termination and descendant cleanup. Management commands
are excluded from both parsed execution and native fallback.

`lib.rs` privately compiles `main.rs` to reuse its argument definitions and
in-place dispatch. Keep the standalone startup and the library entry separate
when updating RTK, so library calls do not start telemetry or hook reminders.

## Pinned source and validation

Maintain the API in `src/tokenless/third_party/patches/rtk-library.patch`.
`src/tokenless/scripts/setup-rtk.sh` declares the pinned commit and ordered patch
list. It verifies patch SHA-256 values and the applied-source digest before
reusing a prepared tree, including a source archive without Git metadata.
Preserve local edits before running `just clean-rtk` and preparing again.

Build and test on Linux with Rust 1.91 or later. Initial preparation requires
`git`, `patch` and access to `https://github.com/rtk-ai/rtk.git`; a verified prepared
tree can be reused offline.

```bash
cd src/tokenless
bash scripts/setup-rtk.sh
RUSTUP_TOOLCHAIN=1.91.0 make test-rtk-integration
```

This target runs the library and standalone CLI formatting, Clippy, tests and
rustdoc checks, then the Tokenless-to-RTK integration tests. Run
`RUSTUP_TOOLCHAIN=1.91.0 make test-rtk-library` for only the RTK checks. When
updating the pin, review the parser-inventory test's command exposure decisions
and retain coverage for permission outcomes, explicit arguments, host return,
signal handlers and standalone compatibility.

## Legacy hook integrity

Legacy Hook integrity belongs to the standalone RTK CLI. The library executes
Rust command handlers directly; neither `rewrite` nor `execute` inspects or runs
`~/.claude/hooks/rtk-rewrite.sh` or verifies its hash baseline. Changes to that
script do not block library execution, native fallback or `recall`. Configuration
and rewrite permission rules still apply.

The standalone CLI retains the upstream check and command classification:
`rtk git status` checks integrity, while `rtk rewrite` and `rtk recall` are exempt.
If the standalone CLI reports a modified script, inspect it with `rtk verify`,
then run `rtk init -g --auto-patch` only after reviewing the change. These
management commands remain outside the library API. Do not delete the baseline
or re-trust unexplained modifications to bypass the standalone check.

## Rollback

Revert the library patch together with its source-preparation and validation
changes, then regenerate the prepared RTK tree. No user configuration or database
migration is required.
