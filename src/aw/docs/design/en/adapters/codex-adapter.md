# Codex CLI Adapter

[中文版](../../zh/adapters/codex-adapter.md)

The `codex` Adapter uses `aw run` to bind a local Codex process to the shared
Service and Provider chain. `aw connect` writes one stable AW command Hook for
each of UserPromptSubmit, PreToolUse and PostToolUse. Codex owns session, model,
sandbox, approval and native Hook scheduling. AW owns launch admission, ordered
Provider steps within each AW command, and the resulting allow/block decision.

Installation has one publication boundary. Cancellation observed before the atomic
exchange (or initial no-clobber creation) leaves the live Hooks unchanged.
After publication, sync and verification finish even if cancellation arrives.
The displaced inode is retained for native editors that do not share AW's lock.
Any retained backup blocks all subsequent installation paths, including no-op
success, until the operator reconciles it. Its presence is the durable recovery
state across crashes; the installer never infers reconciliation from matching
live AW definitions. The [integration guide](../../../../../../docs/developer-guide/en/aw/agents/codex.md)
describes that manual acknowledgement. Cancellation, failed publication and
repeated installation are tested as consecutive state transitions.

Preflight records the reported CLI version and checks the public Hook inventory,
enabled state, trust and exact AW definitions. Versions 0.163.0 and 0.166.0
have native acceptance evidence; these general launch forms discover capabilities at runtime.
A required `tool.before` also rejects other PreToolUse Hooks: Codex runs matching Hooks concurrently and may
execute another Hook's `updatedInput` after AW checked the original input.
Optional checks retain this native composition and attest only to original input.

On the plain app-server stdio entrypoint, AW leaves its
transport on inherited stdin/stdout and checks the same startup Hook inventory;
the probe and launched process both receive Codex's remote-control disabled
marker. Codex 0.166.0 reports the disabled state at startup. Both verified CLI
versions execute prompt, before-tool and after-tool Hooks on
new threads using the launch cwd and native config. App-server accepts client
RPCs that override thread or turn cwd and configuration. A thread/start config
override disabling Hooks bypassed AW in the native fixture. Startup admission
therefore covers a trusted fixed-config client, while arbitrary app-server RPCs
need a separate controlled request boundary.

The 0.166.0 gateway entrypoint admits six distinct `-c` model-provider
assignments, including a numeric loopback port and a named bearer-key environment
variable. All five provider settings must belong to the ID in `model_provider`.
The relay receives that validated ID at launch and rejects a different
`thread/start.modelProvider`. The inventory query runs under the exact admitted argv, cwd and home.
Pre-reviewed native declaration hashes can be relocated to a new home's trust
keys. Every worker still probes the effective configuration layers. No instance
identity or inventory result is shared across homes.

The binding records `app-server-gateway` as its entrypoint. The stdio relay
accepts integer request IDs, up to 128 outstanding requests, 1024 tracked
threads and 4 MiB per frame. It forwards only the
admitted fresh-thread operations, records thread IDs from the native replies,
and rejects requests that can restore a different configuration or introduce
another Hook. Unknown requests receive a protocol error. Server-initiated tool
or approval requests receive an unsupported-method response. Native
notifications are forwarded unchanged. Codex has no cooperative registration
factory or late PID attach in this release.

The Adapter normalizes each native event with session and tool-call identity.
One AW command invokes its admitted steps in order through Service, Host and
Provider. A before block exits with Codex's native denial status; an allow leaves
native approval intact. Codex 0.163.0 and 0.166.0 can turn a PostToolUse `continue:false`
reason into ordinary model-visible text. AW admits this Hook behavior
only with Code Mode disabled and no peer PostToolUse Hooks. The final replacement
step uses `withhold_result` on Provider failure or unsupported projection.
Codex still captures and logs some original results before the Hook, and native
Hook command failures may expose the original; complete result enforcement
needs an independently verified result-delivery boundary.

In the cooperative credential configuration the controller removes its native
Bash PostToolUse and runs its live secret scrubber at its authenticated Responses
gateway. AW's PostToolUse sees the original and applies its admitted PII policy
first. The gateway checks each explicit textual tool result in the outgoing
request and gives the redacted request to the upstream sender or Chat translator.
A rejected gateway check prevents that upstream send. AW Hook audit and the
gateway's scanned/changed counts cover different stages. Shell
`aggregatedOutput` and MCP `item/completed.result` are captured before the Hook.
The [native acceptance fixture](../../../../tests/codex_gateway.py) checks these
notifications, the model request, invalid native trust and rejected RPCs.

Public audit derives `resultStatus` for Codex PostToolUse from explicit outcome
fields (such as MCP `isError` or a structured exit code) and from recognized MCP
`CallToolResult` envelopes whose omitted `isError` defaults to `success`. A
plain-string shell `tool_response` without an exit status leaves an
`aw.native_metadata_unavailable` gap, which audit export reports as
`projection_unavailable`.

See the [integration guide](../../../../../../docs/developer-guide/en/aw/agents/codex.md)
for setup and the [capability reference](../../../../../../docs/user-guide/en/aw/capabilities.md)
for supported effects.
