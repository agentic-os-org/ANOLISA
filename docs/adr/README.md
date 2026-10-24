# Architecture Decision Records

[中文版](README_zh.md)

This directory contains all ANOLISA architecture decision records (ADRs),
including system-wide, cross-component, and component-specific decisions.
Records share one numbering sequence; each record identifies its scope.

The English original of each ADR is authoritative. All translations are for
reference only; if they differ, the English original prevails.

| Number | Decision | Scope | Status |
| --- | --- | --- | --- |
| 0001 | [ANOLISA Architecture and Component Boundaries](0001-anolisa-architecture.md) · [中文参考译文](0001-anolisa-architecture_zh.md) | ANOLISA system and monorepo | Proposed |

For record placement, naming, required content, and assets, follow the
[documentation standard](../../specs/documentation-standard.md#321-architecture-decision-records).
Supporting implementation designs remain with their components and link to the
corresponding ADR here.

## Review workflow

1. Open a focused PR with the ADR marked `Proposed`, following the
   [contribution guide](../../CONTRIBUTING.md).
2. Request reviews from affected component owners explicitly. Central ADR paths
   do not automatically request those owners through `CODEOWNERS`.
3. Resolve comments and agree on the decision. Update the acceptance metadata
   below, obtain approval of the final revision, and have a maintainer merge it
   into the official `main`. That merge makes the accepted decision effective.

On acceptance, update the record's metadata:

```markdown
- **Status:** Accepted
- **Date:** YYYY-MM-DD
- **Decision date:** YYYY-MM-DD
- **Decision PR:** <PR URL>
```

Keep `Date` as the original proposal date; `Decision date` is the acceptance
date, and `Decision PR` links to the review. Acceptance does not change the
implementation baseline. Discussion and approvals remain in the PR.

Synchronize the English record, translations, both indexes, and any ADR status
shown in assets. `Accepted` means the architecture is adopted; implementation
may still be pending. Proposed integration labels and dashed connectors do not
change merely because the ADR is accepted.

## Lifecycle

| Status | Meaning |
| --- | --- |
| Proposed | Under discussion; no accepted decision yet. |
| Accepted | Adopted through review and merged into the official `main`. |
| Rejected | Reviewed and explicitly not adopted. |
| Superseded | Replaced by a later accepted ADR. |

Change an accepted architectural decision through a new ADR. Once its replacement
is accepted, mark the old record `Superseded`, link the two records in both
directions, and retain the original rationale. Text corrections, link repairs,
and translation fixes may update the existing record through a PR.
