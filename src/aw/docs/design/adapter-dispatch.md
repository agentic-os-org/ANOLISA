# Native adapter dispatch

[中文版](adapter-dispatch_zh.md)

The `aw` executable owns daemon discovery, instance binding, foreground process
lifetime and cleanup. Each native adapter owns version and entrypoint admission,
registration files, event normalization and the host's response dialect. These
interfaces are private CLI code. The service adds an optional native-command
environment snapshot to `invoke_step`; existing requests may omit it.

## Launch lifecycle

An adapter prepares its native configuration and reports capabilities. The common
launcher admits the configured steps and asks the adapter to validate their native
limits before starting or reusing the service. It then generates private launch
files, binds the instance, writes an adapter-tagged callback binding and starts the
foreground Agent. Providers receive the adapter's pinned launch environment.

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

## Delivery boundary

This shared change retains Qoder CLI 1.1.64 as the only implemented adapter.
OpenClaw, Hermes and QwenPaw supply separate native implementations and acceptance
evidence. The daemon still retains at most 1024 correlated events per instance;
long-lived native hosts must expose that limit and their configured failure
behavior. OS enforcement, portable approval and additional events remain outside
this interface extraction.
