# Local service demo

[中文版](../../zh/aw/local-service.md)

Run a local daemon and synthetic tool events to inspect socket communication and audit records.

## Run the local service demo

This demonstration needs no Agent account or model request. From `src/aw`:

```bash
cargo build --locked -p aw-cli --bin aw
cargo build --locked -p aw-provider --example policy
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw service serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

In a second terminal in `src/aw`, use the printed absolute socket path:

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- "$AW_DEMO_SOCKET"
```

The example supplies synthetic capabilities and events. It checks `read_demo`,
blocks `delete_demo`, observes an after event and prints audit keys. No native
Agent or tool is started. Replace `AUDIT_KEY` with a reported preparation key or
event ID to inspect its records:

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

`terminal: false` means no terminal record is present; the operation may be
active or interrupted. A timed-out call must not be retried as a new step.
The stop response acknowledges the request; wait for the foreground service to
exit before removing this demo directory in its original terminal:

```bash
rm -r -- "$AW_DEMO_ROOT"
```

Normal shutdown retains audit history and removes the owned socket. After a
forced kill, AW reports a stale socket instead of deleting it automatically.
Verify that the old service has stopped before removing its owned `aw.sock`;
keep its lock and journal when retaining the service directory. Restarting
creates a new service identity; historical records remain queryable without
resuming or replaying old events.

The [configuration reference](configuration.md)
covers every field and event name. The [local service contract](../../../../src/aw/docs/design/en/runtime/local-service.md)
describes operation JSON, deadlines and lifecycle for Adapter developers.

Connect sec-core scanning through `aw-provider-sec-core`; see the [Provider guide](../../../user-guide/en/aw/security.md).

Model-visible PostTool compression uses the optional [Tokenless Provider](../../../user-guide/en/aw/tokenless.md).
