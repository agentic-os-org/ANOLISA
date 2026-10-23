# AW

[中文版](README_zh.md)

AW lets different Agents share security checks, tool output compression, and custom rules.
It starts a native Agent from one configuration and runs selected Providers around
its tool calls. The Agent keeps its model configuration, interface, and permissions;
AW records execution locally for inspection. The current release is a Linux Preview.

## Install and use

AW ships as Preview packages and is not yet available through `anolisa install` or
RPM. Follow the [installation guide](../../docs/user-guide/en/aw/installation.md)
to install core or all-in-one, generate a configuration with `aw config init`,
and run it.

An existing AW installation can install an unpacked local bundle with
`aw install --prefix ABS_DIR --bundle ABS_DIR`, edit it with `aw config`, and
remove its owned files with `aw uninstall --prefix ABS_DIR --yes`. Native Hook
registration uses `aw agent connect --config FILE --agent TARGET`. The legacy
native `aw install --config ... --agent ...` remains compatible. The standalone
`aw-package` executable has been removed; the Rust library remains. Every bundle
includes `aw` at its root for first installation, using an explicit absolute
`--bundle` path. Named components, direct URLs and system package backends are not
supported by this Preview installer.

Use `--dry-run` to inspect install or removal plans without creating files.
`aw uninstall PACKAGE --prefix ABS_DIR --yes` removes selected packages after
checking dependencies, file ownership and running references. New operations
commit a local package database; `aw recover` resumes cleanup or rolls back an
interrupted operation. User scope is the default. Root-managed raw installs
require `sudo aw install ... --scope system`; privileges never select that scope
implicitly. RPM/DEB support remains separate.

Canonical removal asks for confirmation only with interactive stdin and stderr.
Scripts and JSON output must use `aw uninstall --prefix ABS_DIR --yes` (or `-y`).
Confirmation never overrides package checks or system permissions.

