# Trust-mode shell capability negotiation (#2156)

Owner: cosh-core `protocol.rs`/`core.rs` (classify + verdict release) and cosh-shell
`adapter/control_protocol` (capability declaration) with `agent/approval_bridge`
(staged-call disposition). Introduced by the #2156 fix as the approval half of #2067.

## Problem

In trust approval mode, a streamed `ToolCall` reaches the shell before the core's
hook verdict and enters a 200 ms staging grace. Without a wire contract for the
verdict, a grace-released staged call could be auto-approved or handed off and
execute despite a hook Block — and even when the Block won the race, the bridge
journaled the replayed result as `Approved` + provider-native execution.

## Wire contract

The `initialize` control request gains an optional `capabilities` object:

```json
{
  "subtype": "initialize",
  "capabilities": {
    "can_handle_can_use_tool": true,
    "can_handle_host_executed_shell": true
  }
}
```

- **Additive and absent-safe in both directions.** Both fields default to `false`;
  a legacy client or legacy core keeps the pre-capability behavior exactly.
- **Fail-closed pairing.** The core reroutes trust-mode `ShellExec` classification
  to `RequireApproval` (audit reason `trust_shell_handoff`) only when *both* flags
  are true: `can_use_tool` obtains the verdict and `host_executed_shell` returns
  the result, so a semi-capable client would strand mid-handoff. An asymmetric
  declaration is treated as fully absent until an explicit protocol decision says
  otherwise; capabilities are session-scoped and set once at initialize.
- **Block release.** When a hook blocks a rerouted call, the core emits the
  provider-native `tool_result` on the wire, deterministically releasing the
  staged call instead of letting the grace timer decide. The result carries a
  machine-readable verdict marker — `"cosh_hook_verdict": "blocked"` on the
  tool_result content block, present only for hook blocks — so the client keys
  rejection semantics on a core-controlled field, never on result text (which
  the executed command itself can control). Absent on every other result, so
  legacy clients and legacy cores are both unaffected.

## Client behavior matrix

| client | capabilities sent | trust shell call path |
| --- | --- | --- |
| cosh-shell persistent control transport (`cosh_core_service`) | both flags | `can_use_tool` → trust auto-approve → foreground handoff; verdict-gated, executed exactly once |
| cosh-shell one-shot sync transport (`cosh_core_process/input.rs`) | none | legacy provider-native — its stdin writer thread cannot answer `can_use_tool`, so declaring capabilities would strand staged calls |
| claude / qwen drivers | none (plain initialize) | legacy provider-native fallback unchanged |

## Staged-call disposition matrix (shell bridge)

| verdict timing | verdict | wire signal | branch | journal (terminal) |
| --- | --- | --- | --- | --- |
| within grace | Block (any morphology) | result with `cosh_hook_verdict` | hook-block branch (ahead of completed-replay) | `Blocked` + `hook_block`; no approval card |
| within grace | Allow, executed ok/failed | result without marker | completed-replay | `Approved` + execution recorded |
| within grace | Allow, command outputs the block text itself | result without marker | completed-replay | `Approved` + execution recorded (forgery has no effect) |
| after grace | Allow | late `can_use_tool` | M3 provisional, then reconcile | one entry: `Approved` + `staged_resolved_late_verdict` |
| after grace | Block | late marked result | M3 provisional, then reconcile | one entry: `Blocked` + `hook_block` |
| never | — | nothing | M3 fail-closed guard | `Blocked` + `staged_unresolved` (true desync) |

Invariants:

- **Machine-readable verdicts only.** The bridge never infers a hook block
  from result text or notification strings; the `cosh_hook_verdict` wire field
  is the sole source, covering raw `block`/`deny`/`reject`, hook failures, and
  message-less blocks in one form.
- **One terminal journal entry per tool_use_id.** `staged_unresolved` is a
  provisional record of the grace-window desync; when the late verdict arrives
  (control-channel approval/denial, or a block-marked result), the entry
  converts in place — keeping the `staged_resolved_late_verdict` provenance —
  instead of doubling the journal with a contradictory pair.
- **Ordering-race safe.** If the staged call bridges before the marker lands,
  M3 records the provisional entry and the marker's arrival reconciles it.

The hook notification itself stays pending so finish/cancel drains still
surface it through governance. Non-cosh-core drivers never produce these wire
markers, so their legacy fallback is untouched.

## Rollback

Revert the fix commits; with capabilities absent both sides fall back to legacy
behavior. Changelog entries are aggregated by the version-bump PR, not here.

## Follow-up: `approval_bridge.rs` split plan

The bridge file stands at 941 lines after this fix (AGENTS.md: >700 needs a
split plan; >1000 must not gain features). The two render loops share the
four-branch staged-call disposition (foreground-covered → hook-block →
completed-replay → M3 guard). Before the next feature PR touches this file,
extract that disposition into one shared
`resolve_staged_provider_tool_call(state, request) -> StagedCallDisposition`
helper consumed by both loops (est. −60 lines), then reassess whether the
remaining trust/auto surface warrants a further `approval_bridge/` split.

## Decision note: activity → approval reconcile edge

`activity/runtime.rs` converts a provisional `staged_unresolved` journal entry
when the block verdict marker arrives. This is the first `activity → approval`
edge, and it is deliberate:

- The reconcile must run at event-processing time. A late block marker can
  arrive when no bridge pass is running (the staged call was already bridged
  at grace release), so deferring the conversion to the bridge would leave the
  provisional entry stuck until an unrelated call happens to bridge.
- Activity performs no approval-state surgery itself; it calls the approval
  owner's `pub(crate)` API (`reconcile_staged_unresolved_entry`), which owns
  the journal mutation — the same shape as the existing activity → control
  state recorders (`record_provider_tool_output_delta` et al.).
- The alternative (a queued runtime command consumed by an approval owner)
  adds an indirection layer with no additional invariant protection, since the
  conversion rule is already centralized in that one API.
