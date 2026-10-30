# sec-core Provider integration reference

[中文版](../../../zh/aw/providers/sec-core.md)

This reference is for integration and maintenance. For installation and daily use,
see the [user guide](../../../../user-guide/en/aw/security.md).

`aw-provider-sec-core` lets an AW policy call the public `agent-sec-cli scan-code` and `scan-pii`
commands. You choose which tool inputs contain code and whether reported risks
produce an observation or a block candidate. sec-core supplies the scanner and
rules; AW supplies configuration, protocol validation and bounded invocation.

See the [user guide](../../../../user-guide/en/aw/security.md) for installation and backend setup. This page covers input mapping, failures and standalone bridge validation.

## Prerequisites and build

Install sec-core using the [sec-core guide](../../../../user-guide/en/agent-sec-core/index.md).
This bridge requires the Rust V2 CLI with `--socket`, `--timeout-ms` and
`scan-code --code --language --mode`, plus an accessible, running sec-core daemon.
Check the deployed CLI's help; the legacy Python CLI is not certified by this
bridge. AW adds no command or dependency to sec-core.

Build the AW Provider and local Host example from source:

```bash
cd src/aw
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
cargo build --locked -p aw-host --example host
```

The Provider binary is `../../providers/agent-sec-core/target/debug/aw-provider-sec-core`. Its invocation is:

```text
aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH
```

`--cli` selects the public `agent-sec-cli` executable; `--socket` selects its
existing daemon. Both paths are required and absolute. `--help` prints usage.
The process reads one EOF-terminated AW Provider JSON request on stdin and writes
one response on stdout. It is normally launched by the Host, not interactively.

## External backend and explicit health diagnostics

The Provider package contains the bridge, schema, examples and configuration
resources, not `agent-sec-cli`, `agent-sec-daemon` or `agent-sec-daemon-health`.
Install and maintain these through the [sec-core guide](../../../../user-guide/en/agent-sec-core/index.md)
and the backend's own service lifecycle. Reuse its actual socket. Root services
must not execute programs from user-writable installations.

An installer or diagnostic consumer can explicitly query the installed helper:

```bash
AW_SEC_PROVIDER="$AW_EXTENSION_PREFIX/libexec/aw/providers/sec-core"
"$AW_SEC_PROVIDER/aw-provider-sec-core" --health \
  --helper "$(command -v agent-sec-daemon-health)" \
  --socket /run/agent-sec-core/daemon.sock
```

This is an optional component diagnostic, not an additional Agent startup step.
Continue starting Agents with `aw run`. The command invokes only the existing
`daemon.health` probe, with a one-second process deadline and 4 KiB stream
limits. It never starts a daemon, scans code, or performs target admission.
The helper authenticates the daemon's root peer credentials; AW strips the
sec-core testing bypass environment variable. Nonzero helper exits, invalid or
unhealthy replies, a non-root peer, missing components and deadlines remain
unavailable. Private helper output is not returned.

The separate `aw-sec-core-health/v1alpha1` JSON schema is published at
`providers/agent-sec-core/schemas/health-v1alpha1.schema.json` with
reachable/unavailable fixtures in `tests/fixtures/`. `helper_installed` means
only that the helper file exists; `socket_configured` means an absolute socket
path was supplied. Only successful authenticated health yields
`backend: authenticated_reachable`. `admission` and `check_result` remain
`unknown`: neither health nor a recent execution's `status=ok` proves that a
target admitted a Provider or that a scanner found no risk. This does not change
`aw inspect`, the existing Provider view protocol, or their `unknown` fields.
Diagnostic success/failure does not replace each invocation's configured
`on_error` behavior; backend loss still blocks when `on_error: block` applies.

AW uninstall removes only its managed bridge/resources and retains the
independently managed system service, `/etc` configuration and backend data.
Upgrade or remove that backend through its owning system package. AW currently
has no automatic service installation, upgrade or removal bridge.

## Configure a Provider

Copy [the complete sample](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/agent-sec-core/examples/aw.yaml)
to `aw.sec-core.yaml` and replace its executable and socket paths. It selects
Qoder CLI `Bash` with `native:qoder:Bash`, maps `Bash` to `/command`, and uses
`mode: block`, `[observe, block]` and `on_error: block`. Both before scanning and
after observation use this explicit range. Other native tools, MCP calls and
new tool names retain native behavior and produce `skipped` for these steps.
A skip carries no security approval; native permission denial remains effective.

