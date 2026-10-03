# OpenClaw Gateway adapter

[中文版](openclaw-adapter_zh.md)

The adapter launches OpenClaw 2026.9.6 as a new foreground Gateway and registers
AW steps at `before_tool_call` and `after_tool_call`. The same `aw.yaml` selects
Providers and tool-event policies; OpenClaw retains its model configuration,
credentials, native plugins and conversation state.

## Launch and ownership

```sh
aw run --config aw.yaml --agent openclaw \
  --native-settings /absolute/path/openclaw.json \
  --native-state-dir /absolute/path/openclaw-profile
```

The configured Agent `argv` must select `openclaw gateway run`. A Node executable
followed by the official `openclaw.mjs` entrypoint is also accepted. The adapter
checks the OpenClaw version before connecting the launch to the service.
`--force`, reset and profile-switching options are rejected. AW does not attach
to an existing Gateway or stop one to obtain its port.

Both native paths are explicit. The state directory must already exist and
belong to the caller. AW holds `.aw-launch.lock` in that directory until its
Gateway exits; OpenClaw also retains its native Gateway ownership checks.
`OPENCLAW_ALLOW_MULTI_GATEWAY=1` is rejected. The lock file remains as a private
empty file and can be removed when no AW launch is using the profile.

AW creates a private configuration overlay and plugin beneath the launch
directory. It points `OPENCLAW_CONFIG_PATH` at the overlay and
`OPENCLAW_STATE_DIR` at the selected persistent profile. It does not replace
`HOME`, copy credentials or rewrite the original configuration. The overlay is
read-only to OpenClaw and is removed after launch cleanup. Native configuration
must be expanded JSON without `$include`, with `gateway.mode: local`; JSON5 and
relative includes are not interpreted by this adapter.

Existing plugin entries and load paths remain present. AW appends its own load
path, adds `aw-native-hooks` to a nonempty explicit plugin allow list, and rejects a
disabled or already-owned AW plugin entry. A registration receipt is written
only from the native `gateway_start` callback. The launcher checks its private token, adapter and registration count;
the receipt is same-user registration evidence, not a process-identity attestation. Failure to register within 30
seconds terminates only the new owned Gateway process group.

## Native tool semantics

| Boundary | Native scheduling | AW behavior |
| --- | --- | --- |
| `before_tool_call` | Serial, descending plugin priority; equal priority retains registration order | One callback per AW step at native default priority; block ends the native chain |
| `after_tool_call` | Concurrent handlers; returned values are discarded | One callback per AW step; records observation without rewriting tool results |

OpenClaw clones the original before-event arguments for each handler. Existing
plugins' returned argument replacements are merged by OpenClaw and are not
passed through successive AW steps as new event snapshots. AW retains this
native behavior. It does not claim that its check observes every plugin's final
argument rewrite.

Callbacks carry the original `{hook, event, context}` JSON. Structured Providers
receive the normalized event plus this native payload. `sessionId`, `runId` and
`toolCallId` are required. AW derives a stable call identifier from the run and
tool-call IDs, so callbacks for one event share one daemon deadline while later
runs cannot accidentally reuse the same event. Arbitrary native tool names and
argument objects are preserved.

Native commands receive the Gateway's callback-time environment, including
values loaded after process startup. Structured Providers retain the launch
environment pinned by the service. The environment is not inserted into the
native JSON payload or the metadata audit.

Budgets are limited to 1–12,000 ms, leaving callback transport time before
OpenClaw's 15-second before-handler timeout. A native command's successful
stdout must be empty or a JSON object; its object is returned to the native
hook runner. Command failure, signal, timeout or invalid output follows the
step's `on_error`: before may block or report; after reports. Structured block
returns `{block:true}` and a reason. A neutral response never grants a native
permission. Native `requireApproval` output remains OpenClaw-specific; AW does
not advertise a portable structured `ask` effect.

## Acceptance boundary

The ordinary test suite checks callback payloads, errors, bounded execution,
configuration preservation and registration ownership without a model or
cloud credentials. The optional `openclaw-native.mjs` suite executes the pinned
official Hook runner to verify serial before, terminal block, plugin coexistence
and parallel after semantics.

Some OpenClaw execution paths dispatch after hooks without awaiting them. Agent
turn completion alone is not evidence that after callbacks finished. Gateway
shutdown and the AW instance-release cancellation boundary must be included in
runtime acceptance; this adapter does not guarantee delivery after forced
process termination.

The Agent runner also emits `after_tool_call` for a tool rejected by a before
hook, carrying an error result. Such an after event does not mean the tool ran.
Acceptance checks the allowed command's file marker and the blocked command's
absent marker independently of callback counts.

The Agent tool lifecycle is the supported entrypoint. Direct operator
`tools.invoke` HTTP/RPC calls do not carry the same session/run identifiers or
after boundary and are not accepted as equivalent coverage.

The current daemon retains at most 1,024 native-event records per bound
instance until release. A long-lived Gateway can reach this limit; two events
are normally consumed per completed tool call. Subsequent event failures follow
`on_error` and remain visible in diagnostics. Production retention and
session-level instance renewal are separate work.

This adapter does not cover existing-Gateway takeover, tool-result persistence
or result middleware, a portable approval dialog, OS enforcement, or a final
protected security checkpoint.
