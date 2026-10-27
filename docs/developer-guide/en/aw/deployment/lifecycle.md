# Service and container lifecycle

[中文版](../../../zh/aw/deployment/lifecycle.md)

## Startup and persistence

Install matching AW, Providers and pinned Agent versions into the image. Prepare native profiles and model credentials, then launch the application with `aw run`.
Install Hermes plugins into the actual runtime profile. Validate [native behavior](../validation.md) before accepting application traffic.

| Resource | Deployment |
| --- | --- |
| Binaries, scripts, default configuration | Image or read-only mount |
| Editable configuration | Store outside the disposable container layer |
| `daemon.state_dir` | Writable absolute path, runtime UID ownership, mode `0700`; parent owned by that UID and protected from other writers |
| `state_dir/journal` | Persist when history is needed; stores call metadata only |
| Socket, locks, `launch-*`, bindings | Recreate each run; cannot restore sessions |
| Native profile, workspace, backend data | Managed independently by framework or Provider |

`endpoint: auto` with explicit state_dir uses its `aw.sock`. With both fields auto, use `XDG_RUNTIME_DIR`, falling back to `/tmp/aw-UID`.
Give each independent daemon a separate state directory. Persistent volumes must support `fsync`.

`startup: on_demand` starts the service when needed; `external` requires an existing service. Use foreground service startup with an external supervisor.

```bash
aw service serve --config /absolute/aw.yaml --state-dir /absolute/private/state
```

Service reuse requires matching protocol and startup configuration. After edits, [update the policy](policy-updates.md) or stop the old service before restarting.

## Stop and upgrade

Stop dispatch, drain tasks and close SDK clients, then exit the Agent and run `aw stop --config FILE`.
Wait for the recorded daemon PID to exit and its socket to disappear. After forced termination, remove only its stale `aw.sock` once exit is confirmed; retain journal.

CLI and daemon use `aw-service/v1alpha7`. Stop old services with the old binary before upgrading or rolling both binaries back together.
Configuration and Provider protocols remain `aw/v1alpha1` and `aw-provider/v1alpha1`. Use new state directories for Preview cross-version upgrades; retain old configuration and data for rollback.

## Set the Agent shutdown grace period

`spec.agents.<target>.shutdown_grace_ms` accepts 1..120000 ms, default 2000.
For example, `15000` gives the launcher 15 seconds after first forwarding an external shutdown signal before forced cleanup of its owned process group. Repeated signals do not extend the timer.

This controls foreground launcher shutdown by external signal. Internal readiness/lifecycle failure retains a two-second grace; natural exit immediately cleans up remaining children.
`aw stop` and externally attached processes have separate lifecycle controls. Coordinate application drain deadlines with the container shutdown budget.
Changes require launcher restart. Old AW versions reject this field; remove it when rolling configuration back.

## Concurrent service budgets

Pi/Claude resident paths reclaim completed Hook state and admit active resources. These defaults each accept 1..4096.

```yaml
spec:
  daemon:
    limits:
      bindings: 64
      events: 128
      calls: 64
      connections: 128
      invocations_per_binding: 256
```

| Field | Counted resource |
| --- | --- |
| bindings | Hosts and preparing, idle or draining clients |
| events | Live Hook events |
| calls | Active Provider invocations |
| connections | Active service connections including control RPC; 16 additional connections reserve control and cleanup access |
| invocations_per_binding | Active model tool invocations for one resident client |

Inspect `resources.limits`, `resources.usage`, `peak_invocations_per_binding`, `admission_rejections` and `rejection_reasons` in `aw status`. Drain and restart the service after changing limits.
Legacy task entrypoints and restricted Qoder SDK retain the 1024 native event association limit per instance; restart the instance on `native_event_limit`.
Set audit disk and SDK history retention separately. See the [resident SDK contract](../../../../../src/aw/docs/design/en/adapters/resident-sdk.md).
