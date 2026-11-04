# Issue triage and community claiming

ANOLISA separates routing, triage, implementation ownership, and code review.

## Roles

- **Component label** answers where the issue belongs.
- **Triager** gives the first maintainer response and decides the next status.
- **Claimant** is the person actively working on an accepted community task.
- **Code owner** reviews the resulting pull request.

A new issue passes through the ANOLISA Issue Router before a parameterized
GitHub Actions workflow applies the validated result. Assignees represent people
actively implementing the work, not notification targets.

## Status labels

| Label | Meaning |
|---|---|
| `status:needs-triage` | Awaiting the first maintainer decision |
| `status:needs-info` | The reporter needs to add focused information |
| `status:accepted` | In scope and ready for implementation |
| `status:in-progress` | Someone has claimed the work |
| `status:blocked` | Waiting on another decision or dependency |
| `status:duplicate` | Tracked by another issue |
| `status:declined` | Not planned or outside the current scope |

Only one `status:*` label should describe the current workflow state.

## New issue flow

1. The Issue Form may supply a component hint. It is authoritative only for a
   structured report whose author passes the private fast-path policy.
2. The external dispatcher discovers the issue, uses the structured fast path
   when eligible, or requests a component classification with a public summary
   and evidence.
3. The dispatcher invokes `Issue Router` through `workflow_dispatch`.
4. The workflow validates the component against `.github/components.json` and
   preserves any conflicting component label already applied by a maintainer.
5. Decisions below the configured confidence threshold remain non-mutating.
6. An accepted decision adds `component:*`, mentions the component owners,
   posts the public summary, and sends the configured notification.

The workflow does not infer a component from the Issue Form or title, and it
does not rewrite issue titles. It assigns component owners only when the issue
author is authorized by the externally managed `ISSUE_TRIAGE_POLICY` secret;
all other issues remain unassigned until someone claims the work. The secret is
a JSON object whose optional `auto_assign_authors` field is an array of GitHub
logins. An absent secret safely disables automatic assignment.

Only a first-line decision marker authored by `github-actions[bot]` is trusted
for idempotency. Router summary and evidence text is HTML-escaped before
publication so it cannot create workflow markers. Owner notification has a
separate marker on the trusted triage comment: a failed notification remains
pending and is retried by rerunning the workflow. If delivery succeeds but
recording the marker fails, a rerun may deliver a duplicate rather than lose the
notification. HTTP success alone is not treated as delivery: the DingTalk
response must also contain `errcode: 0` before the notification is recorded.
The notification uses a fixed Issue link and displays the GitHub-provided title
separately with Markdown metacharacters escaped.

## Claiming community work

An issue can be claimed when it has at least one of:

- `status:accepted`
- `status:ready` (legacy compatibility)
- `good first issue`
- `help wanted`
- `action:helpwanted` (legacy compatibility)

A fresh claim is rejected when any `status:*` label other than
`status:accepted` or the legacy `status:ready` is present, even if a claimable
label remains on the issue. A maintainer must first move the issue back to a
ready state. Claiming removes the ready-state label before adding
`status:in-progress`.

Commands must be the first and only text on the comment line:

```text
/claim
/unclaim
```

`/assign` and `/unassign` are accepted aliases.

The workflow records ownership in a bot marker and `status:in-progress`, then
tries to make the claimant the formal assignee. GitHub only permits some users
to become formal assignees, so the bot marker remains authoritative when an
external contributor cannot be assigned through the API.

Claim and release markers are accepted only as the first line of a comment from
`github-actions[bot]`. The workflow writes the authoritative marker before
changing labels or assignees. Rerunning an interrupted command reconciles the
remaining mutable state without creating another marker. During reconciliation,
an existing `status:in-progress` is allowed only for the marker owner.

A claimant should release the issue when they can no longer work on it.
Maintainers may release abandoned claims after communicating in the issue. A
release restores `status:accepted` only when no other workflow status remains;
otherwise it preserves that status and reports that the issue is unavailable.

## AgentSight work limits

External contributors may have five active ordinary AgentSight claims and their
five oldest open, non-draft PRs touching `src/agentsight/` awaiting review.
Repository variables `SIGHT_MAX_ACTIVE_CLAIMS` and `SIGHT_MAX_READY_PRS` override
these limits separately; each accepts an integer from 1 to 100 and defaults to
five when unset. Invalid values fail the applicable check rather than disabling
the limit. Other components and Draft PRs do not consume PR slots.

AgentSight maintainers registered in `.github/maintainers.json` and users with
repository `write`, `maintain`, or `admin` permission are exempt from quantity
limits. Organization membership alone does not grant an exemption. The PR check
looks up the author's permissions, not the actor who edited or reran the PR.
The claim handler checks the claimant. The registered list comes from trusted
target-branch files; contributors cannot exempt themselves by editing it in
their PR. Permission lookup failures other than a non-collaborator response
fail the check and require a retry. Maintainer-applied `priority:p0` or
`priority:p1` labels also exempt urgent work. All authors still need validation.

The claim handler checks trusted bot markers across open AgentSight issues,
including claims without a formal assignee. Releasing or closing an issue frees
its slot. Existing claims can still reconcile their labels; the limit prevents
new claims rather than withdrawing ongoing work. Per-issue workflow concurrency
and per-author job concurrency serialize ownership and quota decisions; both
use queued execution so concurrent commands are not silently replaced.

