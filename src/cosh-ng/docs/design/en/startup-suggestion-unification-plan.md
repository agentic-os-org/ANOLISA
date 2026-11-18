# Unified startup suggestions execution plan

[中文版](../zh/startup-suggestion-unification-plan.md)

Status: planned on 2026-11-16; Tasks 1-5 executed locally the same day
(focused lib/logic/protocol gates green, layout and inventory audits pass).
Real ALinux4 PTY acceptance and provider-binding verification remain open.
The [design](startup-suggestion-unification.md) is the contract. Readers are
implementers and reviewers of `cosh-shell`.

## Entry and scope

1. Create a follow-up tracking issue for the unified experience; #10179 and
   PR #10182 cover the previous brand-only fix, not acceptance of this design.
2. Read the current component instructions and proportionate test guidance;
   fetch the official main and inspect divergence from the documented
   baseline `ce4c5724ec17be30e197f12c19768b1e4bc139c1`.
3. Use an isolated worktree for implementation. Preserve this chat's existing
   local evidence and unrelated untracked files. Do not start from a stale
   merged feature branch without refreshing the base.
4. Record a real ALinux4 PTY baseline plus focused failing assertions before
   changing behavior. No model call is needed for deterministic fixture
   selection/rendering; real-provider context checks are reported separately.

No install, resource creation, PR, commit or implementation is performed by
writing this plan. Follow the existing issue-fix and test-acceptance workflows
when execution is authorized. Use existing dependencies; no `.env`, lockfile,
version, packaging, or CHANGELOG changes are planned.

Paths below are relative to `src/cosh-ng/`. File ownership is limited to the
listed responsibilities; expand only when a demonstrated call-site dependency
requires it and record that change in the issue. Do not append new behavior
to a production file over 1000 lines. Keep
`runtime/startup/recommendations.rs` below the 700-line threshold by moving the
changed selection/presentation responsibility to its existing owner directory
rather than adding more startup orchestration there.

## Task 1: capture identity and exposure regressions

First pin the design's shared-foundation/scenario matrix. Reuse the same
report fixture to compare Bash and Cosh projections while separately testing
`LoginBanner`, `Startup` and `OnDemand` check eligibility. Assert that changing
the presentation target does not itself trigger collection or mutate the
original report. Cover Cosh with an inner Bash, banner on/off independent of
internal health, login guide vs session healthy-row behavior, and the absence
of personal reads/model generation on the Bash path. Use existing health-plan,
guidance and health-banner test owners for these compatibility checks; do not
fork the collectors or shared metric renderer.

Owners: `crates/cosh-shell/src/runtime/startup_tests.rs`, the existing
`runtime/startup/recommendations/health.rs` component tests,
`recommendation/personal_planner.rs` tests and `ui/guidance/tests.rs`.

- Add mixed health/personal 0/1/2/3-candidate fixtures, first-health suppression,
  guide changes between preparation and publication, and duplicate question
  text with distinct IDs.
- Assert exactly the same ordered IDs in output preparation, ghost route,
  bindings and eligible exposure accounting; retain `MAX_VISIBLE = 3`.
- Demonstrate that a primary-only health claim is insufficient when multiple
  health choices are displayed. Cover a losing concurrent claim without
  silently charging omitted items or refilling the snapshot.
- Include distinct health IDs with one finding fingerprint, one-write batch
  persistence, write failure and reply timeout. Prove height overflow with
  three maximum-supported personal questions at 40-by-24 and 80-by-24, not
  only with the short synthetic preview.
- Freeze failing output/logs as the baseline. Add regression assertions to
  existing private test modules, not a new root test implementation file.

Exit: the new tests fail for the intended mismatch, with no unrelated failure
misreported as the baseline.

## Task 2: prepare one bounded session snapshot

Owners: `runtime/startup/recommendations.rs`,
`runtime/startup/recommendations/health.rs`, a small private
`runtime/startup/recommendations/selection.rs` if needed, and
`diagnostics/health/suppression/claim.rs`. The claim runs in-process under the
existing nonblocking try-lock; the old claim worker flag, dispatch and IPC
transport are retired with their tests.

- Keep existing planner ordering, scope, authentication and capacity.
- Separate eligible health preparation from primary-only visual invitations;
  after planning, atomically claim only the selected health IDs (at most
  three) in-process under the existing nonblocking try-lock, without a
  subprocess budget.
- Revalidate guide state and retain only winning IDs in both the displayed
  and selectable snapshot. Preserve Bash's single claim and suppression
  formats. Enumerate retired transport consumers (flag, dispatch, decoder,
  tests) before removal.
- Evaluate eligibility before recording, deduplicate stored finding keys,
  retain winning candidate IDs, and persist once. Never loop over
  `record_and_persist` to implement a supposedly atomic batch. Replace the
  session worker's old primary-only response with the in-process batch
  result; retire the worker flag, dispatch and decoder together with their
  tests. Lock contention or a failed persist drops the whole batch
  fail-closed with a warn; deterministic witnesses cover success,
  contention and persist failure.
