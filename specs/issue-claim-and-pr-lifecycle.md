# ANOLISA Issue Claim and Pull Request Lifecycle Standard

[中文版（评审参考）](issue-claim-and-pr-lifecycle_zh.md)

> Canonical specification for issue claiming (`/claim`), Draft Pull Requests
> (`isDraft=true`), ready-for-review Pull Requests (`isDraft=false`), competing
> PRs, and issue closure across human contributors, maintainers, and AI agents.
> Both human contributors and AI agents MUST follow this specification.
> This English text is authoritative. The Chinese companion is for reference.

## Document Boundaries

- [`.github/TRIAGE.md`](../.github/TRIAGE.md) documents the repository's GitHub
  Actions mechanics (`Issue Router`, `Issue Claim`, status labels, trusted bot
  markers, and `COMMUNITY_AUTOMATION_MODE`).
- [`specs/issue-claim-and-pr-lifecycle.md`](issue-claim-and-pr-lifecycle.md)
  (this file) is the single normative source of truth for ownership-signal
  precedence, Draft PR semantics, claim inactivity policy, competing PR
  resolution, and agent coordination rules.
- [`CONTRIBUTING.md`](../CONTRIBUTING.md),
  [`CONTRIBUTING_zh.md`](../CONTRIBUTING_zh.md),
  [`contribute/en/workflow.md`](../contribute/en/workflow.md),
  [`contribute/zh/workflow.md`](../contribute/zh/workflow.md),
  [`GOVERNANCE.md`](../GOVERNANCE.md), and
  [`GOVERNANCE_zh.md`](../GOVERNANCE_zh.md) summarize the contributor and
  governance workflow and redirect here.
- [`AGENTS.md`](../AGENTS.md) §15 mandates this specification for AI agents.

This specification distinguishes **implemented repository automation** from
**community, maintainer, and agent policy**. Workflow behaviors described below
reflect `.github/workflows/issue-claim.yml`, `.github/scripts/issue-claim.js`,
`.github/workflows/prelint.yml`, `.github/workflows/pr-opened.yml`, and
`.github/workflows/qoder-review.yml`.

---

## 1. Authoritative Ownership Signals

External contributors often cannot be added to GitHub's native `assignees`
field by the API, whereas maintainers and allowlisted reporters may be assigned
directly. Human contributors, maintainers, and AI agents MUST interpret issue
ownership using the following consistent precedence so no one duplicates active
work or treats unsubmitted drafts as permanent locks:

1. **Authoritative `/claim` bot marker** (highest precedence for community
   claims):
   - Created by `.github/workflows/issue-claim.yml` (`.github/scripts/issue-claim.js`)
     as the **first line** of a comment authored by `github-actions[bot]`:
     `<!-- anolisa-claim owner=<login> state=claimed -->`, accompanied by the
     `status:in-progress` label.
   - Remains authoritative even when GitHub formal `assignees` is empty.
   - A later first-line `github-actions[bot]` marker with `state=released`
     clears the claim.
2. **Direct non-bot GitHub `assignee`**:
   - When no active claim marker exists, a non-bot GitHub `assignee` on the
     issue indicates an active implementer (per `.github/TRIAGE.md`, assignees
     represent active implementers, not notification targets).
3. **Ready-for-review open PR (`isDraft=false`)**:
   - An open non-draft PR that links the issue via a closing keyword
     (`closes #<n>`, `fixes #<n>`, or `resolves #<n>`) represents active
     in-flight delivery for that issue.
4. **Non-locking signals**:
   - Free-text comments (for example, "I'll take a look") without an accepted
     `/claim` command do **not** create an exclusive lock.
   - A **Draft PR (`isDraft=true`)** without an active `/claim` lock does
     **not** reserve or lock the linked issue.

---

## 2. Draft PR Semantics (`isDraft=true` vs. `isDraft=false`)

- **Definition**: A GitHub Draft PR (`isDraft=true`) is the author's personal
  work-in-progress (WIP) or early design exploration. It is **not** a formal
  submission for merge-gate review or a delivery commitment.
- **Formal submission boundary**: A PR enters formal code review, merge-gate
  accountability, and delivery tracking only when the author marks it **Ready
  for review** (`isDraft=false`).
