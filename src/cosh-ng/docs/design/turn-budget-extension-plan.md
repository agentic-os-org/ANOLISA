# Turn Budget Extension Implementation Plan

Status: Implemented; automated and ECS A/B validation complete

Issue: [alibaba/anolisa#2029](https://github.com/alibaba/anolisa/issues/2029)

Verified against `up/main` at `74eaf8feedb2` on 2026-07-30.

## Summary

Let an interactive cosh-shell user explicitly continue a capped Agent task
with another configured turn budget. Reuse the committed provider session and
the existing approval-card input path instead of extending the active
cosh-core loop or silently running without consent.

If the effective `max_turns` is 50, each approval grants one new 50-turn Agent
request. A later cap asks again. Configuration remains unchanged, and every
extension requires an explicit user decision.

## Goals

- Offer `Continue` and `Stop` after a max-turn failure.
- Offer continuation only when the provider session was persisted.
- Continue the same provider conversation with a fresh request budget.
- Use the effective limit reported by cosh-core for the card text.
- Require approval again after every exhausted budget.
- Keep cancellation and ordinary provider failures unchanged.
- Preserve non-interactive and headless failure behavior.

## Non-goals

- Mutating `[agent] max_turns` at runtime.
- Extending one cosh-core loop beyond its configured bound.
- Automatically continuing without user consent.
- Adding custom 10/50/arbitrary budget choices in the first version.
- Adding a pre-limit warning or estimating remaining task work.
- Offering continuation when persistence is disabled or failed.

## Architecture

### Core boundary

Keep the cosh-core loop and visible
`Agent exceeded max turns (<configured limit>)` failure unchanged. Exhausting
the loop produces a typed internal `MaxTurns { limit }` outcome rather than an
error inferred from display text. After persistence succeeds, the headless
protocol reports `error_code = "max_turns"` and the positive numeric
`max_turns` value.

The structured error code is the source of truth for retention, and the
positive numeric limit is the source of truth for extension size. cosh-shell
ignores the human-readable error wording. Missing, zero, or inconsistent
budget metadata suppresses the extension card; retention also requires an
explicitly resumable, committed provider session.

### Shell state

When a standard interactive run finishes:

1. Confirm its terminal event carries structured max-turn metadata.
2. Confirm the adapter exposes an explicitly resumable, committed session.
3. Confirm no user request is already queued.
4. Build a local turn-extension approval tied to the finished run.
5. Store a bounded continuation request next to the approval identifier.

The continuation request uses the same shell session, cwd, Agent mode, and
provider binding. Its user input is a fixed internal instruction to continue
the unfinished task without repeating completed work.

### Approval reuse

Add a turn-extension approval kind and a two-action approval set:

- `Continue`: approve one additional configured budget.
- `Stop`: end automatic continuation and leave the persisted session
  available for a later manual request.

Reuse the existing approval card, keyboard capture, decision journal, and
input-generation safeguards. Do not represent the decision as a tool or shell
command approval.

Approving consumes the matching pending continuation exactly once and starts
it as a user-authorized Agent request. Denying, cancelling, stale input, or a
mismatched approval identifier cannot start a request.

### Safety and ordering

- Never offer or start an extension without a committed session.
- Never fall back silently to a fresh provider conversation.
- Never extend cancelled, timed-out, auth, API, or persistence failures.
- Never grant more than one budget per approval.
- A queued explicit user request takes precedence over an extension card.
- Automatic compaction keeps its existing idle-boundary priority.
- A second max-turn failure creates a new approval instead of reusing the old
  decision.

## User Experience

For an effective five-turn configuration:

```text
Agent exceeded max turns (5)

Approval required
Agent turn budget
The Agent used all 5 configured turns. Continue the same task with 5 more?
[ Continue ]  [ Stop ]
```

After `Continue`, the same provider session receives a new request with five
turns. After `Stop`, cosh-shell returns to the prompt and keeps the session
available for a later user message.

## Implementation Areas

- `cosh-core/core.rs`: typed completed and max-turn loop outcomes.
- `cosh-core/headless.rs` and `protocol.rs`: structured max-turn result fields.
- `adapter/cosh_core/recovery.rs`: structured retention and limit
  classification.
- `agent/`: extension eligibility, continuation request, and finish ordering.
- `approval/`: local approval creation and decision delivery.
- `runtime/approval_state.rs`: approval state and request types.
- `runtime/state.rs`: one pending turn-extension request.
- `raw_input/` and `ui/agent_render/`: two-action card capture and labels.
- `i18n/`: equivalent English and Chinese card text.
- Protocol and raw CLI tests: same-session continuation and visible card
  behavior.

Do not add a new root `crates/cosh-shell/src/*.rs` implementation file.

## Test Matrix

### Logic

- Accept positive structured limits without depending on display wording.
- Reject missing, zero, or inconsistent metadata when calculating an
  extension.
- Keep a provider error matching the display text classified as an ordinary
  failure.
- Require an explicitly resumable, committed session.
- Do not offer after ordinary failure, timeout, cancellation, or completion.
- Do not offer when an explicit user request is queued.
- Build a continuation with the same shell session, cwd, and Agent mode.
- Consume a pending continuation at most once.

### Approval and input

- Render only `Continue` and `Stop`.
- Enter on `Continue` starts the matching continuation.
- `Stop`, Esc, and Ctrl+C do not start it.
- Stale or mismatched card events do not start it.
- The approval journal records the decision as a turn extension.

### Protocol and process

- A capped persistent cosh-core run offers extension.
- Approval starts a second request on the same committed provider session.
- The persistent service process is not respawned.
- A second cap offers a fresh approval.
- Non-resumable and persistence-failure paths offer nothing.

### ECS A/B

Validation completed on 2026-07-30 with isolated candidate and control homes
using `max_turns = 5`. Exact revisions and sanitized evidence are recorded in
PR #2035.

- Control stopped after five turns with no committed session or extension card.
- Candidate stopped after five turns and offered `Continue` or `Stop`.
- Candidate `Continue` reused the same persistent cosh-core process and
  provider session, then completed a deterministic seven-tool-call prompt
  within the next five-turn budget.
- The evidence recorded revisions, session continuity, tool-call counts,
  visible card text, and terminal outcomes without raw provider payloads.

## Definition of Done

- The configured budget is reused without changing configuration.
- Every extension is user-approved and same-session.
- The original max-turn error remains visible.
- Local format, Clippy, targeted tests, workspace tests, docs, release build,
  and layout audit pass.
- The fork branch and PR contain a separate atomic `feat(cosh-ng)` commit.
- ECS candidate/control evidence demonstrates the five-plus-five behavior.
