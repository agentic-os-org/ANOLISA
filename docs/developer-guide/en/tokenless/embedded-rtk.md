# Embedded RTK maintenance

[中文版](../../zh/tokenless/embedded-rtk.md)

Tokenless links RTK v0.49.0 into its CLI. npm, raw/RPM, Python wheels, ANOLISA, and
AW packages ship `tokenless` (`tokenless.exe` on Windows). The standalone RTK CLI
remains buildable for upstream compatibility tests.

## Library boundary

`third_party/patches/rtk-library.patch` adds RTK's `lib.rs`. Its public surface is
`rewrite(&str) -> RewriteOutcome`, `execute(&[OsString]) -> Result<i32, RtkError>`,
`VERSION`, and those result/error types. Execution arguments exclude the program
name. The library returns instead of explicitly exiting and preserves command stdio.

The standalone `main.rs` retains management dispatch, telemetry, and hook reminders.
The two entries share argument definitions and execution dispatch; no `CliMode` is
needed. Vitest receives module-owned execution options. Argument restoration receives
the caller's argument vector rather than reading the embedding process's arguments.

Only `tokenless-cli` links RTK. The runtime and Python native extension invoke the
selected Tokenless executable with the argument prefix `rtk rewrite`. This preserves
the existing process deadline and descendant cleanup. RTK's library rewrite keeps
native `rtk` text; the Tokenless integration converts only confirmed inserted wrappers.
POSIX execution binds the absolute Tokenless path. PowerShell uses a scoped forwarding
function with native arguments, pipeline input, exit status, and environment restoration.

The embedded entry excludes upstream management commands. See the
[CLI reference](../../../user-guide/en/tokenless/cli-reference.md#bundled-rtk-commands)
for the supported entry points and rewrite exit codes.

## Source preparation and validation

Build on Linux with Rust 1.91 or later. Always prepare RTK before Cargo resolves the
workspace. The setup checks both the pinned upstream commit and the ordered patch
SHA-256, plus a digest of the applied source tree. Modified source files and untracked
additions reject cache reuse, including source archives without Git metadata.
A changed patch set rejects a stale generated checkout; preserve any local
edits before removing that checkout and preparing it again.

```bash
cd src/tokenless
bash scripts/setup-rtk.sh
cargo +1.91.0 build --locked -p tokenless-cli
cargo +1.91.0 test --locked --manifest-path third_party/rtk/Cargo.toml
cargo +1.91.0 test --locked -p tokenless-cli -p tokenless-runtime
make test-rtk-integration
```

The Windows final CLI carries the RTK stack-size requirement. Release payloads include
the upstream license, source commit, patch digest, and local patches. AW builds from a
fresh copy of tracked Tokenless sources and prepares its pinned RTK dependency there.

`make fmt` and `make lint` also prepare this workspace dependency. The first run
needs `git`, `patch` and access to `https://github.com/rtk-ai/rtk.git`; a verified
prepared tree can be reused offline. Setup reports fetch failures explicitly.

## Legacy hook integrity

`rewrite` only transforms text using configuration and permission rules. It does
not execute a command or authorize one, so it does not inspect legacy RTK hooks.
`execute` checks legacy hook integrity before both parsed execution and native
fallback. A modified hook with a stored baseline blocks execution and returns an
error to the host. The standalone RTK CLI retains its pre-dispatch integrity check.

If this gate reports a modified `rtk-rewrite.sh`, inspect the file and its origin
before repairing it. Hook inspection and repair require a separately installed or
built upstream RTK CLI (`rtk verify`, then `rtk init -g --auto-patch` after review).
Tokenless does not expose those management commands. Do not delete the baseline or
re-trust unexplained modifications to bypass the check.

## Upgrade and rollback

Existing explicit `rtk ...` commands are preserved as calls to a separately
installed RTK. They are not adopted or redirected by rewriting. If you relied on
the launcher formerly shipped by Tokenless, change those commands to
`tokenless rtk ...`; without standalone RTK, the old command will no longer resolve.

Python's public SDK and `rtk_enabled` stay unchanged; the private helper moves to
`_bin/tokenless` or `_bin/tokenless.exe`. Runtime protocol values, `rewrite-command`
statistics, attribution fields, receipts, and database formats are unchanged.

AW configurations use `--cli-sha256` for the same executable passed by `--cli`.
Install an AW upgrade into a new prefix, generate a new managed configuration with
`aw configure`, and restart sessions using that configuration. The generated argv
uses `--cli-sha256`; old `--rtk` and `--rtk-sha256` arguments are no longer accepted.
Keep the old prefix and configuration together for rollback, then uninstall the
old prefix through AW when rollback is no longer needed. PreTool and PostTool verify ownership,
permissions, checksum, embedded RTK version, and the remaining request deadline before
execution. Retry suggestions still require a new native authorization decision.

Remove old RTK files only through the original installer's ownership records or its
owned helper directory. Preserve separately installed RTK. To roll back, restore the
previous program/package and its matching configuration; no database migration is needed.
