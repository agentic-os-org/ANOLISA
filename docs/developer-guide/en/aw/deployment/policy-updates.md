# Update instance policies

[中文版](../../../zh/aw/deployment/policy-updates.md)

The deployer pauses and resumes task dispatch; AW activates a complete policy after confirming the instance is idle. Configure a supported worker with its [Agent guide](../native-agents.md) first.

## Select a runtime mode

| Mode | Readiness | Wait before updating |
| --- | --- | --- |
| Ordinary or prepared native Qoder SDK 1.0.56 / Worker 1.1.78 | Managed `startDaemon()`; without an explicit daemon, initialize `createQuery()` within 30 seconds | All Queries fully closed, callbacks and leases cleaned up |
| Ordinary or prepared native AgentScope 2.0.6 factory | First `AwBridge.create_agent()` | All replies, tool tasks and Scope cleanup completed |
| Ordinary or prepared native QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | Managed App plugin startup | All replies and cleanup completed |
| Prepared native Claude SDK resident factory | `prepare_native()` in a dedicated process group | Every client disconnected; SDK, relay and MCP cleanup confirmed |
| Prepared native Pi worker factory | `runNativeWorker()` with existing `createSession` / `runWorker` factory | All sessions closed, including preparation, abort and disposal |
| Prepared native OpenClaw Gateway | Plugin confirmation at `gateway_start` | Successful `agent_end` and drained tool callbacks |
| Prepared native Hermes local chat | Connected plugin and guarded chat startup | Completed chat turn and drained callbacks |
| Prepared native Qoder CLI (TUI or print), or Qoder TUI in `--reloadable` / external attachment mode | Installed startup Hooks confirm each process; external TUI attachment requires [attachment before the first request](process-attach.md) in an exclusive profile | All native requests and background cleanup completed |
| Qoder SDK / QwenPaw `--reloadable`, or externally attached QwenPaw App | Native stable bridge; SDK also needs an explicit daemon and initialized Query | Same restricted bridge boundary |

Follow the framework's [startup instructions](../native-agents.md) once. SDK
applications call `aw instance prepare` from their own startup and retain the
existing AW factory; OpenClaw, Hermes and QwenPaw install a startup component and
restart. Preparation creates a pending instance; native confirmation makes it
ready. A running process without these connections cannot gain Hook coverage
through a policy update. Qoder can install startup Hooks before its next native
launch; an existing TUI retains its pre-request attachment route.

Ordinary Qoder SDK and QwenPaw retain strict Post, managed MCP and private execution
leases. Restricted mode does not provide resident strict Post or private leases.
A Qoder SDK Query occupies its instance between turns until closed; prewarmed
workers should call managed `startDaemon()` first. Prepared Pi and Claude clients
also keep a fixed policy until their whole session or client closes.

Prepared OpenClaw uses event budgets of at most 12,000 ms; Hermes retains its
55,000 ms limit after the documented callback-timeout preparation. Uncertain
native completion keeps updates busy and requires restart. OpenClaw supports
Gateway runs, excluding `tools.invoke`; Hermes supports local chat, with
`input.submit` limited to quiet single-query text mode. Hermes Skill checks use
the same source selection and pinned manifest contract across policy updates. Ordinary `aw run` Pi, Claude SDK, OpenClaw and
Hermes retain their existing restart requirement. See each Agent guide for
remaining native input and result limits.

## Use one default configuration

Keep `aw.yaml` in the controller's working directory and start the service with
that configuration. After the prepared native application reports ready, pause
new work, edit the policy in `aw.yaml`, and run:

```bash
aw instance update --agent TARGET
aw status --config aw.yaml
```

Use `--instance ID` to select one of several instances. `aw instance update` uses
the same update contract as `aw update`; it does not discover or attach processes.
The startup connection and native settings remain in place for subsequent policy
changes. Changes outside the permitted policy fields require restart.

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
