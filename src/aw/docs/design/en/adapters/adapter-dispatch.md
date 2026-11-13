# Native adapter dispatch

[中文版](../../zh/adapters/adapter-dispatch.md)

[Design index](../README.md) · [Architecture](../architecture.md)

The `aw` executable owns daemon discovery, instance binding, foreground process
lifetime and cleanup. Each native adapter owns version and entrypoint admission,
registration files, event normalization and the host's response dialect. These
interfaces belong to `aw-adapter`. The service adds an optional native-command
environment snapshot to `invoke_step`; existing requests may omit it.

Native dialects, callback interpretation and Python/JavaScript assets live in
`crates/aw-adapter/`. The CLI and service consume this internal library; CLI
arguments, RPC orchestration, signals and launch-directory lifetime belong to `aw-cli`.
See [Architecture boundaries](../architecture-boundaries.md).

## Launch lifecycle

An adapter prepares its native configuration and reports capabilities. The common
launcher admits the configured steps and asks the adapter to validate their native
limits before starting or reusing the service. It then generates private launch
files, binds the instance, writes an adapter-tagged callback binding and starts the
foreground Agent. Providers receive the adapter's pinned launch environment.

For DeepSeek Harness, the Adapter adds launch-scoped plugin and callback files
through native `--patch`. When `security.violation` is configured, it also adds a
Node preload to the Agent's `NODE_OPTIONS` before startup. The preload registers
a process-wide module loader Hook and checks the published agent-loop source hash
and final-result anchor. It removes only its own `NODE_OPTIONS` token after
registration, so tool Node children inherit the caller's original options;
the pinned Provider environment likewise excludes the AW preload.

On exit or failure the launcher releases its instance, invokes adapter cleanup and
removes its launch directory. Adapters own rollback for temporary files outside
that directory and must leave pre-existing profile data intact. The shared daemon
has a separate lifetime. Persistent native installation is an explicit adapter
operation; the Qoder adapter requires none and rejects `install`. Version probes
are bounded to five seconds by default; adapters can explicitly allow up to
60 seconds when their native version command performs additional startup work.

## Callback boundary

Generated callbacks pass `--adapter` and a private binding path. The selected
dialect must match the binding. The common callback performs the bounded service
exchange and passes normalized input to `OpenHookEvent`; all callbacks for the
same event reuse its original deadline and once-only step claims.

Raw commands retain stdin, stdout, stderr and exit/signal status. Their callback
process supplies the complete native environment, including profile-loaded
variables. This replaces the bound environment only for raw commands; structured
Providers keep their pinned launch context. Snapshots are limited to 4096 entries
and 1 MiB, and are excluded from event input, correlation digests and audit. Structured
Provider effects and configured failures are translated by the adapter. A neutral
reply never grants native permission. Registration order and scheduling remain
under the native host. Changing input between steps still fails the immutable
event check; this dispatch layer does not add input-rewrite chains or approval.

## Native startup evidence

Plugin adapters can require a private startup receipt containing a launch token,
adapter, process ID and hook count. The receipt must be written atomically with
mode 0600 after the native host activates the relevant registration. It attests
same-user registration, not privileged enforcement or external API readiness.

The receipt PID must exactly match the foreground Agent leader reported by
`aw-exec` after spawn and terminal handoff. An unrelated live PID, or a wrapper's
separate child PID, is rejected even when the token and other fields match;
a wrapper must exec the Agent to preserve this identity. The watcher starts only
after that owned identity is available. Spawn/transfer failure starts no watcher.

A bounded watcher checks that receipt while `aw-exec` owns the foreground process.
Missing or invalid evidence stops the owned Agent group and reports failure;
a successful process exit alone does not establish hook readiness. The watcher is
joined before instance and file cleanup. Qoder uses its existing command-hook
registration path and does not require a plugin startup receipt.

DeepSeek's receipt proves plugin registration. Its final-result guarantee also
requires the preload to patch the exact agent-loop build before Agent work starts;
a missing patch or changed build fails startup. For a top-level model tool call,
the plugin associates the execution token with the native result through a
`WeakMap`. After native `finalize`/`finish` and before Session commit, the
patched loop awaits the process-wide final gate. A missing association, Provider
block or final-check failure yields a withheld error result. The plugin replaces
the global gate with a rejecting function on disposal; the launcher owns process
termination and private launch-file cleanup.

This gate covers the verified headless Agent-loop delivery path on DSH
`0.2.1-rc.5`. Same-process `tools/result` observers receive raw content before
the gate. Nested/PTC results and direct `ToolRuntime.execute` consumers do not
pass through this top-level commit point. Web, SDK, ACP and custom-profile
delivery require their own native adoption tests; see the
[DeepSeek integration guide](../../../../../../docs/developer-guide/en/aw/agents/deepseek-harness.md).

## Delivery boundary

Qoder records the installed semantic CLI version; its
command-Hook configuration and callback contracts remain enforced.
OpenClaw, Hermes and QwenPaw supply separate native implementations and acceptance
evidence. The daemon still retains at most 1024 correlated events per instance;
long-lived native hosts must expose that limit and their configured failure
behavior. OS enforcement, portable approval and additional events remain outside
this interface extraction.

Resident Pi/Claude factories and ordinary Qoder SDK Queries use the
[sequenced invocation lifecycle](resident-sdk.md)
to reclaim completed correlations; their capacity is concurrent rather than cumulative.
The 1024-correlation boundary above applies to legacy bindings, including the
separate reloadable Qoder SDK path.
