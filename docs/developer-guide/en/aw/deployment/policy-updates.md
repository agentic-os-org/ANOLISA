# Update instance policies

[中文版](../../../zh/aw/deployment/policy-updates.md)

The deployer pauses and resumes task dispatch; AW activates a complete policy after confirming the instance is idle. Configure a supported worker with its [Agent guide](../native-agents.md) first.

## Select a runtime mode

| Mode | Readiness | Wait before updating |
| --- | --- | --- |
| Ordinary Qoder SDK 1.0.56 / Worker 1.1.78 | Managed `startDaemon()`; without an explicit daemon, initialize `createQuery()` within 30 seconds | All Queries fully closed, callbacks and leases cleaned up |
| Ordinary QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | Managed App startup | All replies and cleanup completed |
| Qoder 1.1.73 TUI and the SDK / App versions above in `--reloadable` mode | Native stable bridge; SDK also needs an explicit daemon and initialized Query | All native requests and background cleanup completed |
| Externally started Qoder TUI / QwenPaw App | [Attach before the first request](process-attach.md) | Same restricted bridge boundary |

Ordinary Qoder SDK and QwenPaw retain strict Post, managed MCP and private execution leases. Restricted mode does not provide resident strict Post or private leases.
Other Adapters require restart. An ordinary Qoder SDK Query occupies its instance between turns until closed; prewarmed workers should call managed `startDaemon()` first.

## Apply a candidate policy

1. Pause new tasks for the target instance and wait for the cleanup boundary above.
2. Prepare a candidate. Change only `providers`, `events` and `execution.default_event_budget_ms`; retain launch, audit and evidence fields.
3. Validate, apply and query the active revision. Replace `TARGET` with the configuration target name.

```bash
aw validate --config next.yaml --against current.yaml
aw update --config current.yaml --agent TARGET --policy next.yaml
aw status --config current.yaml
```

Success returns `policy_revision`, the previous revision, audit key, preparation and activation durations. Resume dispatch after confirmation; each new task receives a complete policy snapshot.
Add `--instance ID` when a target has multiple instances. Sessions share the instance policy; task child scopes cannot be updated separately.

Alternatively, edit `current.yaml` and run `aw update --config current.yaml --agent TARGET`.
AW records the service identity connected to that path; files are not watched. `--policy` rewrites neither file, so put the full desired policy into the main file before a later single-file update.

## Failures and recovery

| Result | Action |
| --- | --- |
| `busy` | Old policy remains active; explicitly retry after requests and cleanup finish |
| Preparation failure, revision conflict or unsupported capability | Correct the candidate or query the new revision before deciding to retry |
| Timeout or uncertain outcome | Query the active revision before dispatching work |
| Audit persistence failure after activation | Stop the affected worker, fix the cause, and restart service and worker with the desired configuration |
| Unconfirmed cleanup, persistently busy | Stop the worker and recreate the instance |

Updates are not queued. Keep updates and task dispatch serialized by one controller after success.
Restore an old policy by submitting its complete configuration through the same flow. Changes to AW, SDK, model credentials or launch fields require [redeployment](lifecycle.md).

## Controller integration

```bash
aw update --socket "$AW_SOCKET" --instance "$AW_INSTANCE" \
  --policy next.yaml --expected-revision "$AW_POLICY_REVISION"
```

`--expected-revision` sets a concurrency precondition; conflicts do not retry automatically. The CLI has no `aw update --pid` selector.
Read-only containers must share one absolute `XDG_RUNTIME_DIR` across launch, update, status and stop, owned by the runtime user with mode `0700`.
Connection metadata lives in `$XDG_RUNTIME_DIR/aw/connections`, or `/tmp/aw-UID/connections` when unset. Retain this runtime mount until both worker and service stop.

See the [update contract](../../../../../src/aw/docs/design/en/runtime/policy-reload.md) for native busy confirmation, task snapshots and audit ordering.
