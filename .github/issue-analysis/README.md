# Component Issue source analysis

Repository writers can analyze Issue titles and bodies against a frozen source
snapshot in `agentic-os-org/ANOLISA`. The shared engine validates source
references and priority rules, performs same-agent review, and optionally
publishes a short report and an absent priority label. Analysis uses source
evidence; runtime verification remains in the report's verification plan.

## Enable components

Set the repository Actions variable `ISSUE_ANALYSIS_COMPONENTS` to exact
comma-separated component IDs, for example:

```text
sec-core,cosh-ng
```

An unset or blank variable disables analysis. Start with `sec-core` to retain
the existing rollout scope, then extend the list through repository settings.
Whitespace around IDs and duplicate IDs are accepted; unknown IDs, internal
components, empty list entries and wildcards fail selection visibly.

The selector reads the Issue's current labels. Exactly one `component:*` label
must map to an enabled public component. Unlabelled, mismatched, ambiguous,
closed or already prioritized Issues are skipped before source collection,
container builds and model calls. Any `priority:*` label protects existing
grading. The same selection rules apply to Router, single and batch entrypoints.

Component identity, source paths and triagers come from
`.github/components.json`. Analysis entrypoints and scope notes are defined in
`.github/issue-analysis/components.json`; all public components have a profile.
The shared `policy.md` reads these notes and applicable source instructions.
Component identity and policy hashes participate in the input fingerprint.
SecCore alone permits source-confirmed V1-only P3 backlog classification.
Other components use the captured GitHub priority descriptions.

Configure `QODER_PERSONAL_ACCESS_TOKEN` as a repository Actions secret. Define
`priority:p0` through `priority:p3` with descriptions in the repository's label
settings. Missing definitions leave ordinary priority decisions unresolved.

## Automatic analysis

Issue Router calls `issue-analysis.yml` after applying a new routing decision,
using `target_ref=main` and `publish=true`. The original dispatcher must have
repository write, maintain or admin access. Dry runs, shadow mode, low confidence,
manual component conflicts and repeated routing decisions do not start analysis.
A later routing-notification failure does not block an applied route's analysis.
Cancelled runs do not start analysis.

Changing the component variable affects subsequent workflow runs. Historical
Issues are selected through an explicit batch run; changing the variable does
not start a scan.

## Manual single Issue or historical batch

In Actions select **AI / Issue Analysis**, then **Run workflow**:

- `mode`: `single` (default) or `batch`.
- `issue_number`: positive Issue number for single mode.
- `max_issues`: batch limit, default 10, accepted range 1–25.
- `target_ref`: source branch, tag or full SHA; defaults to `main`.
- `publish`: defaults to `false`; enable to publish reviewed results.

```bash
gh workflow run issue-analysis.yml \
  --repo agentic-os-org/ANOLISA --ref main \
  -f mode=single -f issue_number=1234 -f publish=false

gh workflow run issue-analysis.yml \
  --repo agentic-os-org/ANOLISA --ref main \
  -f mode=batch -f max_issues=10 -f publish=true
```

The batch selects open Issues by enabled component label, in configured
component order and oldest-first order within a component. It scans at most
ten pages of 100 candidates per component and records a reached scan boundary
in the selection summary. It stops after selecting `max_issues` and runs at
most two Issue workers concurrently. Matrix jobs expose each Issue's outcome;
one failure allows the remaining selected Issues to finish.
All workers in a batch use the same resolved source SHA and recheck eligibility
before preparing inputs. Completed analyses are recognized by trusted bot
records bound to the Issue content, component, source SHA, policy and priority
definitions. Published refutations count as completed even without a priority.
Validated results outside publication scope record a short completion receipt
with the Actions report link. Batches skip matching completions, and workers
recheck them before model work. Use **Re-run failed jobs** to reconcile failed
or uncertain publication writes. The writeback preview lists completion operations,
and their receipt is read back before the publisher reports success.

Use the same full source SHA when continuing a historical sweep. Changing the
source, Issue content, component policy or priority definitions makes the result
eligible again. Single-Issue mode permits an explicit reanalysis of completed
inputs. Report-only batches write no completion record, so they can select the
same unprioritized Issues again.

The existing `sec-core-issue-analysis.yml` manual inputs and script entrypoint
delegate to the shared implementation and use the same component variable.
`--ref` selects workflow code; `target_ref` selects analyzed source.

## Reports and publication

Reports capture claims, exact source evidence, priority definitions, same-agent
review and a separate verification plan. Snapshots and bounded failure
diagnostics are retained as Actions artifacts for seven days. The model receives
read-only source and no GitHub credential or shell tool.

Publication runs separately from the model. It rechecks authorization, Issue
content, state, labels and priority definitions, then verifies its writes.
Discussion activity does not change the analysis inputs. An intervening edit
to the selected previous report blocks overwriting it.
SecCore retains its existing report markers for safe retries.

Use **Re-run failed jobs** for failed analysis or publication. Publication can
reuse validated artifacts from an earlier attempt, including partial label
writes. Redispatching a repeated Router decision does not retry its analysis.

## DingTalk result notifications

Router notifications continue to announce new ownership decisions. After
publication, a separate job sends a result card with the component, priority,
conclusion, impact, next action, triagers and report link. P0/P1 results request
maintainer verification. Unresolved scope or review objections produce a
needs-review card. Other out-of-scope or inactive results remain quiet.
Report-only runs and eligibility skips send no result card.

Before each delivery, including a retry after explicit rejection, the job
re-reads the linked bot report and compares its complete body with the validated
publication artifact. An edited or deleted report blocks delivery and requires
fresh analysis. Already delivered decisions remain quiet.

The job uses the existing `DINGTALK_WEBHOOK` and `DINGTALK_SECRET` secrets and
the same signed transport as Router notifications. With no webhook configured,
result notification is disabled and its audit records that status. Model jobs
receive neither DingTalk nor GitHub write credentials.

A separate bot comment records delivery state, leaving the analysis report
untouched. Its key identifies the component and public decision, so rerunning
an already delivered result stays quiet. Explicit API rejection permits a
notification-only retry through **Re-run failed jobs**. Transport timeouts,
unreadable responses and interrupted receipt writes retain an uncertain state;
automatic resending stops to avoid duplicating a potentially delivered card.
Inspect the receipt and DingTalk delivery before resolving it to `delivered`
or, after confirming no delivery, `rejected`; then rerun the failed job.
Notification audits are retained for seven days alongside publication artifacts.

## Local checks

```bash
node --test .github/scripts/sec-core-issue-analysis.test.js \
  .github/scripts/issue-analysis.test.js .github/scripts/issue-triage.test.js \
  .github/scripts/issue-claim.test.js
python3 -B .github/scripts/test_dingtalk_notification.py
```
