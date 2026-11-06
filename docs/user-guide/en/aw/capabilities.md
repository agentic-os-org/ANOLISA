# AW capability support matrix

[中文版](../../zh/aw/capabilities.md)

AW shares tool checks, result processing and custom rules across Agents. Adapters connect these capabilities to native tool execution. See the [Agent index](../../../developer-guide/en/aw/native-agents.md) for versions and setup.

## Hook support matrix

✓ Declared with contract coverage; × unsupported. Result replacement is limited to authorized text slots and preserves native envelopes and failure status.

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.after` / `observe` | `tool.after` / `replace_result` | `input.submit` / `observe` | `input.submit` / `block` |
| --- | --- | --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `openclaw` | ✓ | ✓ | ✓ | ✓ | × | × |
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `qoder` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `qoder-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->
`tool.before` can observe or block; `tool.after` can observe or replace results through a structured Provider. Boolean scripts and native commands cannot request result replacement.
Configuration can [select steps by tool](configuration.md#select-tools-for-a-hook-step); native permissions and denial retain precedence.
Qoder CLI has no failed-tool Post callback integration. See each Agent page for result types and callback limits.

`input.submit` supports observation and blocking before model dispatch in ordinary
resident Qoder SDK and QwenPaw launches, Claude SDK foreground/resident factories,
Pi workers and AgentScope factory replies. Submission policy uses complete text and session/turn identities.
Claude accepts strings or a single async text message and closes rejected/cancelled
inputs before reuse. Pi requires a `runWorker` factory and literal interactive text;
media, `input`/`context`/`before_provider_request` extension handlers, expanded slash
commands and queued/alternate ingress are rejected when checking is enabled.
Pi also rejects direct callback rewrites of user messages or serialized provider
requests. Restricted reloadable mode and other Adapters reject this event.
AgentScope 2.0.6 accepts native user `Msg` text via `reply` / `reply_stream`; see its
[input contract](../../../developer-guide/en/aw/agents/agentscope.md#check-user-input-before-a-reply).
See [submission checks](security.md#check-user-submissions).

Tool PII is verified with Qoder SDK 1.0.56 / Worker 1.1.78 ordinary resident
Queries and QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App replies. Input checks precede
tool dispatch; output checks scan the complete result and replace one authorized
text slot after checking retained fields. Required failures withhold delivery.
QwenPaw buffers selected tool streams before SSE/model delivery. Qoder SDK's
restricted `--reloadable` mode rejects result replacement. Multi-block/media,
managed MCP and background PII result delivery require separate acceptance.
See [PII policy setup](security.md#check-tool-pii-with-qoder-sdk-or-qwenpaw) and
[runnable native cases](../../../../contribute/en/aw/testing.md#tool-pii-acceptance).

Qoder SDK foreground `Read` text results support PII replacement of file content while preserving file metadata. Image and other non-text Read results require separate acceptance.

## 16 common Hooks

Public Provider Hook status; see the matrix above for effects supported by each Adapter.

| Hook | Status |
| --- | --- |
| `session.start` | Planned |
| `input.submit` | Implemented for ordinary resident Qoder SDK / QwenPaw text, Claude SDK foreground/resident text factories, Pi worker text input and AgentScope factory text replies |
| `tool.before` | Implemented |
| `tool.after` | Implemented |
| `permission.request` | Planned |
| `compact.before` | Planned |
| `compact.after` | Planned |
| `subagent.start` | Planned |
| `subagent.stop` | Planned |
| `turn.stop` | Planned |
| `session.end` | Planned |
| `model.before_request` | Planned |
| `runtime.observed` | Planned |
| `runtime.exited` | Planned |
| `security.violation` | Planned |
| `coverage.changed` | Planned |

## Skill checks

Qoder SDK 1.0.56 / Worker 1.1.78 and QwenPaw 2.2.2.post1 / AgentScope 2.0.10
support local Skill checks through `tool.before` in ordinary resident mode.
The sec-core Provider's `check_skill` validates the actual resolved source;
refusal stops instructions before model delivery. `--reloadable` and interactive
`ask` are unsupported for this operation. See [setup and source limits](security.md#check-skills-before-loading).
Native acceptance is registered as `skill-ledger` for both adapters, with the
QwenPaw `skill-objects` suite covering native duplicate selection, source pinning
and cancellation in the [native matrix](../../../../src/aw/tests/native-matrix.json).

Hermes local chat supports directory-based `skill_view` through the same
`tool.before` check. See [Hermes Skill sources](../../../developer-guide/en/aw/agents/hermes.md#skill-trust)
for native selection and source limits.

## MCP and policy updates

| Entrypoint | Managed MCP | Idle policy updates |
| --- | --- | --- |
| Qoder SDK | In-process handlers with Query identity | Ordinary resident mode, after all Queries close |
| QwenPaw App | Explicitly wrapped MCP client calls | Ordinary resident mode, after all replies drain |
| Claude SDK resident | In-process handlers; single-text result compression, `report` faults keep the original result | Restart required |
| Pi SDK | MCP clients exposed as managed custom tools | Restart required |
| Qoder CLI, OpenClaw, Hermes, AgentScope SDK | Exposed native tool callbacks | Ordinary mode requires restart |

Qoder TUI, Qoder SDK and QwenPaw also offer restricted update modes. See [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md) for versions and deployment.
Configure MCP security checks for actual input semantics; see [Qoder SDK / QwenPaw MCP wiring](../../../../src/aw/docs/design/en/adapters/structured-mcp.md) and [Claude SDK MCP wiring](../../../../src/aw/docs/design/en/adapters/claude-mcp.md).

Qoder SDK applications can also integrate pinned Bash credential consumers, Worker
imports and declarative role assets. Setup and permission requirements are in the
[Qoder SDK guide](../../../developer-guide/en/aw/agents/qoder-sdk.md#native-bash-command-consumers).

Qoder SDK supports native configuration and one-shot prewarming with unmodified
SDK/Worker files. Checks observe public Hook input; later native argument rewrites
are outside the final-input guarantee. See [native configuration](../../../developer-guide/en/aw/agents/qoder-sdk.md#native-configuration) for MCP, Pilot and coverage limits.

## Boundaries

- Ordinary Qoder SDK and QwenPaw accept Post failure action `withhold_result`, withholding delivery and stopping subsequent dispatch. Existing tool side effects remain.
- AgentScope 2.0.6+ accepts Post failure action `withhold_result` for tool.after: a failed strict check replaces the unchecked native result with an error-state placeholder at the end of the middleware chain, so other after hooks still observe the original result. Admitted only when the bridge supports result replacement.
- Pi managed tool.after steps with `on_error: report` keep the checked original result when a Provider or transport fault occurs, and the same session continues with its next prompt. Before-tool faults, post-tool `block`/`withhold_result` faults, cancellation, identity and cleanup failures still stop the task. Input refusal and blocking input-check faults reject that prompt and allow another prompt after cleanup.
- Claude SDK resident `tool.after` steps with `on_error: report` keep the original result and continue model calls for audited, recoverable Provider failures. This covers native HTTP MCP text and `structuredContent` results and built-in tools such as Bash; managed SDK MCP retains its existing recovery. The same client accepts another task after callbacks drain. Identity/scope failures, cancellation, lifecycle/cleanup failures, before-Hook failures, invalid native responses, unknown transport details and audit failures remain fail-stop. `on_error: block` and native security denial retain their blocking semantics. See [resident deployment](../../../developer-guide/en/aw/deployment/resident.md) for the recoverable categories. Post checks cannot undo an executed tool.
- Public Provider Hooks currently cover the two events above. The other 14 event names, common approval, input replacement, guard, Schema/BeforeModel and Retrieve integration are not available.
- AW enforces policy through native Hooks. Deploy OS sandbox isolation separately.

The matrix is generated from the [regression manifest](../../../../src/aw/tests/regression-matrix.json). The [native manifest](../../../../src/aw/tests/native-matrix.json) records pinned versions and coverage; see [contributing](../../../../src/aw/CONTRIBUTING.md) for validation.

Framework releases use the [native integration checks](../../../developer-guide/en/aw/native-agents.md). The recorded test baselines identify reproducible validation inputs; other releases can use the same native contracts.

## Native configuration loading

The common native-loader API and offline source-reference editor preserve
original files and keep declaration, load, admission and activation distinct.
Required deterministic consumer tests cover unknown native fields, source order,
credential references, updates and covered-call failures. Built-in Adapter
source selection and native configuration restrictions remain documented in
their Agent guides; this API alone does not remove them or certify business SDK
acceptance. See [configuration](configuration.md#record-native-configuration-sources).

### Claude SDK native configuration

| Native capability | Compatibility | AW boundary |
| --- | --- | --- |
| Default discovery, user/project/local sources and inline/file settings | Preserved through the SDK loader | Existing files and source precedence stay native |
| Resident model endpoint in native settings | Anthropic Messages upstream follows effective native settings | A confirmed session route sends model responses through AW before queries are accepted |
| Project Skills and local plugins | Preserved; native CLI discovery tested | Internal commands require separate coverage evidence |
| CLI-loaded settings/project/plugin argument rewrites | Native behavior retained | AW checks public Hook input; a rewritten ordinary tool command can execute without a final AW check |
| SDK, HTTP and stdio MCP configuration | Preserved without protocol conversion; HTTP tenant header tested | Native tool callbacks support observation and before policies; HTTP MCP tool.after compression replaces the model-visible string (structuredContent serialization or single text block) in the next model request; a report fault keeps the original projection |

The regular factory regression covers original options and unknown settings fields.
Pinned native acceptance covers settings/Skills, plugin commands and HTTP MCP.
Bash text replacement and explicit resident SDK MCP single-text replacement retain
their existing limits. Runtime and callback composition restrictions are listed in
the [Claude SDK guide](../../../developer-guide/en/aw/agents/claude-sdk.md).

On the verified SDK 0.2.88 / CLI 2.1.161 baseline, native `disableAllHooks: true`
disables settings Hooks while registered SDK AW callbacks still run. Native rewrite
cases record the unchecked execution boundary and confirm that an AW deny still
prevents execution. These cases do not certify final-argument enforcement.