- Build display items, bindings and ghost choices from the same snapshot;
  stop calling the personal-only presentation projection for Cosh startup.
- Remove the superseded personal-only projection and primary-invitation
  fields as their callers migrate; preserve guide validity and binding
  cleanup through the shared publication owner, not legacy wrappers.
- Cover both initial-ready and late-ready health paths, while preserving
  existing no-report/disabled/compact/read-only behavior.

Exit: Task 1 identity/exposure tests pass; existing Bash claim tests pass.

## Task 3: render the approved two-line area

Owners: new private `ui/guidance/suggestions.rs` and its private component
tests, `ui/guidance.rs`, Cosh startup presentation call sites, existing
`i18n/en/health.rs` and `i18n/zh/health.rs` with only necessary registered
message-ID owner files discovered from those definitions.

- Implement zero-state omission; single question without prefix; two/three
  numbered rows with one active marker; no second card or source badges.
- Use display-cell wrapping, hanging indents and width-aware shortcut groups.
  No number, marker or line-wrap padding reaches accepted text.
- Compute total live rows including the actual prompt and blank separator;
  require visible-viewport containment. Unknown geometry or overflow selects
  the specified static-list/prompt-local path, never truncation or hidden
  choices. Remove the old Cosh invitation append/style branch after both
  startup paths use the new writer, retaining facts sanitization.
- Add localized Cosh-only health questions, preserving diagnostic scope and
  target arguments; reuse personal question text verbatim.
- Keep existing resource/finding rendering, Bash invitation and `/health`
  unchanged. Do not add features to `ui/agent_render/` compatibility code.
- Keep Cosh selection presentation outside the shared health renderer;
  preserve context-specific guide/compact-row flags. Any genuinely shared
  rendering edit must pass same-report paired Bash/Cosh assertions as well as
  normal/warning/critical/partial cases, not just the OOM preview.
- Add semantic layout assertions for 40/80/100 columns, CJK, long service IDs,
  plain/NO_COLOR and duplicate text. Avoid tests that only repeat a format
  string without exercising visible/selectable consistency.

Exit: shared output tests pass, and Bash output/quoting regressions remain
green. This task alone is not acceptance of dynamic switching.

## Task 4: synchronize selection and safe terminal ownership

Owners: `raw_input/relay.rs`, `raw_input/mod.rs`, existing raw relay tests,
`shell_host/raw_relay/input_events.rs`, relevant shell-host prompt ownership
call sites, and a small private shell-host selection-surface module if needed.

- Carry stable selected identity and snapshot generation through cycle
  notifications while preserving the relay's synchronous selection of the
  next candidate, including Shift+Tab/Tab/Enter in one input chunk.
- Let the shell host own the live recommendation region and its anchor;
  never let both runtime and raw relay independently repaint that region.
- Implement Live / PromptLocal / Deferred / Draft explicitly. Update active
  decoration and ghost together with fresh-generation and ownership checks.
  Only a still-proven empty prompt permits prompt-local fallback; otherwise
  defer all drawing, prompt restoration and candidate-route installation.
  Revalidate pending state on the next safe boundary, not through a blind
  replay of an earlier RestorePrompt.
- Preserve draft ID/text/binding on Tab, editing, cancellation, late results
  and background output. Keep recommendation selection priority and normal
  Assistance-mode switching outside recommendation ownership.
- Add tests for identical text/different IDs, stale cycle events, long wrapped
  prompts, split key bytes, coalesced keys, resize and foreground output.
  New pure cases belong to private components; process/PTY cases go to their
  existing `raw_cli`/`shell_host` targets.
- Verify zero recommendation writes and no route theft while a card or
  foreground process owns input. Test a height-only shrink and an oversized
  list whose top has scrolled out of view; resumption must preserve candidate
  identity while rejecting an expired pending snapshot.

Exit: the currently indicated candidate always matches both direct Enter and
Tab acceptance; no terminal rows or user drafts are erased using guessed
cursor coordinates.

## Replacement and retirement checkpoint

Apply the design's migration inventory during Tasks 2-4, not as unrelated
repository cleanup or a later optional follow-up. Before editing, record
consumers of each retired symbol in production, tests, re-exports and worker
dispatch; after integration record whether it was removed, replaced or kept
with a named remaining consumer.

- Remove the old Cosh single-invitation presentation, the personal-only
  startup card/projection, double facts/invited rendering and superseded
  post-render ghost compensation once one snapshot owns those obligations.
- Keep Bash login/failure/quoting, `default_login`, explicit `/health`,
  post-command recommendation panels, shared notices and health rendering.
  Do not mistake them for unused startup-only helpers. Keep an explicit
  presentation scenario even if a legacy Cosh invitation branch disappears.
- Simplify helpers with a single remaining scenario where that reduces
  unused abstraction, without changing its admission or safety checks.
  Update old function-focused tests to assert the new end-to-end outcome;
  retain suppression, privacy, binding and input regression intent.
