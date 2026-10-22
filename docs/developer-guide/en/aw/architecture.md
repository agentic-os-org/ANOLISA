# AW architecture

[中文版](../../zh/aw/architecture.md)

AW runs policies around native Agent tool calls. An Adapter receives native
callbacks, the local service manages bindings and events, the Host invokes
Providers, and the Adapter returns results to the Agent. The Agent owns model
calls, tool scheduling, and permission approval.

## User workflow

![AW user workflow and component architecture](../../../images/aw/architecture.png)

This view follows user actions; arrows show runtime collaboration. The green
area is the Provider contribution space. The footer shows target release ownership:
component owners deliver components, and the OS distributor validates and combines
artifacts. See the [Provider guide](providers.md) for current integration and
subsequent release work.

## Crate relationships

The AW framework contains ten crates under `crates/`. Concrete Providers are
maintained separately under `providers/`.

| Crate | Responsibility | Direct AW dependencies |
| --- | --- | --- |
| `aw-cli` | Unified `aw` frontend, launch and command orchestration | adapter, config, exec, host, package, provider, service |
| `aw-service` | Local service, client and runtime state | adapter, config, contracts, core, exec, host, provider |
| `aw-adapter` | Native registration, callback normalization, phase validation and effect adoption | config, contracts, exec, provider |
| `aw-host` | Prepare Providers and execute admitted Hook steps | config, exec, provider |
| `aw-provider` | Validate Provider messages and capability admission | config |
| `aw-exec` | Bound process lifetimes and output; clean up owned processes | None |
| `aw-config` | Parse configuration and validate fields and static references | None |
| `aw-core` | Capability plan executor, Host/Journal ports, and FileJournal | contracts |
| `aw-contracts` | Schemas, canonical encoding, and cross-record validation | None |
| `aw-package` | Reusable package installation and configuration APIs; `aw-build` packaging tool | config, exec |

The dependency column omits the `aw-` prefix. Tests in `aw-package` also depend
on `aw-service`; CLI integration tests also use `aw-contracts`. Libraries do not
depend on `aw-cli`. The `aw` frontend calls package APIs in-process. The standalone
`aw-package` executable has been removed; the crate and `aw-build` remain.

The public `aw` tree uses one typed Clap declaration for parsing and help.
Admitted options call existing handlers without reparsing argv. Internal Hook
and lifecycle callbacks retain their protocol parser and failure replies.
Software installation, configuration and removal all use this `aw` tree;
the removal of the other executable does not change service messages,
Provider effects or existing file ownership records.

### Independent Providers

| Cargo package and binary | Directory | Responsibility | Framework dependencies |
| --- | --- | --- | --- |
| `aw-provider-sec-core` | `providers/agent-sec-core/` | Bridge the sec-core CLI for security checks | exec, provider |
| `aw-provider-tokenless` | `providers/tokenless/` | Bridge the Tokenless CLI for output compression | exec, provider |

These implementations each own an independent Cargo workspace and lockfile at
the repository root. They connect as separate processes and are not part of the
ten framework crates. Tokenless Provider tests also depend on the sec-core Provider. Other teams
can develop and distribute Providers independently; see the [collaboration guide](providers.md).

## One Hook request

1. `aw run` calls `aw-adapter` to check the native entrypoint and prepare registration,
   then the launcher starts the Agent.
2. The Agent triggers a callback; the Adapter translates it into a service request.
3. `aw-service` manages the binding, event deadline, and audit, then delegates the
   step to `aw-host`.
4. The Host validates protocol messages with `aw-provider` and starts the configured
   Provider command through `aw-exec`. Built-in bridges then invoke Tokenless or
   sec-core through their public CLIs, across a process boundary.
5. Results return through the same path. The Adapter adopts blocking or result
   replacement through the native interface. A successful Provider call alone
   does not prove adoption; the Adapter must confirm it.

Native Hook scripts also run through `aw-exec`, preserving raw output and exit
status without a Provider handshake. AgentSight is a separate optional observation
source; read-only `aw inspect` views do not participate in Hook admission.

### Explicit policy updates

