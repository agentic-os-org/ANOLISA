# Tokenless Windows integration

[中文版](../../zh/tokenless/windows.md)

This reference documents the Windows adapter's host contract and path lookup
for maintainers. For installation and daily use, see the
[QwenWork user guide](../../../user-guide/en/tokenless/framework-integration.md#qwenwork-windows-x64-local-mode).

## QwenWork host contract

QwenWorkCN 1.3.2-26101704 supplies
`QWENWORK_PLUGIN_HOOK_COMPATIBILITY=managed-tool-hooks-v1` through the `env`
entries for `opencode` and `qwencode` in
`resources/agent-sdk/engine-runtime-manifest.json` inside its installation.
This identifies `Bash` events that execute in Windows PowerShell. The adapter
reads the host value; the installer must not force it, because legacy Qoder CLI
sessions use Git Bash and need POSIX rewriting when the flag is absent.

Windows installation sets each command hook's `shell` to `powershell`.
In the same QwenWorkCN version, the installed
`resources/agent-sdk/opencode/qoderwork-plugin-adapter.mjs` passes
`configuration.shell ?? true` to Node's `spawn`. This is the host code that
selects PowerShell for the Python hook command; the manifest flag above instead
identifies the shell used by the tool. Hosts that ignore `shell` are unverified:
the generated command uses PowerShell syntax and cannot run under cmd.exe.
The verified host ignores ordinary hook execution failures and leaves the tool
input unchanged; exit code 2 is reserved for blocking decisions. A missing
Python interpreter or shared hook resource therefore disables optimization for
that invocation without requiring another dispatcher.

PreTool matches `Bash`, `PowerShell`, `mcp__workspace__bash`, and
`mcp__qw-builtin__mcp_workspace_bash`; RTK decides whether to rewrite each command.
PowerShell calls bind the native RTK executable and scope attribution variables
to that invocation. PostTool classifies PowerShell output as command output,
retains file-content protection, and avoids compressing RTK output twice.
Unsupported commands pass through.

The PowerShell wrapper uses a child-scope `rtk` alias. In a rewritten compound
command, this also binds any pre-existing bare `rtk` invocation to the managed
binary; POSIX binding only changes calls inserted by the rewrite. Explicit
executable paths are unaffected. The wrapper preserves `$?` and `$LASTEXITCODE`
after restoring attribution variables, including nonzero native exits, without
adding stderr output or entries to `$Error`.
When this wrapper is the final command passed to `powershell -Command` or
`pwsh -Command`, a `$false` value of `$?` produces process exit code `1`; the original
native exit code remains available in the session's `$LASTEXITCODE`.

The lifecycle script searches `%LOCALAPPDATA%/Programs/QwenWorkCN`, including
version subdirectories. `TOKENLESS_QWENWORK_APP` overrides the installation
directory; `TOKENLESS_QWENWORK_HOME` overrides the data directory
(default `~/.qwenworkcn`). The plugin is copied to `plugins-custom/tokenless`,
and hooks record the Python interpreter's absolute path. Installation does not
change settings or enable disabled plugins. Modified or foreign plugins are
not overwritten or removed. The desktop application needs the npm command
directory on its PATH.

Native hook tests cover rewriting, execution, compression, file-read protection,
statistics, deduplication, and lifecycle operations. The optional installed-host
tests read both engines' manifest environments and invoke the installed OpenCode
adapter to verify hook shell selection, rewriting, PostTool, Read protection,
statistics, and fail-open after a shared resource is removed. These tests do not establish
that a particular application session has loaded or enabled the plugin; check
that separately in a QwenWork local session.

## Binary discovery and support boundaries

Native binary discovery uses the executable's sibling directory and absolute
PATH entries. Unlike Unix, Windows discovery does not check file or directory
ACLs, ownership, or reparse points: configured installation and PATH directories
are trusted. RTK resolution accepts only `.exe`; general dependency discovery
also accepts `.com`, `.bat`, and `.cmd` entries from `PATHEXT`.

Windows support covers the native CLI and QwenWork adapter. npm copies other
adapter resources but skips Claude auto-enable; other Windows adapters are not
validated. Security-workspace/VM sessions and Windows ARM64 are unsupported.

## Build and test on Windows

Build this component from native PowerShell with Git, Rust's
`x86_64-pc-windows-msvc` toolchain, and Visual Studio C++ Build Tools (including
the Windows SDK). Packaging requires Node.js/npm and Python 3.10+; the
QwenWork adapter also uses Python. No Git Bash, WSL, or `make` is required. Windows ARM64 is not
supported; Windows CI remains separate from this build entry point.

```powershell
cd src/tokenless
./scripts/build-windows.ps1
node npm/scripts/package-npm.js --target win32-x64
$packages = Get-ChildItem npm/dist/tokenless-win32-x64/*.tgz, npm/dist/tokenless/*.tgz
npm install -g $packages.FullName
```

The build script fetches and patches the pinned RTK revision and writes both
executables to `target/npm-prebuilt/win32-x64`. Add `-Debug` for a development
build. The npm packages are local artifacts; Windows packages are not yet
published. The existing Linux/macOS release workflows are unchanged.

For a combined release, transfer the complete `target/npm-prebuilt/win32-x64`
directory from the Windows build host to the same location under `src/tokenless`
on the Linux packaging host. Use the same source revision on both hosts, then
run `make npm-package-all` after staging the other four platform directories.
`--all` requires all five targets and fails if any binary is missing;
`--target x64` also includes Windows. Use `--target linux` or `--target darwin`
when packaging only those platforms. No release workflow transfers Windows
artifacts yet; Windows CI will be added separately after support merges.

For local verification, run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
and `python -X utf8 -B tests/test_windows.py` after packaging. Set `PYO3_PYTHON`
to the intended Python executable when testing the Python workspace member.
The Windows smoke suite invokes real PowerShell hooks and native executables;
application UI loading and activation require a separate QwenWork local session.
Set `TOKENLESS_TEST_QWENWORK_APP` to the installed version directory (containing
`QwenWorkCN.exe`) to test managed Bash with the actual host manifest environment
for both engines and both MCP aliases, and to exercise the installed OpenCode
hook runner using Node on PATH. Set `TOKENLESS_TEST_BASH` to Git Bash's executable
to include legacy execution tests. The database rejection fixtures use system
temporary storage (`%SystemRoot%/Temp` on Windows, `/tmp` on Unix), independently
of a home-backed `TEMP`/`TMP`/`TMPDIR`; they report unavailable fixtures if that
directory is unwritable or inside the trusted home. A separate test covers
allowed home paths. Set `TOKENLESS_TEST_POWERSHELL` to `pwsh.exe` when rerunning
the Rust `powershell_binding` tests and the installed-host smoke test's rewritten
tool command on PowerShell 7; their default is Windows PowerShell 5.1. The host
still executes the registered Python hooks with the installer's `powershell` setting.
To test installed npm launchers, set `TOKENLESS_TEST_BIN` to the installed
`anolisa-tokenless/bin` directory and `TOKENLESS_TEST_NODE` to the Node executable
to verify (including the supported minimum, 16.7.0).
