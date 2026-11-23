# Unified Cosh startup suggestions

[中文版](../zh/startup-suggestion-unification.md)

Status: visual direction approved on 2026-11-16; implementation landed the same
day, real-terminal product acceptance is still pending. This specification and its
[execution plan](startup-suggestion-unification-plan.md) describe proposed
changes to ordinary interactive `cosh` startup, not released behavior.

## Problem and baseline

At the inspected baseline `ce4c5724ec17be30e197f12c19768b1e4bc139c1`, Cosh
prints one health invitation below its health card and a separate card for
personal suggestions. The planner and input ghost can nevertheless contain
up to three health/personal candidates. Independent presentation paths make
the visible choice, active input choice, and exposure accounting harder to
keep consistent. Adding the cyan brand in PR #10182 addressed plain-looking
text, but did not unify these paths.

Before `9acd6b35e`, ordinary startup already separated health facts from a
shared suggestion card containing up to three health/personal candidates.
The Bash integration subsequently introduced a single open invitation. This
design restores the shared Cosh presentation while retaining the improved
health facts, finding evidence, and Bash invitation contract.

The November 13 recommendation proposal contributes short, useful questions,
optional nonduplicated evidence, and quiet empty states. Its proposed default
of one suggestion and optional second suggestion is not adopted here: the
agreed ceiling remains three in total. Its broader generation, permissions,
and result-driven withdrawal architecture remains separate future work.

## Shared health foundation and scenario projections

Keep one health foundation: `diagnostics/health/plan.rs` check definitions,
collectors and judgement rules, `HealthScanReport` facts/findings/try-items,
and the existing `HealthBannerModel` / `write_health_banner` metrics, evidence,
severity and width-aware rendering. Do not fork a Cosh copy of the health card
or remove the Bash-derived OOM and resource improvements.

There are two separate context axes already in the implementation:

- Collection uses `ScanContext` in `diagnostics/health/runtime/plan.rs`.
  `LoginBanner` maps to `login-banner`; `Startup` to `session-startup`;
  explicit checks use `OnDemand` / `on-demand`. Shared check definitions do
  not imply identical enabled checks. For example, passive login excludes
  additional mount scanning while startup can include explicitly configured
  mounts; environment checks require on-demand context and its environment
  snapshot. Keep these existing cost/safety boundaries.
- Presentation uses `Target::Bash` / `Target::Cosh` in `ui/guidance.rs`, not
  the name of the inner shell or `$SHELL`. A Cosh session hosting Bash remains
  the Cosh presentation scenario. Explicit `/health` has its own projection.

| Contract | Bash login / `cosh health banner` | Ordinary Cosh session startup |
|---|---|---|
| Facts and diagnostic meaning | Shared models, collectors, rules and renderer | Same foundation; scenario-eligible report |
| Card projection | Login guide/footer and existing full-card behavior | Session projection, including existing compact healthy-row behavior; no duplicate login guide |
| Action projection | Existing single quoted `cosh --ask` command or eligible entry hint | One shared health/personal pool, zero to three natural-language questions |
| Interaction | Copy and run; no Cosh candidate selector | Existing Tab / direct Enter / Shift+Tab plus synchronized selection display |
| Personal data/model work | No new personal-cache read, collection or recommendation-model request | Reuse only existing permitted candidates; this work adds no generation |
| Claims and exposure | Existing login invitation validation and single claim | Session-only bounded batch claim for displayed health IDs; existing personal impressions |
| Timing | Preserve outer deadline, default maximum 500 ms; current login worker allocation is at most 350 ms within remaining time | Preserve startup scan's existing 500 ms budget and existing ready/late-output behavior |

`cosh health banner on/off` controls the Bash login resource projection; it
does not toggle Cosh's internal health configuration. Shared check switches
and per-context policy exclusions retain their separate meanings. Guidance
suppression must not erase otherwise permitted facts. Keep normal, warning,
critical, partial and unavailable-data distinctions in the shared renderer;
the preview's warning card is not a new rule to box every healthy startup.

Sharing the foundation means shared algorithms and data structures, not one
cross-process scan result, identical collection coverage or a new cache. Do
not add another scan to support display unification. Operate on a presentation
copy when clearing try-items so the original report still supplies candidates
and health context. Only the Cosh session orchestration combines health and
personal choices; the shared health renderer never fetches personal data or
owns a selector. Reuse visual alignment conventions without giving Bash a
three-item list or giving Cosh an unnecessary `cosh --ask` command.

## Approved experience

The health card owns measurements, severity, and evidence such as OOM event
time, affected process and confirmed scope. One open suggestion area follows
it. Health and personal suggestions share that area; no second personal card
or repeated health invitation is printed.

Single candidate:

