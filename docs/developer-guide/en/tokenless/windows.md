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
supported. The Linux CI entry point is described below.

```powershell
cd src/tokenless
./scripts/build-windows.ps1
node npm/scripts/package-npm.js --target win32-x64
$packages = Get-ChildItem npm/dist/tokenless-win32-x64/*.tgz, npm/dist/tokenless/*.tgz
npm install -g $packages.FullName
```

The build script fetches and patches the pinned RTK revision and writes both
executables to `target/npm-prebuilt/win32-x64`. Add `-Debug` for a development
build. The npm packages are local artifacts; this command does not publish them.

For a combined release, transfer the complete `target/npm-prebuilt/win32-x64`
directory from the Windows build host to the same location under `src/tokenless`
on the Linux packaging host. Use the same source revision on both hosts, then
run `make npm-package-all` after staging the other four platform directories.
`--all` requires all five targets and fails if any binary is missing;
`--target x64` also includes Windows. Use `--target linux` or `--target darwin`
when packaging only those platforms. The Linux CI builder also produces the Windows
binaries, so a native build host is optional for artifact generation.

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


## Linux CI and release artifacts

`tokenless-prebuilt-ci.yaml` uses the self-hosted Linux release runner
(`anolisa-release-cross-x64`) to cross-compile `x86_64-pc-windows-msvc` and run
Wine smoke tests on pushes and manual dispatches. Pull requests run the action's
regression tests on the general CI runner. The same build action participates in the release and preview
matrices. Historical source-tag previews keep the matrix from that source tag.

The runner must already contain `anolisa/rust-release-builder:windows-msvc-x86_64`.
Its configuration digest is
`sha256:7649bcb55d8c5632232c7f27e3c2191b71fb7c31149de87412c447fc7a997303`.
Docker's containerd image store reports the corresponding manifest digest as the
image ID: `sha256:1392a2684a4853a3da0219caef07c4b47f8833775b1559f18d54cd656abefb88`.
Both fixed identifiers refer to the same verified builder.
The action checks that identity and the image's SDK inventory before building;
it does not build or pull the image. Provision it before enabling the job.
The frozen image contains Rust 1.93.0, cargo-xwin 0.23.1, maturin 1.15.0,
LLVM 18.1.3, Wine 9.0 and Windows Python 3.11.9 with its import libraries.
The host needs the existing release-runner tools, including Docker,
Node.js/npm and cargo-sbom 0.10.0. Source, Cargo downloads, build output and Wine
state use separate mounts; host compiler wrappers and Cargo configuration are
not passed into the container.

Run the production entry point locally from the repository root with empty output
directories. It builds the checked-out commit, or the exact `--tag` when supplied:

```bash
root="$PWD"
mkdir -p /tmp/tokenless-windows-release
RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= \
TOKENLESS_SOURCE_WORKTREE=/tmp/tokenless-windows-release/source \
  bash .github/actions/build-tokenless-prebuilt/build.sh \
    --source-repo "$root" \
    --output-dir /tmp/tokenless-windows-release/artifacts \
    --wheel-output-dir /tmp/tokenless-windows-release/wheels \
    --version 0.9.0 --target-os windows --target-arch x86_64 \
    --profile windows-msvc-x86_64
```

The Windows ZIP contains `tokenless.exe`, `rtk.exe`, `LICENSE` and two npm
tarballs under `npm/` (the Windows-targeted root package and `win32-x64` platform
package). It has SHA-256 and CycloneDX sidecars like the other release archives.
CLI and RTK use a static CRT, with an import check rejecting external VC++ runtime
DLLs. The separate Python wheel uses `cp311-abi3-win_amd64` and the runtime supplied
with Windows Python. The release workflow uploads
these assets to GitHub; it does not publish to npm or PyPI. Use all five platform
packages when preparing a combined npm registry release, rather than publishing
the Windows-only root tarball as the cross-platform package. Repeated builds currently
differ in PE build identifiers; the checksums identify the specific generated
files, and byte-for-byte reproduction is not guaranteed.

Wine smoke tests extract the ZIP and wheel into a path containing spaces and
Chinese characters, then exercise the CLI, RTK, SDK, compression, byte-exact
Stash recovery, denied retrieval and statistics attribution. npm metadata and
embedded binaries are also checked. Smoke execution has no network access and
an outer timeout. Compiler and Wine failures fail the job. Builder diagnostics
remain in `<output-dir>.build/out`; the CI job uploads those diagnostics.
Wine does not validate native Windows ACL behavior, PowerShell hooks, Node
launchers or QwenWork activation. Continue running the native test suite above
on Windows 10 and Windows 11 for those acceptance checks.