`sight/submission-policy` checks the regression explanation and review slots for
ready AgentSight PRs targeting `main` or `release/**`. Authors describe the
regression case once, updating it when the affected behavior changes. They do
not maintain a SHA, environment, command/result copy, or `Unverified` field on
every push. The workflow reads the live head from GitHub and automatically
includes it and a PR Checks link in its summary. Build/test results come directly
from separate required CI checks, including the existing `Test agentsight`.
GitHub requires those checks for the latest revision; this admission status does
not certify that CI passed. No PR-description write permission or bot edits are
needed. Required runtime verification outside CI, such as real-kernel BPF
tracing, still needs an author report and reviewer confirmation when applicable.

Later ready PRs beyond an external contributor's configured slots fail admission.
Draft PRs and PRs targeting other branches do not consume these review slots.
This check does not change Draft state or prove that claimed regressions ran.

The `pull_request_target` entrypoint and its scripts run from the trusted default
branch revision (`github.sha`). No PR head, merge ref, artifact, dependency, or
contributor script is executed. The token has only contents/read, pull-requests/read,
and statuses/write permissions. Results are published as commit statuses on the
actual PR head SHA; the runner job is named `Refresh AgentSight policies` to keep
it distinct from the required policy status. A PR changing the workflow or policy
cannot replace the trusted execution path. Missing policy files fail the run;
there is no successful bootstrap skip.

Each event refreshes that author's open PRs and every other author sharing an
affected SHA, transitively including their other PRs for independent quota
calculations. This includes the triggering PR when closed, and the previous head
on synchronize events. Commit statuses are shared by SHA: any related PR that
fails keeps the shared status failed, even if another author opens a Draft with
the same commit. Publication is queued under a repository-wide lock to prevent
cross-author overwrites; only the connected PR set is recalculated.

Ready/Draft, open/reopen/close/merge, commits, body/base edits, and priority-label
changes trigger refreshes. Old statuses become pending before calculation; state
is checked before and after publication and changes are retried. API/permission
failures leave known heads in error and fail the execution workflow. A business
rejection instead writes a failed admission status while the trusted execution
workflow succeeds. This separation lets released slots or corrected descriptions
restore admission without another push or a manual rerun of an old workflow.
CI completion is handled by GitHub's separate required checks; the policy does
not wait for CI or duplicate CI results. Execution is asynchronous: wait for the
refresh and CI before merging; API outages require rerunning the failed workflow.

### Activating the merge gate

After merging this implementation into the default branch:

1. Run the trusted workflow on a real fork PR and verify `sight/submission-policy`
   is attached to its current head, including a negative case with missing evidence
   and a same-name passing job added by the fork. Local regressions execute the
   actual workflow script against mocked APIs; they do not certify GitHub's live
   branch-protection behavior or token permissions.
2. Require the `sight/submission-policy` status with GitHub Actions as its expected
   source. Keep fork PR tokens read-only; never enable sending write tokens to
   fork workflows. A same-name job is not the trusted status. GitHub requires both
   a check and commit status to pass when they have the same required name.
3. To enforce workflow identity as well as the status name, use an organization
   ruleset's **Require workflows to pass before merging**, selecting
   `agentic-os-org/ANOLISA`, protected `main`, and
   `.github/workflows/sight-submission-policy.yml`. Requiring an arbitrary job
   name or selecting GitHub Actions alone does not pin a workflow's identity.
   If the organization cannot enforce required workflows, a separately installed
   GitHub App with a source-pinned status is the alternative; do not claim that a
   job-name-only rule protects against workflow replacement. The required workflow
   attests trusted execution; the separate required status controls admission.
   Ordinary evidence/quota rejection must not fail the execution workflow, since
   required workflows do not retrigger on all admission-recovery events.
4. Allow this specific trusted `pull_request_target` workflow in the repository's
   Actions event policy where required. Keep the existing Linux `Test agentsight`
   check required. Compilation and unit tests do not replace real-kernel
   verification for BPF changes. No additional platform build gate is introduced.

Repository/organization settings and a live fork test are rollout requirements;
this PR does not change those settings. The policy does not run for this initial
PR before its trusted entrypoint exists on the default branch.

References: [trusted target events](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target),
[required-check behavior](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
and [required workflow sources](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#require-workflows-to-pass-before-merging).

This policy adds no issue-status transitions or per-issue approval step.
Routing and claim eligibility follow the existing repository workflow above.

Automatic scan creation quotas belong in the external dispatcher. These
repository checks do not prevent GitHub issue creation or impose daily intake
limits, and they do not automatically close or reassign existing issues.

## Automation mode

The repository variable `COMMUNITY_AUTOMATION_MODE` controls the triage and
claim workflows:

- `apply` permits validated triage labels, comments, notifications, allowlisted
  reporter assignments, and claimant assignments.
- `shadow` writes only Action logs and summaries.

Any other value is rejected without changing the issue. This fail-closed
behavior prevents a misspelled incident-response setting from enabling writes.

`Issue Router` also accepts an `apply` input. Set it to `false` for a single
dry run even when the repository mode is `apply`. The default repository mode
is `apply`; set it to `shadow` for incident response or a non-mutating trial.