Use `aw list --installed --prefix ABS_DIR` to list packages recorded in an
installation and `aw doctor --prefix ABS_DIR` to check their files. Both commands
are read-only and accept `--json`. Without `--prefix`, they read the current
user's installed-package database. An explicit prefix follows its recorded
database, including a system installation readable by an ordinary user. Old
Preview installations still need an explicit prefix; queries do not import their
`.aw-packages` records. A successful package write archives those old records
and registers the prefix only when the recorded AW executable matches the
current command. Otherwise, `runtime_unknown` blocks migration before writes;
install the new AW build in a separate prefix. See
[installed packages and file checks](../../docs/user-guide/en/aw/operations.md#installed-packages-and-file-checks).

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

Use `aw validate --config next.yaml --against current.yaml` to check policy-only
edits offline. See [configuration](../../docs/user-guide/en/aw/configuration.md#compare-policy-edits);
this does not update a running instance.

Use `aw run --reloadable` to preinstall the experimental idle-update bridge for
Qoder 1.1.73 TUI or QwenPaw 2.2.2b4 / AgentScope 2.0.9 App.
`aw update --config current.yaml --agent TARGET --policy next.yaml` selects the
unique target instance, retains its process and rejects busy instances; see
[prewarmed instances](../../docs/user-guide/en/aw/configuration.md#update-a-prewarmed-instance).

The local service remains available after the Agent exits. Query it and stop it
when finished.

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

The same operations are available as `aw service status` and `aw service stop`.
`aw service serve` runs a foreground service; it does not install a system unit.
Use `aw service --help` for options and `aw --version` for the CLI crate version.

Use `aw --help`, `aw config --help`, and `aw config validate --help` to
discover the implemented command tree. Public commands also accept `-h`;
`aw -V` reports the CLI version. Help must be the final option, and unknown
or repeated options fail before work starts. Options such as `--json` remain
command-local. See [command help](../../docs/user-guide/en/aw/operations.md#command-help-and-arguments).

Use `aw commands` to list implemented public syntax, or `aw commands --json`
for versioned command metadata. `aw completion bash` prints a completion script;
Zsh, Fish, PowerShell and Elvish are also supported. Scripts are not installed
automatically. See [command discovery](../../docs/user-guide/en/aw/operations.md#command-discovery-and-completion).

Local package install/uninstall, `config init`, `config validate`, and offline configuration edits
accept `--json` for a versioned management result. Human summaries remain the
default; existing service and protocol JSON is unchanged. See
[management reports](../../docs/user-guide/en/aw/operations.md#management-reports) for supported commands and failure semantics.

These management commands accept local `--quiet`, `--verbose` and
`--color auto|always|never` for human output. They cannot accompany `--json`;
native output and protocol streams remain unchanged. Package reports distinguish
commit, cancellation, incomplete recovery and cleanup warnings without inferring
rollback from an exit code. See
[terminal output](../../docs/user-guide/en/aw/operations.md#terminal-output).

Core works independently. The Tokenless package includes RTK and TOON encoding;
security checks require a running sec-core backend. Enable these optional
capabilities in the configuration. AW does not install Agents or configure model credentials.

## Documentation

| Task | Guide |
| --- | --- |
| Choose capabilities and get started | [AW user guide](../../docs/user-guide/en/aw/index.md) |
| Install packages, generate configuration, and run for the first time | [Install AW](../../docs/user-guide/en/aw/installation.md) |
| Select an Agent, enable Providers, and edit configuration | [Configure AW](../../docs/user-guide/en/aw/configuration.md) |
| Compress tool output | [Tokenless](../../docs/user-guide/en/aw/tokenless.md) |
| Enable command security checks | [Security checks](../../docs/user-guide/en/aw/security.md) |
| View Agent activity | [Connect AgentSight](../../docs/user-guide/en/aw/observability.md) |
| Inspect status, upgrade, uninstall, and troubleshoot | [Operations](../../docs/user-guide/en/aw/operations.md) |
| Build and package on customer machines | [Build AW from source](../../docs/developer-guide/en/aw/building.md) |
| Write rules or integrate Providers and SDKs | [AW developer guide](../../docs/developer-guide/en/aw/README.md) |

User documentation lives in `docs/user-guide/{en,zh}/aw/` and
integration references in `docs/developer-guide/{en,zh}/aw/`. Component protocol
designs remain in this directory's `docs/design/`.

## Hook support

This table is generated from the [regression matrix](tests/regression-matrix.json).
It lists capabilities declared by the runtime and covered by CI contract tests.
See the [native Agent integration reference](../../docs/developer-guide/en/aw/native-agents.md)
for supported versions, tool types, and text replacement boundaries.

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.after` / `observe` | `tool.after` / `replace_result` |
| --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ |
| `openclaw` | ✓ | ✓ | ✓ | ✓ |
| `pi` | ✓ | ✓ | ✓ | ✓ |
| `qoder` | ✓ | ✓ | ✓ | ✓ |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->

A Tokenless retry suggestion blocks the original call. The Agent must make a new
call through security and native permission checks. Replacement applies only to
supported text slots. AW does not provide an OS sandbox or cross-Agent approvals.
See [Contributing](CONTRIBUTING.md) for real SDK and model-session acceptance coverage.

## Code architecture

![AW user workflow and component architecture](../../docs/images/aw/architecture.png)

The root is a virtual workspace. `crates/` contains ten framework crates; repository-root `providers/`
is the shared implementation space. Providers may also be developed and shipped
from independent repositories; see [Provider collaboration](../../providers/README.md). Hook requests
flow from `aw-service` to `aw-host`, which calls Providers through `aw-exec`.
`aw-core` exposes a separate plan API; the service reuses only its Journal.

`aw-cli` owns command parsing and presentation, calling the `aw-package` library
for installation and configuration and `aw-service` for runtime operations.

The [architecture overview](../../docs/developer-guide/en/aw/architecture.md)
explains crate ownership and runtime flow. The [design index](docs/design/en/README.md)
organizes protocol details in call order.

## Development and validation

Run the AW CI checks from the repository root.

```bash
python3 src/aw/scripts/check.py
```

Qoder CLI and QwenPaw App/API each exercise real sec-core allow, scan denial,
backend failure and timeout blocking in disposable containers. Qoder uses the
nightly account Secret; QwenPaw uses a local deterministic model. See
[CI coverage](CONTRIBUTING.md#daily-ci-and-nightly-integration) for the full boundary.

- [Development environment and tests](CONTRIBUTING.md)
