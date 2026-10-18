# cosh-shell Code Organization Decisions

> In-repo registry for organization-standard exceptions that reviewers must be
> able to discover next to the code. Each entry records the decision, the
> reason it is acceptable, and the conditions under which it is removed.

## D13: activity -> runtime reverse dependency (temporary exception)

Registered: 2026-07-25, shell-handoff-preexec-loss.

Decision: the `activity` owner's existing dependency on `runtime`
(`crate::runtime::prelude::*`, `crate::runtime::state::PendingInteractiveShellHandoff`,
`crate::runtime::evidence_delivery::record_shell_handoff_completion`) is
registered as a known temporary exception to the `runtime -> activity`
one-way dependency direction and is not a blocking finding for new PRs.
The identical imports in `activity/shell_handoff.rs` were migrated verbatim
from `activity/runtime.rs` during a file-level split and do not add a new
reverse edge.

Reasons:

- The reverse dependency is a pre-existing global pattern on the `main`
  baseline: every module under `activity/` (`runtime`, `runtime_render`,
  `runtime_tests`, `tool_invocation`, `tool_result_summary`) already imports
  `crate::runtime::prelude::*`, and `activity/runtime.rs` has referenced
  `record_shell_handoff_completion` and `PendingInteractiveShellHandoff`
  since before this exception was registered.
- File-level splits do not change the module dependency graph: the
  `activity -> runtime` edge exists identically before and after the split.
- Removing the edge requires decoupling the runtime prelude and injecting the
  evidence-recording responsibility through a trait or closure, touching every
  `activity` module and the runtime state model — an architecture-level change
  that must not ride along inside a bugfix PR.

Removal conditions (follow-up SDD):

- Route the activity-side state updates of `record_shell_handoff_completion`
  through a forward `runtime -> activity` call (closure or trait injection) so
  `activity` no longer holds references to runtime-internal functions.
- Move `activity` modules onto explicit narrow interfaces (`types/` contracts
  or dedicated context parameters) instead of the blanket
  `runtime::prelude` import.
- Once complete, delete this entry and restore the exception-free one-way
  dependency wording in the organization standard.

## D14: shell handoff untracked-status string contract (registered debt)

Registered: 2026-07-25, shell-handoff-preexec-loss.

Decision: `types::shell_handoff::SHELL_HANDOFF_UNTRACKED_STATUS`
(`pub(crate) const &str = "completed_untracked"`) is the cross-owner
contract token for a shell handoff closed at a prompt boundary without
preexec tracking. Consumers: `activity/shell_handoff.rs` (row status),
`runtime/evidence_delivery.rs` (provider notice gating), and
`ui/agent_render/activity.rs` (status label match arm). Following the
D11 precedent for intent string constants, the token stays a `&str`
constant rather than an enum variant because it is a persisted
projection/render value consumed by activity rows and the journal;
enum promotion would require a serialization mapping across owners
without adding compile-time safety beyond what the shared constant
already provides (all comparisons and the ui match arm reference the
constant, so typos fail to compile).

Stability: the literal value is frozen; renaming it is a
projection-format change and requires a dedicated SDD.

## D15: ApprovalActionSet lives in ui/agent_render/actions.rs (owner note)

`ApprovalActionSet` (Hook / Standard / TurnConsent) is a presentation
contract: it enumerates which action labels a card renders, in which
order, and how linear indices map to actions. It is defined next to the
`*_PANEL_ACTIONS` descriptor tables it selects between, because the two
must evolve together (adding an action means adding a descriptor).

The policy decision — which request gets which set — is owned by
`approval/panel.rs::approval_action_set_for` (approval owner).
`raw_input/` and `runtime/` consume the enum only to interpret or relay
already-decided sets; they never decide. This keeps standard.md §2
satisfied: `ui/` holds no policy, and the cross-owner references are
reads of a display contract, mirroring how `ApprovalPanelAction` has
always been shared.

Revisit trigger: if a non-UI consumer ever needs to construct or branch
on sets without rendering context, move the enum to `types/` as an
explicit cross-module contract in a dedicated refactor.

## D16: turn-extension approval orchestration (bounded exception)

Registered: 2026-07-30, max-turn continuation.

Decision: `agent/turn_extension.rs` may call approval request rendering and
resolution helpers, while `approval/runtime.rs` may call the turn-extension
resolver. This bidirectional edge is limited to the lifecycle of a retained
max-turn run: record a continuation candidate, request explicit user consent,
then resume the same provider session once.

Reasons:

- The approval owner remains responsible for presenting and recording the
  decision; the agent owner remains responsible for starting the continuation.
- Moving this single flow into a new coordinator would add another runtime
  abstraction without removing state or policy from either owner.
- The edge is confined to the turn-extension request kind and does not expose
  provider protocol details or UI policy across owners.

Removal condition: when a second cross-owner approval workflow needs the same
lifecycle, introduce runtime domain commands and events for approval outcomes,
move orchestration into that coordinator, and delete this exception.

## D17: ApprovalLifecycleLedger lives in runtime/approval_ledger.rs (owner note)

