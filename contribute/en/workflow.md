# Issues, Roadmap, and Claiming Workflow

[中文版](../zh/workflow.md)

This guide explains how candidate ideas move from GitHub Discussions to accepted Issues, how to claim community work with `/claim`, and what to expect during triage and review. For automation internals and bot implementation details, see [`.github/TRIAGE.md`](../../.github/TRIAGE.md).

## 1. From Community Roadmap to an Issue

Component maintainers post candidate directions in GitHub Discussions under [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap). Before starting work, confirm the scope and claim the task on a linked Issue.

To work on a roadmap item:

1. Search open Issues to check whether someone has already scoped that item.
2. If no Issue exists, open a new Issue linking to the source Discussion and describe the exact scope you propose to implement.
3. Confirm the target version, scope, and acceptance criteria with the component maintainer before writing code.

For bug fixes or small improvements not tied to a roadmap thread, open an Issue directly using the matching Issue Form. Only genuinely trivial changes (such as typo-only doc fixes) may skip an Issue by noting `no-issue: <reason>` in the PR description.

## 2. Understanding Issue Labels

ANOLISA separates component ownership, workflow status, and contribution readiness:

| Label | Meaning |
| :--- | :--- |
| `component:<name>` | Identifies which component owns the issue. |
| `status:needs-triage` | New issue awaiting initial classification and maintainer response. |
| `status:needs-info` | Maintainers need focused details or reproduction steps from the reporter. |
| `status:accepted` | Confirmed in scope and ready for implementation. |
| `status:in-progress` | Actively claimed and being worked on. |
| `status:blocked` | Waiting on another decision, upstream fix, or dependency. |
| `good first issue` | Self-contained task suitable for first-time contributors. |
| `action:helpwanted` | Accepted task where community help is especially welcome. |

Incoming issues may receive initial component routing and context analysis from automated workflows and Agent assistants; human maintainers review and confirm the final scope and status.

## 3. Claiming and Releasing Work (`/claim`)

An issue can be claimed when it carries at least one claimable label (`status:accepted`, `status:ready`, `good first issue`, `help wanted`, or `action:helpwanted`) and does not carry a conflicting workflow status (such as `status:in-progress`, `status:blocked`, or `status:needs-info`).

To claim an eligible issue, post a comment whose **first and only text on the line** is:

```text
/claim
```

To release an issue when you can no longer work on it, comment:

```text
/unclaim
```

(`/assign` and `/unassign` are accepted aliases.)

The claim bot records ownership in a visible marker comment and updates the label to `status:in-progress`. Even when GitHub does not allow assigning an external contributor as a formal repository assignee, the bot marker serves as the authoritative ownership record.

### Keeping Claims Healthy

- Claim one issue at a time so tasks remain available to other contributors.
- Share a brief progress update or draft PR if an issue takes longer than expected.
- If a claimed issue shows no visible progress or reply for **14 days**, automated reminders or maintainers may follow up and release the claim so others can pick it up.

## 4. Submitting and Reviewing Your PR

1. Follow the local build, lint, and test gates for the affected component in the [General Contribution Guide](../../CONTRIBUTING.md).
2. Sign off every commit with `git commit -s` (`Signed-off-by`) and follow [`specs/commit-message-standard.md`](../../specs/commit-message-standard.md).
3. Fill out the PR template completely and link the issue (`closes #<number>`).
4. Address feedback from CI checks, AI-assisted review comments (verify suggestions against the code), and `CODEOWNERS` reviewers.

See [Community Governance](../../GOVERNANCE.md) for response expectations, decision tiers, and how sustained contributions lead to Triager, Reviewer, and Maintainer roles.