- **Automated workflows vs. formal review**:
  - Repository workflows triggered on `pull_request` / `pull_request_target`
    `opened` (such as auto-labeling in `.github/workflows/pr-opened.yml` and
    commitlint/metadata checks in `.github/workflows/prelint.yml`) still run on
    Draft PRs under their current triggers.
  - Formal human review, CODEOWNERS merge gating, and automated or AI review
    agents MUST NOT treat a Draft PR as ready for formal review or press the
    author for merge-readiness fixes, **unless** the author explicitly asks for
    early feedback in a PR comment (or a maintainer with write access manually
    invokes `@qoder review`).
- **Guidance for contributors**:
  - If you want to reserve an accepted issue while iterating in a Draft PR,
    comment `/claim` on the issue first.
  - If you want to share an early experiment without reserving the issue, keep
    the PR in Draft without `/claim`, and expect other contributors to remain
    free to claim and resolve the issue.

---

## 3. Unified Lifecycle Matrix (`/claim` × `Draft PR` × `Ready PR`)

All human contributors, maintainers, and AI agents MUST apply the same rules
across every combination of claim lock and PR state:

| Community signal state | Holds exclusive issue lock? | Enters formal review? | Handling when linked issue is resolved by another PR or closed as not planned |
| --- | :---: | :---: | --- |
| **1a. Marker-backed `/claim` lock** (trusted `github-actions[bot]` claim marker) within the activity window, no PR yet | **Yes** — do not start duplicate implementation; coordinate in the issue | No (no PR yet) | If inactive past the release window (§4), check in and release with `/unclaim` before reassigning or implementing |
| **1b. Direct non-bot GitHub `assignee`** (no bot claim marker), no PR yet | **Yes** — do not start duplicate implementation; coordinate in the issue | No (no PR yet) | `/unclaim` does not clear marker-free assignments (§4); a maintainer clears the lock by removing the native GitHub assignee and updating `status:*` labels (removing `status:in-progress` and restoring `status:accepted` when ready) |
| **2. `Draft PR` (`isDraft=true`) only, no active `/claim` lock** | **No** — personal draft does not reserve the issue; others may `/claim` and submit a formal PR | **No** — skip unsolicited formal review | Treated as a naturally superseded draft; **does not block issue closure or completion tracking**. A brief courtesy note is optional; do not require force-closing the author's draft |
| **3. Active `/claim` lock + `Draft PR` (`isDraft=true`)** | **Yes**, while the claim remains active — staying in Draft does not extend the claim past the inactivity window | **No** — waits for **Ready for review** (unless author requests early feedback) | Once the lock is released after inactivity (§4) and another formal PR resolves the issue, the remaining unconverted Draft PR follows Row 2 (does not block closure) |
| **4. Ready-for-review open PR (`isDraft=false`) linking `closes/fixes/resolves #<n>`** | **Yes** — active delivery in flight; coordinate on the PR instead of opening duplicate work | **Yes** — enters formal review and merge gates | **Competing formal PR MUST be explicitly resolved**: after the winning PR merges (or the issue closes as not planned), a maintainer or component owner MUST thank the author, link the merged PR or decision, and close the superseded PR |

---

## 4. Claim Inactivity Window and Release Policy

`.github/workflows/issue-claim.yml` does **not** automatically expire claims on
a schedule. How an ownership lock is released depends on how it was created:

- **Marker-backed `/claim` lock**: released via `/unclaim` (or its alias
  `/unassign`) by the claimant or by a repository maintainer (`OWNER`, `MEMBER`,
  or `COLLABORATOR`). The workflow writes the `state=released` marker, removes
  `status:in-progress`, restores `status:accepted` when no other workflow
  status label remains, and attempts to remove the formal assignee.
- **Direct non-bot GitHub assignment (no bot claim marker)**: `/unclaim` is a
  no-op (`No active claim to release.` in `.github/scripts/issue-claim.js`) and
  removes neither the assignee nor `status:in-progress`. To hand off or clear
  such an issue, a maintainer MUST remove the native GitHub `assignee` and
  update labels directly (removing `status:in-progress` and restoring
  `status:accepted` when no other blocking `status:*` label remains).

