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
[configuration tool reference](../../../developer-guide/en/aw/packages.md#edit-existing-provider-and-hook-configuration).

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
AgentScope 2.0.9 App. For QwenPaw, use its configured target and initialized
`QWENPAW_WORKING_DIR` as in the native integration reference. Ordinary `aw run`
keeps its existing behavior. An instance started without the stable bridge
cannot be upgraded by this command; external attach is a later capability.

Prepare `next.yaml` separately. In another terminal, update the target using the
original launch configuration to locate its existing service:

```bash
aw update --config "$AW_HOME/aw.yaml" --agent qoder --policy next.yaml
```

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
`preparation_ms` and `activation_ms`. The command does not edit either file,
restart the native host or start a service. Keep the original launch file unchanged
for `aw update`, `aw status` and `aw stop`: auto service paths use its bootstrap
digest, not the candidate policy digest. New bindings still use the service's
original default; editing that file in place does not retarget a running instance.

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
