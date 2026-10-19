# AW architecture

[中文版](../../zh/aw/architecture.md)

AW runs policies around native Agent tool calls. An Adapter receives native
callbacks, the local service manages bindings and events, the Host invokes
Providers, and the Adapter returns results to the Agent. The Agent owns model
calls, tool scheduling, and permission approval.

## User workflow

![AW user workflow and component architecture](../../../images/aw/architecture.png)

This view follows user actions; arrows show runtime collaboration. The green
area is the Provider contribution space. See the [Provider guide](providers.md)
for independent development and distribution.

## Crate relationships

The AW framework contains eight crates under `crates/`. Concrete Providers are
maintained separately under `providers/`.

| Crate | Responsibility | Direct AW dependencies |
| --- | --- | --- |
| `aw-service` | `aw` CLI, native Adapters, local service, and client | config, contracts, core, exec, host, provider |
| `aw-host` | Prepare Providers and execute admitted Hook steps | config, exec, provider |
| `aw-provider` | Validate Provider messages and capability admission | config |
| `aw-exec` | Bound process lifetimes and output; clean up owned processes | None |
| `aw-config` | Parse configuration and validate fields and static references | None |
| `aw-core` | Capability plan executor, Host/Journal ports, and FileJournal | contracts |
| `aw-contracts` | Schemas, canonical encoding, and cross-record validation | None |
| `aw-package` | `aw-build` builds packages; `aw-package` installs and configures them | config, exec |

The dependency column omits the `aw-` prefix. Tests in `aw-package` also depend
on `aw-service`.

### Independent Providers

| Cargo package and binary | Directory | Responsibility | Framework dependencies |
| --- | --- | --- | --- |
| `aw-provider-sec-core` | `providers/sec-core/` | Bridge the sec-core CLI for security checks | exec, provider |
| `aw-provider-tokenless` | `providers/tokenless/` | Bridge the Tokenless CLI for output compression | exec, provider |

These implementations currently share the framework's Cargo workspace for builds
and tests, but connect as separate processes and are not part of the eight framework
crates. Tokenless Provider tests also depend on the sec-core Provider. Other teams
can develop and distribute Providers independently; see the [collaboration guide](providers.md).

## One Hook request

1. The Adapter in `aw run` checks the native entrypoint, registers callbacks, and
   starts the Agent.
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

### Where `aw-core` fits

The current Hook path is `aw-service → aw-host → aw-exec`. The service reuses
`aw-core::journal::FileJournal` for audit storage, without calling
`Core::prepare/execute`.

`aw-core::Core` is a separate capability-plan API. Its embedding caller supplies
`ProviderHost`, `Clock`, and `Journal`; it uses `aw-contracts` to validate plans
and execution records. `aw-host::Host` and this `ProviderHost` port have separate
contracts and are not interchangeable call layers.

## Directories and source entry points

`src/aw/Cargo.toml` defines only the workspace. Eight framework crates live in
`crates/`; two concrete Providers live in `providers/`, each with its own source
and tests. The former root `src/` contracts code now lives in `crates/aw-contracts/`.

`providers/` is a collaboration space for first-party and community implementations.
Providers can also ship independently and connect through the process protocol.
See [Provider development and distribution](providers.md) for directory conventions
and packaging scope.

| Path, relative to `src/aw/` | Content |
| --- | --- |
| `crates/aw-service/src/main.rs`, `crates/aw-service/src/cli/` | CLI, launcher, and native Adapters |
| `crates/aw-service/src/server.rs`, `crates/aw-service/src/runtime/` | Local service and event execution |
| `crates/aw-host/src/lib.rs`, `crates/aw-host/src/event.rs` | Provider preparation and invocation |
| `crates/aw-core/src/journal.rs` | Durable audit storage |
| `crates/aw-contracts/src/registry.rs` | Contract schemas embedded in the program |
| `providers/sec-core/`, `providers/tokenless/` | Concrete Providers, configuration examples, and tests |
| `adapters/` | Packaged Python/JavaScript native bridge assets |
| `scripts/`, `tests/` | Workspace checks, cross-language vectors, and native acceptance |
| `docs/design/` | Protocol contracts, boundaries, and design rationale |

Follow the [design index](../../../../src/aw/docs/design/README.md) in call order
for details. See [Contributing](../../../../src/aw/CONTRIBUTING.md) for the
development environment and checks.