Both follow the same community inactivity cadence:

- **7 days of inactivity (status check)**: If an active claim or assignment has
  no visible progress for 7 calendar days (no status update in the issue thread
  and no ready-for-review PR; an unchanged Draft PR does not count as a formal
  submission), a maintainer or coordination agent may post a polite check-in
  asking whether the owner is still working on the issue.
- **14 days of inactivity (release)**: If there is no response or visible
  progress after **14 calendar days** from the claim/assignment or last
  progress update (or **7 calendar days after a maintainer status check**,
  whichever is later), a maintainer may post a brief courtesy explanation and
  release the lock using the applicable path above (`/unclaim` for a
  marker-backed claim, or native unassignment plus status label cleanup for a
  marker-free direct assignment).
- **Keeping a claim active**: Owners who need more time only need to post a
  short progress update in the issue thread. Claimants who can no longer work
  on an issue should post `/unclaim` (or unassign themselves / ask a maintainer
  to unassign) promptly so others can pick it up.

---

## 5. Competing PRs and Issue Closure Hygiene

When a linked issue is resolved by a merged PR (`completed`) or closed without
code changes (`not_planned` / `status:declined` / `status:duplicate`):

1. **Superseded ready-for-review PRs (`isDraft=false`)**:
   - MUST be explicitly closed by a maintainer or component owner (or by the
     author).
   - Leave a clear, polite closing comment thanking the contributor and linking
     the merged PR or closure decision.
   - An open superseded **non-draft** PR linking a closed issue represents
     incomplete closure hygiene and MUST NOT be left dangling silently.
2. **Lingering unconverted Draft PRs (`isDraft=true`)**:
   - Do **not** block closing the linked issue or marking the issue's delivery
     complete.
   - Maintainers or component owners MAY leave a single brief courtesy comment
     (for example: *"This issue has been resolved by #<pr>. Feel free to close
     this draft or repurpose it if you have follow-up changes."*), without
     requiring force-closure of the contributor's personal draft.

---

## 6. Standalone PRs (No Linked Issue)

When a PR does not link a tracking issue (for example, a proactive fix,
refactor, or `no-issue` documentation update):

- While `isDraft=true`, it is open work in progress and does not trigger formal
  review unless the author asks for early feedback.
- As long as the PR remains **open** on GitHub (whether `isDraft=true` or
  `isDraft=false`), tracking systems and agents MUST NOT mark it as completed
  or cancelled. Resolution requires either merging or explicitly closing the PR
  on GitHub.
- Maintainers may close clearly abandoned standalone PRs after pinging the
  author in the PR thread and allowing a reasonable response window (default:
  14 calendar days).

---

## 7. Mandatory Rules for AI Agents and Automation

AI agents operating on ANOLISA issues or PRs MUST enforce the rules above
consistently:

1. **Before claiming or implementing an issue**:
   - Check **all** authoritative lock signals in §1: the latest trusted
     `github-actions[bot]` claim marker (`<!-- anolisa-claim ... -->`),
     `status:in-progress`, direct non-bot `assignees`, and any open
     **non-draft** PR (`isDraft=false`) referencing the issue.
   - Do NOT start duplicate implementation when an active lock exists, unless
     the lock has been explicitly released (via `/unclaim` for a marker-backed
     claim, or native unassignment and status cleanup for a marker-free direct
     assignment) or a maintainer explicitly instructs otherwise.
   - Do NOT treat an unclaimed `Draft PR` (`isDraft=true`) as an exclusive lock
     blocking issue pickup.
2. **During code review**:
   - Check `isDraft` / `draft` before starting automated or agent-assisted
     reviews.
   - Do NOT post unsolicited formal reviews, change requests, or review
     scorecards on a `Draft PR` unless the author explicitly requests early
     feedback or a maintainer with write access explicitly invokes review.
3. **During issue closure and auditing**:
   - Preserve and inspect `isDraft` on every open PR linked to a closed issue.
   - Require explicit closure with a polite explanation for superseded
     **non-draft** PRs (`isDraft=false`).
   - Do NOT block issue closure or delivery verification solely because an
     unconverted `Draft PR` (`isDraft=true`) remains open.