`ApprovalLifecycleLedger` (#1940) is a pure accounting index keyed by
the #1939 identity contract (`run_id` + `request_id`): registered on
first sight, marked on response, swept per run. It lives in `runtime/`
because its owner is the run lifecycle, not approval policy — the
ledger is held by `ControlState` (`runtime/state.rs`), and its two
sweep triggers are runtime lifecycle events (`runtime/cancel.rs`,
`runtime/evidence_delivery.rs`). It depends only on std and holds no
policy: what a dropped request is denied *with* (message, audit
drop-site, terminal deny) is decided by `approval/runtime.rs`, which
consumes the ledger the same way it already consumes other
`ControlState` accounting. Splitting the data structure from its
`ControlState` host would recreate the cross-owner reach the note is
about; the maintenance contract (every `control_response` exit must
`mark_responded`) is documented in the module docs.

Revisit trigger: if a second consumer beyond approval drain/sweep needs
lifecycle bookkeeping, or the per-domain `ControlState` split lands,
move the ledger into that extracted state module together with its
host field.

## D18: interactive-sentinel hint card renders inside shell_host (owner note)

The #2025 interactive sentinel (`shell_host/raw_relay/interactive_sentinel.rs`)
builds and emits its one-shot hint card itself — hand-assembled ANSI
border, width clipping and i18n copy — instead of routing through the
`ui/` renderer like the sibling #2161 timeout notice
(`runtime/controller/input_wait.rs`, which goes through
`RatatuiInlineRenderer::write_notice_panel`). The exception exists
because the card must be spliced into the raw PTY byte relay at a
precise stream position: the relay owns the display buffer, the
alt-screen state and the in-stream timing (card + prompt-tail redraw
must land between two output chunks), none of which are visible to the
inline renderer, and handing the relay's byte stream to `ui/` would
reverse the allowed dependency direction. The card model (kind →
message IDs) stays declarative and the copy lives in `i18n/`.

Removal condition: if a second in-stream card appears in `shell_host`,
extract a shared presentation helper (e.g. `types/` card model +
byte-emitting formatter) and move both emitters onto it.

## D19: slash-bearing prompt routing crosses input owners

Registered: 2026-08-27, slash-bearing natural-language routing.

Decision: `input/path_prompt.rs` owns the conservative submit decision for a
Han prompt whose literal first token contains a provably missing path.
`raw_input/relay/path_prompt_submit.rs` owns Readline-mirror trust, batching,
and the pre-PTY handoff. `shell_host` remains the source of the trusted
primary-prompt cwd and the consumer of the resulting intercept event. This
split keeps the execution veto outside Bash and Zsh hooks while leaving OSC
markers observation-only.

Reasons:

- Bash does not invoke `command_not_found_handle` for slash-bearing command
  words, so the decision must happen before the submit reaches the PTY.
- The input relay already owns slash controls and exact user-byte state; it
  can fail closed for edited, pasted, batched, or non-primary-prompt input.
- The marker parser already reports the child Shell cwd at primary prompts.
  Sharing that trusted snapshot avoids Linux-only process inspection and
  ensures classification and the Agent request use the same workspace.
- Sensitive-input detection remains in `evidence`; the route carries only its
  boolean result into `shell_host` so durable redaction policy is not copied.

Revisit trigger: if a second natural-language pre-submit route is introduced,
extract a dedicated routing-policy owner and inject its decision into
`raw_input`; do not grow `input/` into a general natural-language parser or
move execution vetoes back into Shell tracing hooks.

## D20: Gateway binary composition root

The Gateway application owner maintains the binary-only `cosh-gateway-app`
crate. Its [design note](../../src/cosh-ng/docs/design/gateway-app-composition.md)
records callers, dependency direction, the executable surface, test boundaries,
the in-package alternative, and concrete revisit conditions. This is a
composition boundary, not a public Rust library or a policy duplication.

## D21: Task presentation test ownership and growth

The `ui` owner maintains `ui/question_presentation_tests.rs`, mounted privately
under `#[cfg(test)]` in `lib.rs`. The `slash/task` owner maintains
`slash/task/tests.rs`, `slash/task/form/tests.rs`, and
`slash/task/snapshot/tests.rs`, each mounted privately by its production owner.
These are process-free private presentation/state tests allowed by the scoped
AGENTS testing strategy. They need no heavy-test waiver and must not expose
production APIs solely to move tests. New subprocess or PTY checks belong in
the existing integration targets; the source heavy-test inventory is unchanged.

Task presentation currently occupies 572 lines in `slash/task.rs`, 684 in
`slash/task/form.rs`, and 678 in `slash/task/snapshot.rs`. Before adding another
flow to the near-threshold form/snapshot owners, extract the relevant submit or
switch orchestration into child modules, preserving the existing
navigation/render and contract/view separation. Keep `task.rs` as dispatch;
move additional Gateway request/response handling into a child owner as needed.
This is a growth condition, not a waiver of the 700-line threshold.

The runtime owner co-locates `task_form` and `task_snapshot` in `InlineState`
with other active interactive state. `runtime/state.rs` is 991 lines at this
revision; the advisory inventory is updated to that count. Before further
Task state growth, extract a Task presentation state holder with its owning
fields and lifecycle together; continue the existing per-domain state split
before reaching the 1000-line growth bar. Do not move the two fields alone
into a new cross-owner interface.