Opt-in `aw run --reloadable` installs stable Adapter entrypoints. `aw update`
addresses an existing instance and asks `aw-service` to prepare a new immutable
Host outside the request gate, then publish it only after a final idle/revision
check under that gate. The service retains the bootstrap identity while each
instance owns its active policy. Native Adapters report request/tool lifetimes
and preserve scheduling and adoption; Provider protocols and Core plan execution
are unchanged. See [policy reload](../../../../src/aw/docs/design/en/runtime/policy-reload.md)
for version pins, busy rejection, process association and acceptance boundaries.

### Where `aw-core` fits

The current Hook path is `aw-service → aw-host → aw-exec`. The service reuses
`aw-core::journal::FileJournal` for audit storage, without calling
`Core::prepare/execute`.

`aw-core::Core` is a separate capability-plan API. Its embedding caller supplies
`ProviderHost`, `Clock`, and `Journal`; it uses `aw-contracts` to validate plans
and execution records. `aw-host::Host` and this `ProviderHost` port have separate
contracts and are not interchangeable call layers.

## Directories and source entry points

`src/aw/Cargo.toml` defines only the workspace. Ten framework crates live in
`crates/`; two concrete Providers live in the repository-root `providers/`, each
with its own workspace, source and tests. The former root `src/` contracts code now lives in `crates/aw-contracts/`.

`providers/` is a collaboration space for first-party and community implementations.
Providers can also ship independently and connect through the process protocol.
See [Provider development and distribution](providers.md) for directory conventions
and packaging scope.

| Path, relative to `src/aw/` | Content |
| --- | --- |
| `crates/aw-cli/src/bin/`, `crates/aw-cli/src/lib.rs`, `crates/aw-cli/src/cli/` | CLI entrypoints, launcher and RPC coordination |
| `crates/aw-cli/src/command.rs`, `crates/aw-cli/src/command/` | Typed public command declaration, generated help and handler dispatch |
| `crates/aw-service/src/server.rs`, `crates/aw-service/src/runtime/` | Local service and event execution |
| `crates/aw-host/src/lib.rs`, `crates/aw-host/src/event.rs` | Provider preparation and invocation |
| `crates/aw-core/src/journal.rs` | Durable audit storage |
| `crates/aw-contracts/src/registry.rs` | Contract schemas embedded in the program |
| `../../providers/agent-sec-core/`, `../../providers/tokenless/` | Concrete Providers, configuration examples, and tests |
| `crates/aw-adapter/src/` | Shared native dialects and catalog for launcher and service |
| `crates/aw-adapter/native/` | Packaged Python/JavaScript native bridge assets |
| `scripts/`, `tests/` | Workspace checks, cross-language vectors, and native acceptance |
| `docs/design/` | Protocol contracts, boundaries, and design rationale |

Follow the [design index](../../../../src/aw/docs/design/en/README.md) in call order
for details. [Architecture boundaries](../../../../src/aw/docs/design/en/architecture-boundaries.md)
explains Adapter interfaces and the deferred release changes. See [Contributing](../../../../src/aw/CONTRIBUTING.md) for the
development environment and checks.

## Schema ownership

| Contract | Owner |
| --- | --- |
| Active capability, plan and evidence schemas | `aw-contracts/schemas/` |
| Configuration | `aw-config/schemas/` |
| Provider messages and View protocol | `aw-provider/schemas/` |
| Provider private configuration and backend data | The concrete Provider directory |

`aw-contracts/schemas/` contains only registered, tested contracts, currently 23.
Plan, evidence and other v1 contracts remain active; the four capability profiles
use v2 inputs and outputs. Eight unregistered capability v1 drafts had no consumers
in this repository and were removed. Their contents remain in Git history.

New Providers can declare operations within the existing events and effects,
owning their private configuration and backend data. Adding a Core capability or
contract version requires schemas, Registry entries, cross-record validation and
valid and invalid examples together. Supported versions can coexist: give new
versions distinct names and `$id` values, retaining the old contracts and tests
until compatibility is explicitly retired. Relocating existing schemas must
preserve their bytes, `$id`, registry names and canonical digest vectors.