```text
┃ Cosh AI  可以直接提问
  帮我排查近期 OOM 的影响和相关内存限制
  Tab 填入 · Enter 直接提问
```

Multiple candidates, with the second active:

```text
┃ Cosh AI  可以直接提问
    1. 帮我排查近期 OOM 的影响和相关内存限制
  › 2. 帮我确认 inventory-worker 实际加载的配置文件
    3. 帮我对照两轮压测的响应差异与负载变化
  Shift+Tab 切换 · Tab 填入 · Enter 直接提问
```

Questions are synthetic examples, not verified model outputs. The health
example can be a localized rule template; a personal example is available
only if the existing permitted candidate pipeline actually supplies it.

- Show zero to three eligible candidates, not three per source. Never fill
  spare slots with generic help or promotion. With zero, omit the entire
  suggestion area and its shortcuts; retain eligible health facts.
- A single candidate has no number or selection marker. Multiple candidates
  use stable local numbers `1.` to `3.` and exactly one cyan `›` marker.
  Numbers identify choices, not execution steps, new numeric shortcuts, or
  promises that candidates can safely be executed in order.
- Keep ordering and numbers fixed within a displayed snapshot. Cycling only
  moves the active choice. Do not introduce a new ranking or source quota.
- Preserve the existing cyan brand, default-foreground active question and
  readable secondary foreground for inactive questions. Color is not the
  only indication of selection. Do not add a second frame.
- Preserve the current direct-Enter action on an empty recommendation prompt.
  Tab inserts the same question for editing; Enter then submits the draft.
  No new explicit submit confirmation or authorization bypass is introduced.

## Layout and narrow terminals

Columns below are zero-based, measured in terminal display cells:

| Element | Start column |
|---|---:|
| Health border / suggestion accent | 0 |
| Health body / brand / single question / shortcut footer | 2 |
| Multiple-candidate selection marker | 2 |
| Multiple-candidate number | 4 |
| Numbered question text and its continuation | 7 |

At 40 columns, wrap questions using Unicode display width and preserve the
entire service name, path, and question. Continuations align with question
text, not the marker or number. At 80/100 columns, avoid unnecessary breaks.
Wrap shortcut groups as units, emitting a separator only between groups on
the same physical row. There is one blank line before the real shell prompt.
No hard-coded screenshot padding, terminal background, or terminal-font
assumptions belong in production rendering.

Width alone does not establish that a live region fits. Before enabling live
selection decoration, measure all wrapped recommendation rows, the shortcut
footer, actual prompt rows and one separating blank row. Their total must fit
within the usable terminal height, and the complete region must remain within
the visible viewport at its proven anchor. Re-evaluate after width or height
changes. Unknown terminal height or prompt geometry cannot authorize a live
region. Three valid near-512-byte personal questions can exceed a 40-by-24
screen; the count limit is not a row limit.

When content is too tall, print a complete static numbered list without an
active-row marker at a safe output boundary, allowing ordinary scrollback.
Keep all displayed candidates in the selection snapshot. At a proven empty
prompt, show the current number and existing question ghost locally; the
local ghost is a selection cue, not a substitute for the complete list.
Do not truncate question text, add a pager, hide selectable candidates or
reduce capacity to disguise the height overflow. If the prompt is not safe,
defer this publication as specified below.

The renderer works on an unchanged logical question. Prefixes, colors,
line-wrap indentation and numbering are presentation only; they never enter
the inserted/submitted question. Terminal-control characters in candidate
data must not become rendering commands. Use existing candidate safety and
display sanitization boundaries without modifying the request text silently.

Plain/noninteractive/read-only output uses the same ordered text, omits ANSI
and active-selection decoration, and advertises no unavailable interaction.
`NO_COLOR` disables colors, not supported TTY selection. A narrow screen does
not independently reduce the candidate pool or hide choices from selection.

## One candidate snapshot for all consumers

Prepare a bounded snapshot from the existing planner after existing source,
scope, authentication, guidance, suppression and duplicate checks. Each item
retains its stable candidate ID, source, logical question, original binding
and suppression identity. Keep the existing capacity of three and current
ranking; source types are internal, not repeated row labels.

The renderer, `PromptGhostRoute::AgentSelection`, ID-to-binding lookup and
personal impression accounting must consume that same final snapshot.
Shortening a visible label while inserting another longer question is not
allowed. Do not map selection back by question text: two candidates can have
identical words but different identities or bindings.

Render health facts without action prompts, then render the unified snapshot
once. Apply the same path when health arrives after the first welcome output.
Recheck current guide validity before publishing. If a candidate loses
eligibility, remove it before publication from both presentation and input;
do not leave selectable hidden candidates or silently refill a lost slot.

