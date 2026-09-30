# AW

[中文版](README_zh.md)

AW provides a shared configuration and local service for Agent policies. The Linux
service prepares external Providers, executes tool-event steps and keeps durable
audit metadata independently of an Agent or shell. Adapters supply native
capabilities, schedule callbacks and apply the returned effects. The interfaces
are experimental; QwenPaw, Qoder CLI, OpenClaw and Hermes integration is still
being built.

## Available today

| Capability | Availability |
| --- | --- |
| Validate one `aw.yaml` with named Providers and all 16 event names | ✅ |
| Run a standalone Linux service and query local execution records | ✅ Source build |
| Execute Provider steps for `tool.before` (`observe`/`block`) and `tool.after` (`observe`) | ✅ Local client API and synthetic-event example |
| Start an Agent, install its Hooks or verify native effect adoption | ❌ |
| Install a published AW package or start the service on demand | ❌ |
| Ask for approval, replace tool results or enforce policy below native Hooks | ❌ |

Service status and Provider admission do not establish that an Agent is protected.
The service returns candidate effects; the future Adapter must prove adoption.

## Run from source

AW is not yet available through `anolisa install` or an RPM. On Linux, install
rustup and build from the repository root:

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-config/examples/aw.minimal.yaml
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-config/examples/aw.minimal.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

`serve` runs in the foreground. The starter configuration contains no Providers.
From a second terminal in `src/aw`, set `AW_DEMO_SOCKET` to the absolute path
printed above, then inspect or stop this service:

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

The foreground command exits after cleanup. Audit records remain in the state
directory. After the foreground command exits, `rm -r -- "$AW_DEMO_ROOT"`
in its terminal removes only this demo directory and its audit history. The [user guide](../../docs/user-guide/en/user-entrypoint/aw.md)
includes a runnable Provider demo, command reference and restart guidance.

## sec-core Provider

The Linux `aw-provider-sec-core` binary maps configured tool inputs to the public
`agent-sec-cli scan-code` command. It supplies candidate before-tool observe/block
and after-tool observation through the AW Provider protocol. It requires an
existing sec-core CLI and daemon; no AW code is installed inside sec-core.
See the [sec-core Provider guide](../../docs/user-guide/en/user-entrypoint/aw-sec-core.md)
for source builds, configuration and a local Host example.

## Integration and development

The reusable `aw-service::Client` binds to one service generation and configuration
revision. An Adapter opens one event, invokes its steps serially or concurrently,
then closes it. All steps share the event deadline and may be attempted once.
The service persists execution metadata before returning results; it never
retries an uncertain call automatically.

`aw-host` also remains available for direct embedding. `aw-core` provides a
separate pinned-plan execution API and the durable `FileJournal` storage reused
by the service. Neither interface grants native permission or certifies adoption.

- [User guide](../../docs/user-guide/en/user-entrypoint/aw.md) and
  [configuration reference](../../docs/developer-guide/en/aw/configuration.md)
- [Local service and client contract](docs/design/local-service.md)
- [Provider protocol](docs/design/provider-protocol.md),
  [Provider Host](docs/design/provider-host.md) and
  [bounded command execution](docs/design/bounded-execution.md)
- [Core execution and storage](docs/design/core-execution.md)
- [Development setup, crate boundaries and tests](CONTRIBUTING.md)
