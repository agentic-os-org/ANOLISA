# AW capability support

[中文版](../../zh/aw/capabilities.md)

Use this reference to choose an Agent entrypoint and determine which policy
controls AW can enforce there. One configuration selects targets through
`aw run --config aw.yaml --agent <target>`; support depends on the selected
Adapter, native version and launch mode.

This matrix describes the capability contract after #8840, verified against its
`1b48f237d183` candidate and ANOLISA `main` at `32925dc0a`, including #9108
resident security and managed MCP. Maintain this reference with changes to those contracts. Native pins and exact acceptance boundaries are maintained in
[the native inventory](../../../../src/aw/tests/native-matrix.json); configuration
syntax is defined in the [configuration reference](../../../developer-guide/en/aw/configuration.md).

## Reading support claims

- **Available** means the named entrypoint admits the capability and has a native
  adoption path. The native inventory identifies automated and manual boundaries.
- **Restricted** means the listed mode, result shape or explicit factory is required.
- **Planned** means implementation or native acceptance is still required.
- A recognized configuration name, an internal lifecycle signal and a public
  Provider Hook are separate contracts. Each public Hook needs dispatch,
  cancellation, correlation, native effect adoption and acceptance coverage.

The shared policy path supports boolean user commands, structured Providers and
native Hook commands. Portable policies use normalized events; native command
input/output retains its framework dialect. Admission rejects unsupported active
controls. Native permissions and denials retain precedence. A shared configuration
can contain target-specific step selectors; AW projects applicable steps before
preparing the target. See [tool selection](configuration.md#select-tools-for-a-hook-step).

## Sixteen common Hook names

All eight registered Adapters currently dispatch only `tool.before` and
`tool.after` into the common Provider path. Their successful/error/denied-attempt
coverage follows the native callback contract. The other fourteen names are
configuration vocabulary with future dispatch work.

The following order is a development proposal, not an availability promise:
**A** maintains existing tool controls and extends the shared resident update
foundation to further Adapters; **B** exposes reliable lifecycle observations; **C** adds model/context
and subagent boundaries; **D** expands permission and specialized security contracts.
Frameworks may enter each stage separately once their native boundary is verified.

| Hook | Available public Provider dispatch | Planned scope / order |
| --- | --- | --- |
| `tool.before` | All eight Adapters: `observe`, `block` | A: retain denial, immutable approved arguments and fresh retry authorization |
| `tool.after` | All eight: `observe`; seven admit constrained `replace_result`, excluding Qoder SDK | A: preserve native envelopes and failure handling; Qoder SDK replacement is separate follow-up |
| `session.start` | Planned | B: verified session create/load/restore observation |
| `input.submit` | Planned | B: verified submission observation; distinguish submission from execution |
| `turn.stop` | Planned | B: native stopping observation with explicit outcome; no inferred task success |
| `session.end` | Planned | B: distinguish session end from process exit and confirmed cleanup |
| `runtime.observed` | Planned | B: publish trusted runtime registration as a Provider event |
| `runtime.exited` | Planned | B: publish trusted root exit observation |
| `coverage.changed` | Planned | B: expose changes in verified integration coverage |
| `model.before_request` | Planned | C: verified outgoing model boundary; Schema transformation needs separate effect/adoption work |
| `compact.before` | Planned | C: verified context compaction boundary |
| `compact.after` | Planned | C: observed compaction result and context identity |
| `subagent.start` | Planned | C: trusted parent/child identity and native startup boundary |
| `subagent.stop` | Planned | C: child termination and callback-drain semantics |
| `permission.request` | Planned | D: observe native permission requests; portable `ask`/approval needs its own contract |
| `security.violation` | Planned; configured guards are rejected at runtime admission | D: specify the final candidate check and enforcement boundary |

Existing sec-core checks run as selected `tool.before` steps; they do not require
`security.violation`. Internal model identity registration, request counters,
cleanup notifications and evidence capture do not enable the reserved public
Hook names. Structured `replace_input` and portable `ask` remain unsupported.

## Adapter and effect matrix

Every row admits before observation/blocking and after observation. Result
replacement changes only an authorized result slot; raw native commands and
boolean Hooks cannot request it. `withhold_result` is an after-step **failure
action**, distinct from replacement. It requires a verified delivery boundary.

| Adapter | Native acceptance entrypoint / pin | After result replacement | Configurable `on_error: withhold_result` |
| --- | --- | --- | --- |
| `qoder` | CLI >=1.1.64; ordinary interactive/print, pinned reload mode below | Restricted to supported successful PostToolUse; failure callback is not connected | Unsupported |
| `qoder-sdk` | SDK 1.0.56 / Worker 1.1.78; managed daemon/Query factories | Unsupported | Ordinary resident only; structured Providers and boolean Hooks |
| `openclaw` | 2026.9.6 Gateway plugin | Restricted to the runtime-scoped native result slot | Unsupported |
| `hermes` | Chat plugin; pinned revision in native inventory | Restricted to its native tool result path | Unsupported |
| `qwenpaw` | 2.2.2.post1 / AgentScope 2.0.10 App/API | Restricted to supported native result slots | Ordinary resident only; structured Providers and boolean Hooks |
| `pi` | SDK 0.84.2 task/resident factory; no native Pi CLI launch | Restricted to successful single text results | Unsupported as a configurable action; resident callback failures stop the task |
| `claude-sdk` | SDK 0.2.88 / bundled CLI 2.1.161; task/resident client factory | Restricted to authorized native text or managed MCP result slots | Unsupported as a configurable action; resident callback failures stop the task |
| `agentscope` | SDK 2.0.6 explicit factories | Restricted to supported middleware results | Unsupported |

Ordinary resident Pi, Claude SDK, Qoder SDK and QwenPaw reclaim completed task
state and enforce active-resource limits. The separate restricted reload modes
have different lifetime contracts; in particular, Qoder SDK reloadable instances
retain Hook correlation history. Choose modes from the
[native integration reference](../../../developer-guide/en/aw/native-agents.md),
including native after-event coverage, cleanup requirements and error behavior.
A native-only test or deterministic local model is recorded with that boundary;
it does not certify an arbitrary hosted-model deployment.

## MCP coverage

Managed MCP authorizes the actual call using its trusted identity, server, tool
and approved arguments. Native tool callbacks alone provide the events exposed by
that framework; they do not establish an actual handler/remote-call boundary.
AW has no universal MCP gateway covering arbitrary external clients.

| Adapter | Managed actual-call boundary | Identity and limits |
| --- | --- | --- |
| `claude-sdk` | `aw_claude.create_sdk_mcp_server` in-process handlers, with the resident client factory | Native dispatch identity bound to the model invocation; Pre/Post once; replacement only for successful single text results without structured content |
| `qoder-sdk` | Managed `createSdkMcpServer` in-process handlers | Query identity and exact server/tool/input; originating model tool ID unavailable, so `parentIdentity: "required"` rejects; strict Post available in ordinary resident mode; no replacement |
| `qwenpaw` | Explicit `managed_mcp_tools(client)` around actual `ClientSession.call_tool` | Model-parent identity; preserves schema, annotations and full result envelope; native remote client lifecycle stays with the application |
| `pi` | MCP clients exposed through managed public custom tools | Existing custom-tool Pre/Post pipeline; no separate general MCP connection wrapper |
| `qoder` | No separate managed MCP factory | Native tool callbacks only where exposed; structured dispatch integration planned |
| `openclaw` | No separate managed MCP factory | Native tool callbacks only where exposed; structured dispatch integration planned |
| `hermes` | No separate managed MCP factory | Native tool callbacks only where exposed; structured dispatch integration planned |
| `agentscope` | No separate managed MCP factory | Native tool middleware only where exposed; QwenPaw's managed helper does not certify this Adapter |

QwenPaw's helper resolves a fixed tool list; dynamic `Toolkit(mcps=[client])`
discovery does not install it. Qoder SDK hosted managed-MCP cases have manual
container evidence; their automation remains follow-up work. See the
[structured MCP contract](../../../../src/aw/docs/design/en/adapters/structured-mcp.md)
and [Claude MCP contract](../../../../src/aw/docs/design/en/adapters/claude-mcp.md)
for exact identity, cancellation and acceptance boundaries.

Select security Hooks by declared tool semantics. A tool name or a `command`
argument alone cannot establish shell execution, working directory or authority.
Opaque tools require explicit mappings/authorization policy. Preserve structured
and multimodal envelopes; a text block may contain a machine-readable business
result. Tokenless Schema/BeforeModel and Retrieve remain planned dependencies,
listed in the [Tokenless delivery contract](../../../../src/aw/docs/design/en/adapters/resident-sdk.md#acceptance-and-full-tokenless-dependencies).

## Hot policy updates

The common update transaction already validates a complete candidate, prepares
Providers, checks revisions and idle state, writes durable audit and activates the
policy. It changes Providers, events and `execution.default_event_budget_ms`.
Model credentials, SDK/Worker versions, launch configuration and AW binaries keep
their restart/deployment lifecycle. Files are not watched; use explicit
[`aw update`](configuration.md#update-a-prewarmed-instance).

| Adapter / mode | Available hot policy update | Readiness and switching boundary |
| --- | --- | --- |
| Ordinary Qoder SDK 1.0.56 / Worker 1.1.78 | Explicit idle update; strict Post, managed MCP and private leases retained | Managed `startDaemon()` confirms readiness before the first Query; all Queries must close and cleanup must finish before update |
| Ordinary QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | Explicit idle update; strict Post, managed MCP and private leases retained | Managed App startup confirms readiness before the first reply; all replies and cleanup must finish before update |
| Restricted Qoder 1.1.73 TUI / Qoder SDK / QwenPaw App | Existing opt-in `--reloadable` mode | Native idle bridge; SDK requires explicit daemon and initialized managed Query; strict Post and private execution leases remain unavailable |
| Externally started Qoder TUI / QwenPaw App | Existing supported PID attach path | Attach before the first request; retain restricted native idle and cleanup contract |
| Ordinary Qoder CLI (interactive/print/headless) | Unsupported | Requires a verified native handoff |
| Pi SDK / Claude SDK | Unsupported | Resident task/snapshot/route handoff planned; retain launch policy until restart |
| OpenClaw / Hermes / AgentScope SDK | Unsupported | Verified native route/idle handoff required; retain launch policy until restart |

For prewarming, start the managed Qoder daemon or QwenPaw App, provision Hook
files and the complete configuration, pause task dispatch, run
`aw update --config aw.yaml --agent TARGET`, and resume dispatch after success.
Without an explicit daemon, ordinary Qoder still requires `createQuery()` within
the 30-second startup deadline; its reservation occupies the instance until the
Query closes. Use `--instance ID` to select among multiple target instances.

PID has two retained roles: external native attachment through
`aw agent connect --pid PID` (and legacy install/remove), and internal process
identity/liveness checks. The optional `aw update --pid` selector is deferred;
all supported update modes use `--agent`/`--instance`. A PID alone cannot establish
an Adapter's update boundary.

The shared transaction coordinates activation with task reservation, immutable
policy snapshots, native-route validation/acknowledgement and final cleanup.
A task retains one revision through PreTool, MCP and PostTool. Preparation,
unacknowledged handoffs and unfinished cleanup prevent activation. The Adapter
supplies readiness, task boundaries and route adoption; the framework owns
candidate validation, revision checks, audit and activation.

Busy updates are rejected without queueing, automatic cancellation or retry.
Preparation failures and revision conflicts retain the old policy. Unknown
cleanup retains occupancy. Audit failure after Host activation closes the instance
and stops the service from further dispatch; stop affected workers and recover
by restarting the service and workers with the intended policy. For an indeterminate transport response, inspect instance state and active
revision before dispatch. Each update affects one instance; the control plane
must serialize updates and task dispatch, including the first task after prewarming.

Further Adapters should reuse this foundation and add native acceptance for their
own task and routing boundary. Public lifecycle Hooks, active-task switching,
Tokenless Schema/Retrieve and Qoder result replacement remain separate work.

## Evidence and maintenance

The [compiled Adapter catalog](../../../../src/aw/crates/aw-adapter/src/adapter/catalog.rs),
[regression matrix](../../../../src/aw/tests/regression-matrix.json) and
[native inventory](../../../../src/aw/tests/native-matrix.json) are the executable
support evidence. The README's generated event/effect grid summarizes that
catalog; this reference adds mode, MCP, update and planned-work boundaries.
Changes to any of these contracts must update both languages and their acceptance
references together. Framework delivery order may differ; unsupported capability
must remain explicit at admission and in this matrix.