The existing session health claim records only its selected primary finding
while returning a wider eligible pool. Showing several health candidates
requires a session-only bounded batch claim for the health IDs in the final
planned snapshot. Revalidate guide generation and eligibility under the
existing nonblocking lock; remove lost claims from the final snapshot. Do not
charge merely eligible, omitted candidates. Bash continues its existing
single-invitation claim path. Preserve conservative pre-write claim semantics
on output failure; personal impressions remain conditional on successful
output flush. This is exposure consistency, not a new suppression policy.

Claim accounting uses the existing host/finding fingerprint, not the display
number. Multiple displayed health questions can refer to the same finding
(for example general OOM and the affected-process follow-up). Evaluate the
whole requested batch against one pre-claim store snapshot; deduplicate
persistent records by suppression fingerprint, retaining all winning
candidate IDs. Apply the records in memory and persist once under the lock.
A failed write must not publish a partly successful claim response. Preserve
the conservative fail-closed outcome: the batch claim runs in-process under
the existing nonblocking try-lock, because startup already loads the same
suppression store and guide state in-process, while a supervised subprocess
adds a full binary boot that loaded hosts cannot finish inside a small
budget, silently dropping the whole suggestion list. Lock contention or a
failed persist drops the batch with a warn; do not retry or refill during
this publication.

## Selection, draft ownership and redraw

Keep `Shift+Tab` candidate cycling while the recommendation owns an empty
prompt, and preserve the existing mode-toggle priority outside that state.
With one candidate it is not advertised as a candidate switch. Numeric keys
remain normal input. Cancellation, ordinary typing and existing input owners
retain their current priority.

The raw relay already updates the active route synchronously, including
several keys arriving in one input chunk. Its cycle notification currently
carries only text. Extend the internal selection notification with candidate
identity and snapshot generation, so the presentation owner can update the
marker without inferring identity from words or replacing a newer snapshot.

A static scrollback card cannot honestly retain a moving selection marker.
Keep the live suggestion rows in a bounded, shell-host-owned region directly
above the active prompt; record its rendered rows, width, prompt epoch and
output generation. Redraw only when that region and prompt origin are still
proven, no draft/card/foreground process owns input, and no intervening output
or resize invalidated the anchor. Reuse prompt presentation and replay
ownership checks; do not guess a cursor-up distance from string length.

Distinguish a lost list anchor from lost prompt ownership; the existing ghost
write function itself is not an ownership check. Use these explicit states:

| State | Preconditions | Permitted action |
|---|---|---|
| Live | Empty prompt ownership and list anchor proven; complete region fits | Reflow/redraw the bounded list and synchronize its marker with the ghost |
| PromptLocal | Empty prompt ownership proven, but list anchor lost or region too tall | Do not repaint old rows; identify the current choice locally, leaving the list as a historical snapshot |
| Deferred | Prompt ownership unknown, a card/foreground process owns input, or the next safe boundary has not arrived | No recommendation drawing, prompt restoration or candidate-route installation; wait for a newly proven empty prompt |
| Draft | A candidate was accepted or the user started entering text | Preserve input and existing edit/submit ownership; do not cycle or republish a list over it |

On resize invalidate the old geometry first; only reflow after regaining the
required proofs, keeping the snapshot IDs and active choice. The transition
from Deferred must revalidate the pending snapshot against current source
eligibility and generation, not replay an old RestorePrompt action. The live
display lease is suspended while other input owners are active; do not alter
their route. Candidate cycling and acceptance resume only after a safe lease
is re-established. Remove old active decoration only where safe, never erase
unknown rows, and never let a deferred fallback write into another owner's
screen. Prior output is a historical snapshot, not another live selector.

Tab freezes the selected ID, exact question and existing binding. Cycling,
late results and later numbering changes cannot change an accepted draft.
Do not overwrite nonempty input. Direct Enter submits the currently selected
ID, not a stale first candidate. Existing binding and permission checks apply.

## Short questions and future-design boundary

For health candidates, introduce Cosh-session-specific localized templates
where they retain the original diagnostic scope. Both displayed and accepted
questions use that template. Preserve target identifiers and necessary
qualifiers. Keep Bash `cosh --ask` text, quoting and natural command wrapping,
and explicit `/health` presentation unchanged.

For personal candidates, keep the actual `prompt_text` as both display and
request text; wrap long existing content without truncation. Do not hide a
long request behind a new short label or add a model call to rewrite it.
Independent acceptance of the future short-question/context protocol is
required before shortening personal candidates. The existing health report
binding and personal frozen binding are not evidence that recursive source
permissions, result-driven withdrawal or final outbound revalidation have
already been implemented.