- Audit stale imports/exports, enum fields, translation IDs, fixtures and
  worker flags. Use repository search plus compilation of affected targets
  on Linux; public exports and test-only uses can conceal production orphans,
  so a clean warning count is not sufficient by itself. Add no dead-code
  allowances, disabled old implementations or compatibility switches without
  an actual supported consumer.

Exit: every inventory row has an outcome, the new Cosh path has one owner,
and any intentionally retained old-looking code has a verified consumer.

## Task 5: focused verification and ALinux4 evidence

Run from `src/cosh-ng/`; capture full output and each command's exit status in
the issue evidence directory. Do not use `| tail` as the success condition.

```bash
cargo fmt --all -- --check
cargo test --locked -p cosh-shell --lib ui::guidance
cargo test --locked -p cosh-shell --bin cosh-shell recommendation::personal_planner
cargo test --locked -p cosh-shell --bin cosh-shell runtime::startup
cargo test --locked -p cosh-shell --lib selection_surface
cargo test --locked -p cosh-shell --bin cosh-shell selection_surface
cargo test --locked -p cosh-shell --lib diagnostics::health::suppression
cargo test --locked -p cosh-shell --lib raw_input
cargo clippy --locked -p cosh-shell --all-targets -- -D warnings
bash crates/cosh-shell/scripts/check-layout.sh
bash scripts/check-test-inventory.sh
```

Before running filters, list the compiled target and verify that they select
nonzero relevant tests. Run all newly added selection-surface tests as well;
module placement may require an additional exact filter. Select the existing
raw_cli and shell_host cases by their registered full test names using
`-- --list` and then run them with `-- --exact`. Do not invent test names or
accept zero executed tests. Widen scope only for changed ownership contracts
or identified regressions; full workspace gates require an explicit request.

Real PTY acceptance on ALinux4 must cover:

| Case | Required observation |
|---|---|
| 0/1/2/3 eligible candidates | Health facts preserved; one unified area; exact count and order |
| Mixed sources and suppressed primary | No hidden selectable item, duplicate area or wrongly charged omitted item |
| Shift+Tab, Tab and direct Enter | Stable numbers; matching marker, ghost, text and binding |
| Filled/edited draft | No overwrite or retarget from selection/result updates |
| 40/80/100 columns, CJK/English, long target | No clipping, lost identifiers, stale wrap rows or cursor drift |
| Resize/output/card/foreground owner | Safe reflow or explicit prompt-local fallback; no guessed erasure |
| Prompt ownership lost | No fallback drawing, prompt restoration or candidate-route takeover until a proven empty prompt |
| 40-by-24 / 80-by-24 / height shrink / maximum-length questions | Full static list when live region cannot fit; all shown IDs selectable at a safe prompt |
| Several questions for one finding / persist failure / reply timeout | Winning IDs preserved; finding keys deduplicated; single atomic persistence and conservative timeout semantics |
| Guidance disabled, AI disabled, compact `--ask` | Existing policy and startup behavior preserved |
| Bash and explicit `/health` | Existing invitation and explicit health presentation preserved |
| Cosh hosting Bash / banner off | Cosh still has session choices; Bash banner setting does not disable Cosh health |
| Context coverage and deadline | No newly enabled checks, extra scans, personal reads on Bash, or widened deadlines |
| Color, NO_COLOR, plain/read-only | Readable selection where interactive; no unusable shortcuts elsewhere |

Use the real changed binary, real Bash/PTY and actual input events; label
health fixtures and synthetic personal candidates explicitly. Capture cast,
full-size screenshots, binary checksum/base SHA, case outcome and cleanup.
Verify selected provider request/context through the real adapter when
claiming provider-binding acceptance; otherwise report that scope unverified.
Do not induce a live host OOM just to demonstrate a visual state.

Exit: focused green plus the above real-PTY evidence; any missing PTY or
binding scope is a declared acceptance gap, not inferred from mockups/CI.

## Task 6: document, review, deliver and clean up

- Update paired `docs/design/{en,zh}/shell-entry-guidance.md` and affected
  canonical recommendation user guidance only when behavior is implemented.
  Keep the paired design and plan aligned with any implementation decisions.
- Review candidate identity, batch claims, draft ownership and redraw exits,
  not just screenshots. Record baseline-only failures separately.
- Include the completed retirement inventory and search/compile evidence in
  review. Confirm the old functions' tests were migrated rather than simply
  deleted and that Bash/on-demand/post-command retained consumers still work.
- Create a focused PR linked to the follow-up issue when authorized, with
  real-PTY before/after evidence and accurate excluded scope. Version bumps,
  packaging changes and changelog entries are not part of this work.
- Keep one logical implementation commit unless independently compiling
  responsibilities justify a split; follow repository commit attribution.
  Re-run applicable checks after a base rebase.
- After confirmed merge, preserve final evidence, remove only this work's
  resources/branches and use managed-worktree archival for managed worktrees.
  Keep assets still referenced by the PR. Report cleanup only after verifying
  the actual state.

Rollback: revert the implementation change; preserve user state and the
existing suppression format. A release/RC/RPM acceptance claim requires its
own artifact and environment evidence beyond this source-change plan.