Step and event selectors intersect. For a selected call, missing mappings,
invalid extraction input, timeouts, Provider crashes and backend failures block
execution through `on_error: block`. Missing event identity is an input failure
before selection. Scanner risks retain their separate `code_risk` result.
Use `mode: observe` with `on_error: report` only for an explicit observation policy.

The shipped templates use these exact names; all mappings select `/command`:

| Template | Native selector | Provider mapping key |
| --- | --- | --- |
| Distribution / Qoder CLI | `native:qoder:Bash` | `Bash` |
| Distribution / OpenClaw | `native:openclaw:exec` | `exec` |
| SDK security / Qoder SDK | `native:qoder-sdk:Bash` | `bash` |
| SDK security / QwenPaw | `native:qwenpaw:execute_shell_command` | `execute_shell_command` |

The [SDK security example](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
adds application Hooks with their own selectors and failure actions. Native
argument changes must pass the Adapter's existing reauthorization path or be
rejected before dispatch. A previous scan does not authorize changed arguments.
Structured MCP authorization uses its own policy contract; a shell/Python input
mapping does not grant remote MCP permission.

Host selectors accept `*`, standard names `bash`, `file_read`, `file_write`, or
`native:<adapter>:<native_name>`. A native selector matches the event's Adapter
and retained `event.tool.native_name`. Provider `config.tools` independently
uses normalized `event.tool.name` keys, so configure both against the Adapter's
event fields. For example, a native Qoder `Bash` call can be selected with
`native:qoder:Bash`; its Provider mapping uses the normalized tool name.

The Provider's private `config` accepts:

| Field | Meaning |
| --- | --- |
| `version` | Required, exactly `1` |
| `mode` | Required, `observe` or `block`; affects `scan_code` and `check_skill` |
| `unmapped_tool` | Optional, `error` (default) or `observe`; controls `scan_code` for a selected tool with no mapping |
| `prompt` | Optional `{mode: observe\|block, scan_mode: fast}` for `scan_prompt` |
| `pii` | Optional `{input_mode: observe\|block, output_mode: observe\|replace_result}`; input mode covers user/tool input; output mode covers tool results |
| `skill` | Optional boolean, default `false`; enables `check_skill` |
| `tools` | Up to 128 exact normalized code tool names; may be omitted when `skill`, `prompt` or `pii` is configured |
| `tools.<name>.language` | `bash` or `python` |
| `tools.<name>.input_pointer` | JSON Pointer into `event.tool.input`; the selected value must be a string |

Unknown fields, unsupported languages and invalid pointers are rejected.
An empty pointer selects a root string. `/command` selects a field;
`/items/0` selects an array element. Names are case-sensitive and use
`event.tool.name`; `native_name` does not select a mapping. A selected unmapped
tool returns the Provider error `tool_unmapped` by default, without candidate
effects. This strict default also applies to existing configurations that omit
`unmapped_tool`. To retain observation without scanning for such tools, set
`unmapped_tool: observe` explicitly; the result is `observe/tool_unmapped`.

`mode: observe` records mapped scan results without blocking on risk. It does
not suppress missing-mapping, extraction or backend errors. A mapped tool with
missing or non-string code fails with `invalid_tool_input`, including when
`unmapped_tool: observe` is set. Keep observation-only error handling explicit
with `on_error: report`.

| Operation | Event | Result |
| --- | --- | --- |
| `scan_code` | `tool.before` | `ok: true, verdict: pass` → `observe/code_pass`; `warn` or `deny` → `observe/code_risk` or `block/code_risk`, according to `mode` |
| `check_skill` | `tool.before` | Valid signed content → `observe/skill_pass`; other completed trust states observe or block according to `mode` |
| `observe_tool` | `tool.after` | `observe/tool_observed`; no scanner call |

`describe`, private-config validation and after observation do not contact
sec-core. Before scanning uses literal arguments and `--mode regex`; code is
never run as a shell command. Exit 0 means the CLI completed a scan, not that its
verdict was `pass`. Nonzero/signal exits, malformed JSON, invalid verdicts,
timeouts and stream limits produce Provider errors without candidate effects.

The caller must enforce a deadline for the entire Provider process, including
stdin receipt and stdout delivery. `aw-host` does this through `aw-exec`. A direct
caller must also close stdin after the request and terminate the process if I/O
stalls; the Provider does not enforce a standalone wall-clock timeout. After EOF,
the bridge charges time already spent reading and parsing against the invoke
budget, capped at 60 seconds, and gives only the remainder to scanning.
CLI stdout is capped at 1 MiB and stderr at 64 KiB.
It forwards its Host-selected environment to the CLI. Responses contain
reason codes and the bound input digest, not source code, findings or CLI stderr.
The CLI receives code through argv, subject to OS argument visibility and size
limits. No stdin/file fallback is provided by this CLI contract.

## User submission checks

The [SDK security example](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
adds `input.submit` to ordinary resident Qoder SDK 1.0.56 / Worker 1.1.78 and
QwenPaw 2.2.2.post1 / AgentScope 2.0.10 launches. Keep the native application's
configuration, permissions, model and startup. Add this Provider setting and
submit step to that configuration:

```yaml
# spec.providers.security.config
prompt: {mode: block, scan_mode: fast}
pii: {input_mode: block, output_mode: observe}
# spec.events
input.submit:
  enabled: true
  required: true
  steps:
    - id: scan-prompt
      provider: security
      operation: scan_prompt
      effects: [observe, block]
      on_error: block
    - id: scan-user-pii
      provider: security
      operation: scan_user_input
      effects: [observe, block]
      on_error: block
```

`scan_prompt` reads `event.input.text` and calls the public V2 CLI
`scan-prompt --mode fast --source user_input --format json`, with text on stdin.
The CLI uses the configured daemon socket and remaining timeout. Fast mode runs
L1 rules; `standard`, `strict`, ML model options and `multi_turn` are rejected by
private-config validation. Whitespace-only text needs no scanner call. All other
responses must identify a completed rule-engine scan of the entire stripped input:
truncation, missing layers, degradation, empty/malformed output or CLI failure
returns a Provider error. Prompt Scanner's `ok=false` plus `warn`/`deny` is a
completed risk decision. `prompt.mode` selects observation or blocking on risk;
`on_error` separately controls failures. Responses expose `prompt_pass` or
`prompt_risk` without findings or raw text.

`scan_user_input` reads the same complete `event.input.text` through the shared
PII scanner transport: `scan-pii --stdin --format json --source user_input`.
It checks the scan source, byte count and completion state, retaining the turn's
trace context and remaining budget. `pii.input_mode` controls risk blocking;
errors follow `on_error`. Responses contain only `pii_pass` or `pii_risk`.
The `output_mode` field belongs to the shared PII configuration; this operation
never replaces user text.

Qoder awaits the public `UserPromptSubmit` snapshot and returns a native block
decision on denial. Existing native Hook decisions remain effective. QwenPaw
awaits the checks before native reply admission and memory insertion. Fresh user
text is scanned once; confirmation, interruption and external execution-result
events resume the native workflow without resubmitting old text. A rejected
QwenPaw submission leaves the next fresh input usable after scope cleanup.
Both Adapters issue a fresh turn UUID, retain session identity and share one event
budget across all steps. Submission events contain no tool/call identity.

Only text submission is covered. QwenPaw non-text user blocks are mapping errors
subject to `on_error`. Restricted reloadable entrypoints reject this event at
admission; use ordinary resident mode. Keep each PII/Prompt check under one chosen
native or AW policy to avoid duplicate scans; AW does not remove user Hooks.

## Tool PII policy

The [two-framework example](../../../../../providers/agent-sec-core/examples/aw.tool-pii.yaml)
adds input PII, code scanning and output PII to one Provider. `scan_tool_input`
scans the complete tool arguments with source `tool_input`; a risk
returns `block/pii_risk` when `pii.input_mode: block`. `scan_tool_output` scans
the actual complete result with source `tool_output`. Pass returns
`observe/pii_pass`; observe modes return `observe/pii_risk` for risks.

Structured arguments and results are scanned as JSON to retain field/value context,
then each decoded string value and object key is scanned separately. This preserves
newlines, tabs and custom-rule anchors. A plain string is scanned directly.
All scans, including the retained-envelope check, share the invocation's remaining
budget; an exhausted budget fails the check.

With `pii.output_mode: replace_result`, a risky result requires an Adapter-authorized
text projection. The Provider clears that single text value in a private copy
and scans the retained envelope within the same budget. Only a clean retained
envelope yields `replace_result` with the fixed message
`Sensitive data detected by AW; content withheld.` It never adopts backend
redacted text. A missing or ambiguous slot, or sensitive retained fields, returns
`unsupported_pii_result`. Native error state and result metadata remain intact.
Safe failed-tool results retain their original error semantics; a Qoder
`PostToolUseFailure` containing PII is withheld because that callback cannot
replace the native error text.

Configure input `on_error: block` and output `on_error: withhold_result` for
required checks. Missing CLI/backend, nonzero exits, empty or malformed responses,
wrong source, truncated or incomplete scans, byte-count mismatch and invalid or
failed custom rules produce errors, separately from risk. Text travels only over
stdin; neither source text nor findings enter Provider responses. The common
`pii::call` API also accepts `Source::UserInput` for submit consumers.

Qoder SDK ordinary resident Queries return the native `updatedToolOutput` directive;
restricted `--reloadable` mode rejects replacement at admission. QwenPaw ordinary
App replies buffer selected replacement/withholding tool streams, including the
Coordinator's public stream, until final adoption. Select supported native tools
explicitly for production. Multiple text blocks, media, managed MCP and background
result delivery need their own native acceptance; a Post callback alone does not
establish replacement support for those modes.

Choose AW or native PII for each tool phase. Explicitly remove the native PII
PreToolUse/PostToolUse registrations when AW owns those phases, retaining any
UserPromptSubmit registration needed for user text. AW preserves native settings
and does not remove Hooks. Keeping both tool registrations causes duplicate scans.

## Validate Skills before loading

The [SDK policy example](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
uses the same Provider for Qoder SDK and QwenPaw. Set `config.skill: true` and
add a `tool.before` step with `operation: check_skill`, `effects: [observe, block]`,
`on_error: block`, and selectors `native:qoder-sdk:Skill` and
`native:qwenpaw:Skill`. The shared `mode: block` allows only `pass`.
`tools` continues to map code inputs; Skill checks do not need an input pointer.
A Skill-only Provider may omit `tools` when `skill: true`.

The Adapter supplies `event.native.aw_skill` with the requested `name`, canonical
`directory` and `source`. Resolution failures supply an `error_code` instead.
The Provider never derives paths from framework names: it calls
`agent-sec-cli skill-ledger check DIRECTORY` with the remaining budget.
The daemon administrator must authorize the actual canonical directories and
make their files accessible. AW neither expands that authorization nor signs Skills.
Use the [Skill Ledger guide](../../../../user-guide/en/agent-sec-core/skill-ledger.md)
to review, scan and sign content separately.

| Ledger status | Reason code | `mode: block` |
| --- | --- | --- |
| `pass` | `skill_pass` | Continue native loading |
| `none` | `skill_unsigned` | Block |
| `drifted` | `skill_drifted` | Block |
| `warn`, `deny` | `skill_warn`, `skill_deny` | Block |
| `tampered` | `skill_tampered` | Block |
| `error`, malformed/empty output, transport failure or timeout | Provider error | Apply `on_error` |

Ledger exit 1 can contain a completed negative trust state; it is decoded as
such. A nonzero exit with `pass`, an unknown status, a signal, or other exit code
is an error. Risk decisions and failed checks remain separate in AW auditing.
`mode: observe` reports trust states without blocking; `ask` is unsupported.
Choose either AW or the native Skill Ledger Hook for this check to avoid
performing it twice. Existing native permission refusals retain authority.

Skill checking requires ordinary resident `aw run`; `--reloadable` admission
rejects it. Idle updates of ordinary resident policies retain their existing
task snapshot boundary. Resolution and pinning run only when a Skill check is
selected; other policies keep native Skill behavior.

Qoder SDK 1.0.56 / Worker 1.1.78 checks a unique local asset among the catalogs
enabled by the Query's `settingSources`, including explicitly enabled
`.agents/skills`. Frontmatter `name` is a display name; nested directories use
the leaf name. Same-name assets in multiple directories, including names differing
only in case, produce `skill_resolution_error`. This also applies when the native
parser would skip one asset: the control API exposes no selected path to prove
which fallback would execute. Use unique names or limit the enabled sources.
AW refreshes the native cache before checking and rejects source edits across
refresh and Provider execution. The refresh response is a UI list and does not
gate model-only Skills such as `user-invocable: false`; native invocation and
permission rules continue to apply. Local Workflow scripts in any enabled
user/project catalog produce a resolution error, including scripts whose filename
differs from their metadata name and directory entrypoints `workflow.js`/`index.js`.
Their JavaScript metadata can introduce a competing Skill without a provable
selected signing root; remove these assets or exclude their source for required
Skill checks. This conservative boundary also applies when native Workflows are
disabled, because their effective gate is not exposed by the control API.
Native `Read` can populate a dynamic Skill cache that survives `reload_skills`.
The control API cannot identify its selected path. Once a Query has attempted
`Read`, Skill checking with project sources enabled returns `skill_resolution_error`,
even if the read failed/was denied or a later idle update enables checking.
Use a fresh Query or select `settingSources: ["user"]` from Query creation for
required Skill checks. Source reads and policy updates never clear this fence.
Legacy command resources, dynamic ancestor
conflicts, additional directories and sources without a local resolved directory
produce resolution errors. Plugin-qualified names and built-in Skill sources
are unsupported. The checked public Hook argument snapshot retains the Adapter's
existing contract for subsequent native argument changes.

QwenPaw 2.2.2.post1 / AgentScope 2.0.10 uses the actual SkillViewer and activated
Toolkit groups. Native duplicate-name precedence selects the object. AW verifies
that its in-memory markdown matches `SKILL.md`, checks its canonical directory,
and pins that object for this invocation. Source edits during checking block
loading. Unavailable or memory-only sources fail resolution. Concurrent calls
retain separate snapshots. AW decodes supplied files with AgentScope's UTF-8-SIG
semantics, including BOM. The stock QwenPaw 2.2.2.post1 App runtime cache itself
skips BOM assets before AW runs; that native loading path remains unsupported.
Read-file loading, automatic context injection and
Skill-internal commands require their own policies.

## Trace correlation

The bridge passes native IDs to sec-core through the JSON `--trace-context` argument, with `agent_name` set to the Adapter name. It reads `trace_id`, `session_id`, `run_id`, `call_id` and `tool_call_id` from `event.native`, accepting snake_case and camelCase aliases. OpenClaw uses `native.event` then `native.context`; Hermes uses `native` then `native.extra`.
Qoder, Qoder SDK and Claude SDK prefer native `tool_use_id` for the tool call; Pi, QwenPaw and AgentScope use `native.tool_call.id`. Missing native IDs remain absent; AW-generated IDs are kept separate.
Values must be strings. The bridge trims surrounding whitespace, omits empty values and caps each value at 256 Unicode characters including the `...[truncated]` suffix. Field precedence is defined in [trace_context.rs](../../../../../providers/agent-sec-core/src/trace_context.rs).

## Try candidate effects locally

From `src/aw`, after editing `aw.sec-core.yaml` and setting `mode: block`:

```bash
./target/debug/examples/host aw.sec-core.yaml tool.before Bash '{"command":"echo safe"}'
./target/debug/examples/host aw.sec-core.yaml tool.before Bash '{"command":"rm -rf /"}'
./target/debug/examples/host aw.sec-core.yaml tool.after Bash '{"command":"echo safe"}'
```

These commands submit synthetic tool events; they do not execute the code being
scanned. With the built-in regex rules, the expected candidates are `code_pass`,
`block/code_risk` and `tool_observed`. The Host prints `adoption: not_tested`:
a block candidate does not prove an Agent blocked a tool. Native before/after
integration and persistent AW auditing still require the daemon and adapters.

Remove the Provider and its event steps to disable this integration. There is no
sec-core patch or packaging change to roll back. Existing Agent-native sec-core
Hooks continue using the same public CLI.