Existing successful-result behavior and permissions must not regress, but
this work does not implement new history collection, source controls,
generation budgets, semantic value filtering, cross-process context recovery
or a new default candidate limit. First-use privacy notices, `--ask` compact
startup, guidance controls and AI-disabled behavior keep existing contracts.

## Replace and retire superseded code

The following is a migration inventory based on the inspected source, not a
claim that the currently used code is already dead. Recheck all production,
test, import/re-export and internal-worker callers on the execution base.
Replace owners in the implementation change, then remove orphaned paths;
do not ship new rendering beside a disabled legacy implementation.

| Existing responsibility | Disposition when the unified path is connected |
|---|---|
| `ui/guidance.rs::style_question_invitation` and its Cosh single-invitation append branch | Remove when Cosh startup uses the unified writer; retain scenario-specific facts projection |
| `PreparedStartupSuggestions::personal_candidates` and the personal-only use of `write_startup_suggestion_card` | Replace with the final shared snapshot and one writer for both initial and late health paths; remove old wrappers or unused notice-card construction |
| `visible_personal_candidates` | Remove if snapshot binding construction no longer needs it; migrate its test-only startup imports rather than keeping test-only obsolete API alive |
| `PreparedHealth.invitation`, `selected_invitation` test helper and facts-versus-invited double rendering in `write_session_health` | Replace primary-invitation preparation with health eligibility/batch-claim state and one facts rendering; move guide revalidation to publication, retaining facts sanitization |
| `discard_health_ghost` reconciliation after separate health rendering | Replace with one final snapshot installation; remove only once no caller depends on it for invalidation or active-choice preservation |
| Session suppression claim's primary-only `selected`/`eligible` response and its worker flag/dispatch/IPC decoder | Replace with the in-process batch result; retire the worker transport and its tests together, keeping deterministic fail-closed witnesses for lock contention and persist failure |
| `health_candidate_for(Target::Cosh)` formatting/admission coupling | Move Cosh question construction/admission to the session question owner; simplify the old dispatcher only if no remaining caller needs it; preserve control-character/length and eligibility checks |

Explicitly retain Bash `health_candidate`, `style_invitation`, quoting,
login/failure invitations and its `GuideStore` claim path. That is distinct
from the session health suppression batch claim; its single-login semantics are
not a reason to keep an unused session primary-only response. Retain `Target`
or equivalent explicit scenario projection, including Cosh facts, rather
than deleting Cosh context handling with the obsolete invitation formatter.

Also retain `default_login` notice behavior, shared health metrics/findings
and explicit `/health` try-item rendering. `HEALTH_MAX_VISIBLE_PROMPTS`,
`try_lines` and `primary_health_prompt_suggestion` still have relevant shared
or on-demand consumers. The generic `RecommendationPanelModel` and
`write_recommendation_panel` are used by post-command recommendation runtime;
the shared notice panel also serves welcome/privacy/upgrade notices. Neither
whole renderer is retired just because startup no longer uses its old card.

Migrate tests of removed presentation functions to the unified public outcome,
keeping the underlying suppression, guide, input and binding regressions.
Remove unused imports/re-exports, localization IDs/translations and obsolete
fixtures only after proving that no remaining entry point needs them. Never
delete assertions merely because the old helper disappeared, add dead-code
allowances, or retain test-only callers to disguise production orphans.

## Acceptance and rollback

The [plan](startup-suggestion-unification-plan.md) defines focused tests and
real-PTY evidence. Required invariants include one visible/selectable pool,
0/1/2/3 candidates, duplicate text with distinct IDs, exact Tab/Enter binding,
safe switching after line wrapping, nonempty draft preservation, suppression
claims only for published health choices, and no changes to Bash or `/health`.

Check Chinese/English, 40/80/100 columns, color/NO_COLOR/plain, late health,
resize, intervening shell output, cancellation, guide changes and unavailable
sources. Real ALinux4 PTY screenshots and casts must come from the changed
binary; HTML mockups are design evidence only. A fixture is identified as a
fixture, not as a live OOM or personalized inference.

Include 40-by-24 and 80-by-24 terminals, a height reduction while selecting,
and three maximum-supported personal questions. Exercise Live, PromptLocal,
Deferred and Draft transitions: foreground/card ownership must cause zero
recommendation writes, while the next safe prompt resumes only a current
snapshot. Check distinct candidate IDs sharing one suppression fingerprint,
one persistence per batch, write failure and reply-timeout outcomes. The
migration inventory must finish with no obsolete production path or unused
test export; document each intentionally retained shared consumer.

Rollback the implementation commit without migrating user state. Keep the
existing suppression record format and conservative claim behavior; do not
delete privacy settings or user history. A failure of safe redraw or
identity consistency blocks product acceptance rather than being concealed
by a screenshot-only change.
