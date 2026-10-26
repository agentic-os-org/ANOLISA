# Configuration and Agent launch

[中文版](../../zh/aw/configuration.md)

`aw.yaml` contains Agent entrypoints, enabled Providers and runtime data locations.
Model accounts remain in the Agent's own configuration. Complete [installation](installation.md) first.

## Generate configuration

Reuse the installation variables. `config init` does not overwrite files; see the next section to edit one.

```bash
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
```

| Capability | Add to the generation command |
| --- | --- |
| Launch the Agent only | No Provider arguments |
| Compress tool output | `--provider tokenless` |
| Check tool safety | `--provider sec-core --socket /run/agent-sec-core/daemon.sock` |
| Safety checks and compression | `--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock` |

Install the selected Providers first; the generator supplies executable paths and integrity metadata.

## Edit existing configuration

Exit Agents using the file and stop the service with the original configuration before editing.
Configuration files are not watched. For an explicitly reloadable instance, use
the idle-update workflow below.

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw config show --config "$AW_HOME/aw.yaml"
```

Edit YAML directly or use `aw config` to add and remove Providers and Hooks.
The file and its parent must belong to the current user and must not be writable by others.
The editor requires an absolute path and rejects symlinks. `show` includes private configuration
values; remove sensitive data before sharing its output.

```bash
aw config validate --config "$AW_HOME/aw.yaml"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Successful validation prints `configuration valid`. It checks fields and references;
program availability is checked at launch. Write actual absolute paths in YAML;
AW does not expand variables such as `$HOME`.
For all editor commands and a custom rule example, see the
[configuration tool reference](packages.md#edit-existing-provider-and-hook-configuration).

## Select tools for a Hook step

Set `match.tools` on an existing step to limit which tools invoke its Provider.
With a declared `security` Provider and Agent targets using the `qoder` and
`openclaw` Adapters, this event fragment checks Qoder `Bash` and OpenClaw `exec`;
other tools skip the scan step.

```yaml
tool.before:
  enabled: true
  required: true
  match: {tools: ["*"]}
  steps:
    - id: scan-code
      provider: security
      operation: scan_code
      match: {tools: ["native:qoder:Bash", "native:openclaw:exec"]}
      effects: [observe, block]
      on_error: block
```

The event and step selectors must both match. Omitting either selector, or using
`["*"]`, selects all tools at that level. Standard names `bash`, `file_read` and
`file_write` match the Adapter's normalized tool name. Exact
`native:<adapter>:<name>` selectors use the Adapter ID and the Adapter-retained
`event.tool.native_name`, with case-sensitive matching. Each native selector's
Adapter ID must appear in `spec.agents[*].adapter`. Validation uses the union of
all configured targets, so Qoder and OpenClaw selectors can share an event or
step when both Adapters are declared. Lists are nonempty and unique; `*` must
stand alone. Regex and glob patterns are unsupported.

For `tool.after`, OpenClaw normalizes `bash` to `exec`, `apply-patch` to
`apply_patch`, and `cron` to `automations` in `event.tool.name`;
`event.tool.native_name` preserves the
callback's supplied spelling. When result replacement and completion observation
arrive separately, AW restores the original name saved in the replacement
snapshot. A standalone completion uses the name supplied by its own callback.

An unmatched step records `kind: skipped` without invoking the Provider. Other
steps keep their order and selection, so an observer or compression step can
still run. Provider preparation and configuration checks still apply at launch.
Native permission prompts and denials remain in force. Configure sec-core's
input mappings separately; see [security checks](security.md).

## Set the Agent shutdown grace period

Set `spec.agents.<target>.shutdown_grace_ms` in the selected Agent's configuration
when its shutdown handler needs more time to close requests and SDK clients.
The value is an integer from `1` to `120000` milliseconds; omitting it retains the
default `2000` milliseconds. For example, `shutdown_grace_ms: 15000` gives the
Agent process group a 15-second grace period from when the foreground launcher
first forwards an external termination signal. When the grace expires, AW starts
forced cleanup. Repeated termination signals do not restart the grace period.
Time spent before signal forwarding and in final process cleanup is outside this
interval; this field is not a deadline for the entire shutdown operation.

This setting does not change the two-second grace before forced cleanup for
internal readiness or lifecycle failures. Cleanup of a naturally exited Agent's
remaining child processes starts immediately. It does not change `aw stop` service
shutdown or the lifetime of independently started Agents attached with
`aw agent connect`.

The application must stop accepting requests, drain work and close SDK clients.
Increasing this grace period does not perform that work or guarantee that SDK
cleanup finishes. Coordinate the application's deadlines and the container or
supervisor's termination budget with this value, and verify shutdown in the
deployment image.

Changes take effect only for a newly started launcher; policy updates cannot
change this field. Older AW binaries reject configurations containing it. Before
rolling back, remove the field and account for the restored two-second default.

## Compare policy edits

Prepare a separate candidate file and compare it with the existing configuration:

```bash
aw validate --config next.yaml --against current.yaml
```

Both files are validated. The comparison permits changes only to Providers,
events and the default event budget. Agent entrypoints, native scheduling,
service paths, audit, observation and metadata must remain identical.
Success prints `policy update scope valid; runtime activation is not checked`.
It does not run Providers, modify files, check native readiness or update a
running Agent. Use explicit idle updates below for a reloadable instance;
otherwise use the stop/edit/run workflow above.
Plain `aw validate --config FILE` still prints `configuration valid`.

`aw config validate` is equivalent to `aw validate`, including `--against` and
relative file paths; it does not enforce the editor's ownership checks. The
standalone `aw-package` command, including its stricter editor validator, has
been removed. Continue using `aw config init`, `aw config show` and the editing
commands; their file-safety checks do not make `aw config validate` a replacement
for that removed validator.

## Register native Hooks

For Agents with persistent native registration, use
`aw agent connect --config FILE --agent TARGET` and any required native options,
such as Hermes `--native-profile`. It uses the existing Adapter registration and
support checks; it does not install the Agent or software packages.
The old `aw install --config FILE --agent TARGET` syntax remains compatible.
Combining native-registration options with `--prefix` or `--bundle` is rejected.

## Update a prewarmed instance

To retain a warm host while changing AW Hooks, opt in when launching it:

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder --reloadable
```

This runtime path is pinned to Qoder 1.1.73 TUI or QwenPaw 2.2.2b4 with
AgentScope 2.0.9 App, and Qoder SDK 1.0.54 / bundled Worker 1.1.74.
For SDK targets, use the [explicit daemon and Query factories](../../../developer-guide/en/aw/native-agents.md#start-a-qoder-sdk-worker);
all managed sessions in the instance must be idle, including native cleanup.
For QwenPaw, use its configured target and initialized
`QWENPAW_WORKING_DIR` as in the native integration reference. Ordinary `aw run`
keeps its existing behavior. An instance started without the stable bridge
cannot be upgraded by this command. For a caller-owned native process, use the installation flow below.

Keep editing the same `aw.yaml` after launch or connection. In another terminal,
explicitly apply its complete desired policy to the existing instance:

```bash
aw update --config "$AW_HOME/aw.yaml" --agent qoder
```

AW remembers the service identity selected for that configuration path. Editing
or atomically replacing the file does not change that connection, restart the
Agent, or activate a policy. Busy, validation failure or conflict leaves the
previous policy active; the file retains your desired edits for an explicit retry.
Keep launch fields unchanged and use `aw service status --config` to inspect the
active revision. Keep the same file path and runtime directory for these commands.

For a `qoder-sdk` target, use the same update flow with `--agent sdk` when its
configured name is `sdk`. All managed Queries share the instance policy;
requests, background-task cleanup and Query close keep updates busy until native
cleanup completes. A relaunch with edited bytes is rejected while the recorded
service is live. Close the worker and stop that service before launching again.

The separate-file form remains supported:
`aw update --config aw.yaml --agent qoder --policy next.yaml`. It applies
`next.yaml` without writing either file. For later single-file updates, first put
the complete desired policy in `aw.yaml`; AW never merges an update as a patch.

The command selects the target's unique live instance and reads its current
policy revision. That revision remains an optimistic concurrency check: if a
concurrent update wins, the command fails with a conflict rather than retrying.
No matching instance fails; multiple matching instances require `--instance ID`.
Use `aw status --config "$AW_HOME/aw.yaml"` to inspect instance IDs and revisions.
Selection includes unready and nonreloadable instances; it never silently chooses
an idle instance over a busy one.

For explicit instance selection or an externally managed control plane:

```bash
aw update --socket "$AW_SOCKET" --instance "$AW_INSTANCE" \
  --policy next.yaml --expected-revision "$AW_POLICY_REVISION"
```

`--socket` replaces `--config`; `--instance` can also disambiguate `--agent`.
`--expected-revision` is optional and overrides the observed revision when the
caller needs a specific precondition. There is no automatic retry on conflict.

Success returns the actual active `policy_revision`, previous revision, audit key,
`preparation_ms` and `activation_ms`. The command does not edit configuration,
restart the native host or start a service. The saved connection keeps `aw update`,
`aw status` and `aw stop` addressing the original service after policy edits.
Starting another reloadable launch with an edited file while that service is live
is rejected before starting a new daemon; update existing instances, or stop the
service before launching from the edited configuration. New bindings initially
use the service's default; native attach applies the current desired policy
before reporting success.

A missing lifecycle acknowledgement or unavailable active-plan transport blocks
`tool.before` in reloadable mode. Once a plan is resolved, each step keeps its
configured report/block failure action.

An active Agent request, even while waiting for a model with no tools running,
returns `busy` and leaves the old policy active. No update is queued or applied
later automatically. Failed preparation, revision conflicts and unsupported
native policies also leave the old policy active. After an indeterminate timeout,
query the active revision before dispatching work. The control plane must own
exclusive instance assignment and serialize updates with Session dispatch;
success is not a reservation for a future request. Multiple Sessions in one
App share the instance policy, and any active request prevents switching.

Apply policy changes before creating or dispatching the first user Session.
Model accounts and native Agent definitions stay with the framework's own
Session configuration. Updating those fields through AW still requires another
launch. For Qoder, native sequential tool hooks also require `qoder.sequential: true`
in the AW target so dispatch retains the same scheduling. Qoder `Stop` is not an idle signal; reload waits for its native idle-prompt
notification. QwenPaw releases request/tool occupancy on normal stream exhaustion.
Early stream closure, cancellation or exceptions before confirmed completion keep the instance busy because
the native cleanup cannot be confirmed; use a fresh instance in that case.
AW delivers final responses and terminal error/interrupted chunks after native
cleanup completes. A normally returned structured tool error therefore permits
later updates.

This Preview has CLI seam and pinned AgentScope middleware regressions. The native inventory also requires a checksummed Qoder 1.1.73 TUI startup and
pre-dispatch idle-switch case. Required QwenPaw App HTTP tests use a deterministic
local model and real shell/read_file tools to verify repeated policy switches
after successful and structured-error results in one process.
Full hosted Qoder TUI tool-turn adoption still requires separate acceptance.

## Choose an Agent

A configuration can contain multiple targets. `--agent` selects a name under `spec.agents`.
The generator currently supports Qoder and OpenClaw. For QwenPaw, Hermes and SDK applications,
use the [native integration reference](../../../developer-guide/en/aw/native-agents.md).

| Agent | Current requirements | Entrypoint |
| --- | --- | --- |
| Qoder | CLI 1.1.64 or later with native Hooks available | Interactive or print |
| OpenClaw | 2026.9.6 with initialized native configuration and state | A newly launched Gateway |
| QwenPaw | 2.2.2b4 with AgentScope 2.0.9 | App/API |
| Hermes | Follow the [compatible version and setup](../../../developer-guide/en/aw/native-agents.md#start-hermes) | Local chat |
| AgentScope, Pi, Claude SDK | Explicit application wiring required | [SDK integration reference](../../../developer-guide/en/aw/native-agents.md) |

### Qoder

Launch from the intended working directory. Arguments after `--` go to Qoder.

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

For an additional settings file, use AW's `--native-settings /absolute/qoder.settings.json`.
AW preserves existing settings and adds callbacks for this run.

### OpenClaw

Stop any Gateway using the same native profile first. Replace these variables with absolute
paths to your existing installation and profile.

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

Wait for `AW openclaw native hooks ready`, then use a client connected to that Gateway.
Add the Provider arguments above when generating configuration for compression or safety checks.
`agent --local` bypasses this Gateway.

## Add your own rules

A simple rule reads a tool event from stdin and writes JSON `true` or `false` to stdout.
Configuration chooses observation or blocking when the rule matches. `on_error` controls execution
failures; safety checks typically use `block`. Follow the
[boolean rule tutorial](../../../developer-guide/en/aw/integration.md) to prepare a script and verify
both allowed and blocked calls.

Resident Pi/Claude targets retain this configuration format. Select the resident entrypoint and public factory described in the [SDK worker reference](../../../developer-guide/en/aw/native-agents.md#resident-pi-and-claude-sdk-hosts).

## Attach to an already started native process

A container controller can start Qoder 1.1.73 TUI or QwenPaw 2.2.2b4 /
AgentScope 2.0.9 App normally, then install AW before dispatching the first request.
The controller owns the process, its terminal and its lifetime. Record its PID
when starting it and keep request dispatch paused until installation succeeds.

These exact versions are both the supported set and the tested builds for this
Preview's attach/update path. A wider compatible range has not been established;
the Qoder minimum for ordinary `aw run` does not extend this path's support.
Additional versions need native adoption, full-request idle and removal contract
validation before admission can be broadened.

| Native entrypoint | Attach/update support in this Preview |
| --- | --- |
| Qoder TUI | Supported on 1.1.73, with ConfigChange confirmation before input and native idle-prompt acknowledgement after a request |
| Qoder single `-p` / `--print` execution | Ordinary `aw run` supports print; attach/update does not admit this mode |
| Qoder persistent headless input | Not admitted or validated; pre-input readiness, repeated requests and request/idle boundaries need separate validation |
| QwenPaw `app` | Supported on 2.2.2b4 / AgentScope 2.0.9; this is the HTTP backend and can be driven headlessly through its API without opening a UI |
| QwenPaw ACP / TUI | Not supported; the pinned TUI uses an ACP subprocess rather than the App's HTTP plugin-management endpoint |
| QwenPaw `desktop` | Not a supported attach target; its UI starts a separate App through `python -m qwenpaw`, whose entrypoint/process ownership is outside this release's direct `qwenpaw app` validation |

For QwenPaw container prewarming, start `qwenpaw app` directly with its initialized
profile and loopback listener. The real attach acceptance uses HTTP requests and
requires no desktop or browser session.

Qoder SDK `startDaemon()` and its Worker/session controls are a separate
entrypoint from TUI. PID attachment does not provide per-session policy isolation;
SDK daemon support requires its own native lifecycle and isolation acceptance.
The supported QwenPaw App build does not establish compatibility with older releases.

```bash
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID"
aw update --config current.yaml --agent TARGET
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID" --remove
```

`aw install --config ... --agent ... --pid ... [--remove]` remains a compatible
entrypoint for the same operation.

To add Hooks in stages, edit the same complete desired configuration, retaining
earlier Providers and steps. Existing `aw config add-provider` and
`aw config add-hook` accept `--definition`; see the
[configuration tool reference](packages.md#edit-existing-provider-and-hook-configuration).
These are offline edits. Validate the file, then explicitly activate it with
`aw update` while idle. No separate bootstrap file needs to be maintained. A later
attach with the same path applies its current desired policy before reporting success.
The initializer currently generates Qoder/OpenClaw configurations; prepare a
QwenPaw target using the native integration reference linked above.

The connection command reports the instance, active policy revision and
process identity after native confirmation. Subsequent policy changes use the
same `update` command as reloadable `aw run`. `--remove` checks the complete native
request/tool boundary; busy returns an error without removing protection or
queuing work. It leaves the native process alive and preserves unrelated Hooks
and configuration edits. Keep request dispatch paused during removal as well.
Ordinary `aw run` and Hermes installation keep their existing behavior.

Use one exclusive native configuration scope for each prewarmed process. AW and
the Agent must run as the same user in the same PID and mount namespaces. The
selected native configuration directory must be canonical, owned by that user
and mode `0700`. Known shared processes and duplicate attachment are rejected;
the controller must also prevent another process from entering that scope later.

- Qoder uses `HOME/.qoder/settings.json`; start without `--settings`, custom
  configuration directories or Hook-disable options. AW reads the running
  process's environment and arguments, merges its own groups, and confirms them
  through native `ConfigChange` before input. Existing settings must be regular,
  same-user files without group/world write permission. If settings were absent,
  AW creates the native file and retains the empty document after removal.
  Native workspace trust and permission decisions remain with Qoder.
- QwenPaw uses the running App's initialized `QWENPAW_WORKING_DIR`. Start the
  App listening on `127.0.0.1`; AW verifies that the selected PID owns its port.
  The native plugin API installs the existing AW middleware with a private
  descriptor. An acknowledgement from that App is required; an HTTP success
  alone is insufficient. Separate managed runtimes are not supported.
  Start co-resident Apps with an explicit absolute `QWENPAW_WORKING_DIR` in
  their initial environment. When native `.env` loading could select a peer's
  profile or its HOME expansion, AW reports that PID and rejects attachment.
  A currently absent `.env` does not prove the directory used at startup;
  restart that peer with an explicit profile or stop it before attaching.

QwenPaw also requires the same network namespace as AW. If its installed plugin
tree is missing, detach queries the native runtime status. Confirmed absence
permits cleanup. A still-loaded plugin or unavailable status retains ownership,
staged plugin and bridge files for retry. Restore the owned tree if still loaded,
or retry after the API recovers, before dispatching further requests.

First attachment requires a quiescent process before its first request. AW
cannot reconstruct work already in flight before its bridge existed. Remove
attachment before stopping the external process; this first release does not
supervise that process or run an automatic attachment garbage collector. A
failed removal retains its ownership record and bridge files for an explicit
retry with the same command. A replaced native plugin or service generation is
rejected for inspection, rather than removing another owner's resources.
Qoder rollback also waits for native removal confirmation. If cancellation or a
failed installation cannot confirm unload, AW retains its ownership and bridge
files for `--remove` after native callbacks recover. Moving an AW group to another
Hook event is treated as an ownership edit and must be restored before removal.

An unlocked empty ownership claim left by a terminated installer can be reused
by connect or removed with `--remove`; a live installer's lock is never stolen.
Hard termination is not transactional recovery: SIGKILL can leave local staging
files, and interruption after a successful Bind reply but before its durable
record can leave an unrecorded service binding. The owning supervisor must retire
the affected service and its private state in that case. This is separate from
normal cancellation/failed-install rollback; this Preview has no crash-recovery
journal or automatic garbage collector.

## Concurrent service budgets

Resident Pi/Claude clients retain their conversation across executions and reclaim
completed Hook state. Configure resource admission independently of historical calls:

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

These are defaults; omitted fields keep their default. Each accepts an integer
from 1 through 4096. `bindings` includes hosts and preparing, idle or draining
clients; `events` counts live Hook events, `calls` active Provider invocations,
`connections` all live service connections including control RPCs, and
`invocations_per_binding` active model-produced tool calls for one resident client.
Once the shared connection limit is reached, sixteen additional slots admit only
control/cleanup requests. Overload rejects new admission; completed
work returns capacity. Lifetime tool count has no configurable quota in resident
Pi/Claude mode. Legacy entrypoints keep their retained-correlation limits.

Use `aw status --config aw.yaml` to inspect `resources.limits`, `resources.usage`,
`peak_invocations_per_binding`, `admission_rejections` and `rejection_reasons`.
Select concurrency from actual load measurements; audit disk and SDK history need
separate retention. Apply daemon limits by draining clients and restarting the
service. Upgrade or roll back CLI/daemon together after drain and stop: wire
v1alpha7 rejects older live sessions. Configuration/Provider versions are unchanged.
