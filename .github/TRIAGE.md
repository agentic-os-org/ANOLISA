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
