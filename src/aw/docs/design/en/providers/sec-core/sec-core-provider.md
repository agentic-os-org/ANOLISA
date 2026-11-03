# sec-core CLI Provider boundary

[中文版](../../../zh/providers/sec-core/sec-core-provider.md)

The `aw-provider-sec-core` crate is a concrete AW Provider binary. Its dependency
on sec-core is a public process interface, not a Rust crate or daemon protocol.
No sec-core CLI subcommand, source-package inclusion or AW dependency is needed.

```text
AW Host → aw-provider-sec-core → agent-sec-cli scan-code / scan-pii → sec-core daemon
        ← AW effects/errors   ← scan JSON + exit status ← rules/verdict
```

The AW protocol handles describe, private-config validation, request/digest
binding and effect admission. The bridge owns exact tool-name/JSON-Pointer
selection and the explicit observe/block threshold. sec-core owns code scanning,
rules and verdict meaning. Core and Host remain unaware of scanner fields.
`observe_tool` acknowledges tool-after without scanning or transforming results.

The bridge consumes typed `ok` and `verdict` fields and tolerates additive CLI
fields. A successful CLI exit is necessary but not sufficient for a verdict:
`ok=true/pass` is clear; `ok=true/warn|deny` is risk. Invalid or unsuccessful
responses become execution failures. stdout/stderr and scan findings are bounded
and never forwarded as AW response data. The Host separately applies `on_error`.

Invocation passes literal argv, an absolute CLI path and daemon endpoint, the
selected language, `--mode regex` and remaining `--timeout-ms`. No shell, local
scanner, service bootstrap, retry or legacy-CLI fallback is inserted. This first
implementation targets the public Rust V2 CLI. Future CLI fields should describe
general scanner capabilities, without introducing AW request IDs or event types.

`aw-exec` owns the nested CLI's deadline, stream bounds and process-group cleanup.
The CLI receives the Provider's environment selected by the embedding Host. The
execution budget starts before request parsing and is capped at 60 seconds.
It is checked after EOF; arbitrary blocking `Read`/`Write` implementations cannot
be interrupted by this budget. The caller must bound the whole process lifetime
and stdio for every method. `aw-host` supplies this outer deadline via `aw-exec`;
direct callers must close stdin and terminate stalled processes themselves.
There is no standalone wall-clock timeout guarantee. Cleanup has the executor's
existing one-second allowance.
Linux parent-death signaling stops the immediate CLI if the Host forcibly kills
the Provider. It does not provide descendant-wide OS enforcement: group escapes,
privilege transitions and a killed owner's inability to verify cleanup remain
outside the guarantee. The tested Rust scan CLI does not spawn another scanner
process; it calls the existing daemon. Already-dispatched daemon work is bounded
by sec-core and is not rolled back by terminating its client.

The private configuration and runnable Host walkthrough are documented in the
[user guide](../../../../../../../docs/user-guide/en/aw/security.md).
The sample supplies an entire AWConfiguration; no common configuration schema
changes are required. Deployment ownership, native Hook scheduling, effect
adoption and persistent AW auditing remain responsibilities of the daemon and
adapters. Existing consumers of the earlier experimental `agent-sec-cli
aw-provider` command must switch their Provider argv to this standalone bridge.

## Submission scanning

`scan_prompt` and `scan_user_input` consume the non-tool `input.submit` event. The Adapter owns the
fresh turn identity and native decision boundary; the Host retains one deadline
and one claim per step. The Provider sends the complete text on stdin to the
public V2 `scan-prompt` command with explicit fast mode and user-input origin.
It checks scan completeness as well as verdict. Prompt `ok=false` with warn/deny
is risk, while incomplete/error output remains an invocation failure. AW keeps
backend findings and user text out of effects and metadata audit. User PII calls
the shared `pii::call` transport with `Source::UserInput`, the same turn context
and remaining budget. Its risk mode is `pii.input_mode`; failures retain their
separate Provider error path. Configuration,
version limits and runnable native evidence are described in the
[Provider reference](../../../../../../../docs/developer-guide/en/aw/providers/sec-core.md#user-submission-checks).

## PII boundaries

`pii::call` owns the stdin-only CLI request, source label, remaining budget and
strict scan parsing for both tool phases and user-input consumers. It checks
source, byte coverage, truncation and reported execution/custom-rule failures.
`tool_pii` applies private input/output modes. It scans structured values as JSON
for field/value context, then scans decoded string values and object keys
individually to preserve whitespace boundaries and custom-rule anchors. Plain
strings are scanned directly. The same traversal covers arguments, the complete
native result and its retained envelope; each CLI call receives only the remaining
invocation budget. On risk, one Adapter-authorized text value is cleared in a private
copy and the retained envelope is scanned again within the same budget. Only a
clean residual yields the fixed safety-message replacement. Other shapes fail
through the configured output failure action.

The Adapter projects and adopts the result. Qoder SDK ordinary resident Post
returns `updatedToolOutput`; failed-tool callbacks retain native error semantics
and cannot adopt that directive. QwenPaw buffers selected replacement streams
before Coordinator/public delivery. AW admission still permits one final result
replacement per event. A PII replacement policy must occupy that final step.

## Validation boundary

Run from the repository root.

`cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml` exercises public CLI fixtures,
request binding, offline operations, config rejection, exact argv, decision and
failure mapping, budget/output limits, Host cancellation of the nested CLI and
external deadline cleanup while the Provider waits for EOF on an open stdin.
The normal AW gate runs these checks with the unchanged Provider/Host/Core suites.
Real CLI/daemon checks use the same Host example with isolated daemon paths and
built-in rules; they are separate from fixture tests and from native Agent
acceptance. A returned block is a candidate, not evidence of tool suppression.
