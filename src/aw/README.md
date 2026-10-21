# AW

[中文版](README_zh.md)

AW lets different Agents share security checks, tool output compression, and custom rules.
It starts a native Agent from one configuration and runs selected Providers around
its tool calls. The Agent keeps its model configuration, interface, and permissions;
AW records execution locally for inspection. The current release is a Linux Preview.

## Install and use

AW ships as Preview packages and is not yet available through `anolisa install` or
RPM. Follow the [installation guide](../../docs/user-guide/en/user-entrypoint/aw/installation.md)
to install core or all-in-one, generate a configuration with `aw-package configure`,
and run it.

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

Use `aw validate --config next.yaml --against current.yaml` to check policy-only
edits offline. See [configuration](../../docs/user-guide/en/user-entrypoint/aw/configuration.md#compare-policy-edits);
this does not update a running instance.

The local service remains available after the Agent exits. Query it and stop it
when finished.

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

Core works independently. The Tokenless package includes RTK and TOON encoding;
security checks require a running sec-core backend. Enable these optional
capabilities in the configuration. AW does not install Agents or configure model credentials.

## Documentation

| Task | Guide |
| --- | --- |
| Choose capabilities and get started | [AW user guide](../../docs/user-guide/en/user-entrypoint/aw/README.md) |
| Install packages, generate configuration, and run for the first time | [Install AW](../../docs/user-guide/en/user-entrypoint/aw/installation.md) |
| Select an Agent, enable Providers, and edit configuration | [Configure AW](../../docs/user-guide/en/user-entrypoint/aw/configuration.md) |
| Compress tool output | [Tokenless](../../docs/user-guide/en/user-entrypoint/aw/tokenless.md) |
| Enable command security checks | [Security checks](../../docs/user-guide/en/user-entrypoint/aw/security.md) |
| View Agent activity | [Connect AgentSight](../../docs/user-guide/en/user-entrypoint/aw/observability.md) |
| Inspect status, upgrade, uninstall, and troubleshoot | [Operations](../../docs/user-guide/en/user-entrypoint/aw/operations.md) |
| Build and package on customer machines | [Build AW from source](../../docs/developer-guide/en/aw/building.md) |
| Write rules or integrate Providers and SDKs | [AW developer guide](../../docs/developer-guide/en/aw/README.md) |

User documentation lives in `docs/user-guide/{en,zh}/user-entrypoint/aw/` and
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

The root is a virtual workspace. `crates/` contains nine framework crates; repository-root `providers/`
is the shared implementation space. Providers may also be developed and shipped
from independent repositories; see [Provider collaboration](../../providers/README.md). Hook requests
flow from `aw-service` to `aw-host`, which calls Providers through `aw-exec`.
`aw-core` exposes a separate plan API; the service reuses only its Journal.

The [architecture overview](../../docs/developer-guide/en/aw/architecture.md)
explains crate ownership and runtime flow. The [design index](docs/design/README.md)
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
