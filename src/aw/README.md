# AW

[中文版](README_zh.md)

AW provides a shared configuration and local service for Agent policies. On
Linux, it starts Qoder CLI and OpenClaw through verified native entrypoints,
runs external Providers and keeps execution metadata independently of an Agent
session. Native scheduling and permissions remain with the framework; the
current interfaces are experimental.

## Available today

| Capability | Availability |
| --- | --- |
| Validate one `aw.yaml` with named Providers and all 16 event names | ✅ |
| Start Qoder CLI 1.1.64 and connect before/after tool Hooks | ✅ Linux source build |
| Run structured Providers before tools (`observe`/`block`) and after successful tools (`observe`) | ✅ |
| Execute native Hook commands with unchanged callback input | ✅ Byte output and exit status returned to Qoder; rewrite chains and approval flows excluded |
| Start or reuse a standalone service and query execution metadata | ✅ |
| Start OpenClaw through AW | ✅ a new Gateway with Agent tool hooks |
| Start the remaining first-release frameworks | ❌ Separate adapter delivery |
| Install a published AW package, request portable approval or enforce policy below native Hooks | ❌ |

## Run Qoder

AW is not yet available through `anolisa install` or an RPM. On Linux, install
rustup and Qoder CLI 1.1.64, then build from the repository root. Update the
example's `spec.agents.qoder.argv` if that Qoder version is outside `PATH`.

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

The example runs a neutral command before and after successful tools. It
demonstrates Hook execution; it does not install security rules. AW starts or
reuses the configured service and opens Qoder's normal interface. Exiting Qoder
returns to the original terminal and releases that session; the shared service
and audit history remain available.

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

The [user guide](../../docs/user-guide/en/user-entrypoint/aw.md) explains native
settings coexistence, serial/parallel Hooks, Provider configuration, explicit
service startup and record queries. Native Hooks retain their framework's
limits; service status alone does not prove that Qoder adopted a policy.

`aw --help` lists the launcher commands and adapter-specific options. Qoder
accepts `--native-settings` with `aw run`; it rejects `--native-profile`,
`--native-state-dir` and `aw install`. The `install` command dispatches persistent
native Hook setup, with no supporting adapter in this build; it does not install
AW or an Agent. See the user guide for the command and option support table.

## sec-core Provider

The Linux `aw-provider-sec-core` binary maps configured tool inputs to the public
`agent-sec-cli scan-code` command. It supplies candidate before-tool observe/block
and after-tool observation through the AW Provider protocol. It requires an
existing sec-core CLI and daemon; no AW code is installed inside sec-core.
See the [sec-core Provider guide](../../docs/user-guide/en/user-entrypoint/aw-sec-core.md)
for source builds, configuration and a local Host example.

## Run OpenClaw

Use OpenClaw 2026.9.6 with its existing native model configuration.
The [user guide](../../docs/user-guide/en/user-entrypoint/aw.md) explains the
required profile options and supported tool lifecycle. From `src/aw`:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.openclaw.yaml --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

## Integration and development

The reusable `aw-service::Client` binds to one service generation and configuration
revision. Adapters normalize callbacks and retain native scheduling. Related
callbacks share one event deadline, and each step may be attempted once. The
service records execution metadata before returning results and never retries
an uncertain call automatically.

`aw-host` supports both structured Provider messages and explicitly selected
native Hook byte transport. `aw-core` provides a separate pinned-plan execution
API and the durable `FileJournal` storage reused by the service. These boundaries
leave cosh, desktop clients and Herdr independent of the service implementation.

- [User guide](../../docs/user-guide/en/user-entrypoint/aw.md) and
  [configuration reference](../../docs/developer-guide/en/aw/configuration.md)
- [Local service and client contract](docs/design/local-service.md)
- [Provider protocol](docs/design/provider-protocol.md),
  [Provider Host](docs/design/provider-host.md) and
  [bounded command execution](docs/design/bounded-execution.md)
- [Core execution and storage](docs/design/core-execution.md)
- [Development setup, crate boundaries and tests](CONTRIBUTING.md)
