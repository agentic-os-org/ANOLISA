# ANOLISA Specifications Index

Repository-wide engineering standards, review playbooks, and collaboration
specifications routed from [`AGENTS.md`](../AGENTS.md). English documents are
authoritative; `*_zh.md` companions aid review.

## Engineering & Build

| Spec | Purpose | Routed from |
|---|---|---|
| [`development-commands.md`](development-commands.md) | Unified `build-all.sh`, Provider discovery rules, and per-component build/test commands | `AGENTS.md` §2 |
| [`rust-conventions.md`](rust-conventions.md) | Rust comments, `no mod.rs`, workspace deps, `thiserror`/error handling, pre-commit gates, and test module organization | `AGENTS.md` §3 |
| [`component-onboarding.md`](component-onboarding.md) ([中文](component-onboarding_zh.md)) | Mandatory deliverables, registration checklist, and verification for new `src/<name>/` components | `AGENTS.md` §9 |

## Commit, Review & Documentation

| Spec | Purpose | Routed from |
|---|---|---|
| [`commit-message-standard.md`](commit-message-standard.md) | Commit message format, scopes, DCO/trailers, rebase/fixup discipline, version bump & CHANGELOG timing | `AGENTS.md` §6, §13 |
| [`documentation-standard.md`](documentation-standard.md) ([中文](documentation-standard_zh.md)) | Bilingual naming, directory placement, website/agent endpoints, and doc verification rules | `AGENTS.md` §9 |
| [`change-description-standard.md`](change-description-standard.md) | Reader context and accurate technical/change descriptions | `documentation-standard.md` |
| [`independent-review-guide.md`](independent-review-guide.md) | Reviewer-side self-review checklists and ANOLISA-specific verification angles | `AGENTS.md` §14 |
| [`review-response-guide.md`](review-response-guide.md) | Author-side workflow for triaging AI/human findings and inline reply discipline | `AGENTS.md` §14 |

## Collaboration & Governance

| Spec | Purpose | Routed from |
|---|---|---|
| [`contributor-onboarding-guide.md`](contributor-onboarding-guide.md) | AI-agent onboarding playbook, component quick map, read-only `gh` queries, and governance links | `AGENTS.md` §0 |
| [`issue-claim-and-pr-lifecycle.md`](issue-claim-and-pr-lifecycle.md) ([中文](issue-claim-and-pr-lifecycle_zh.md)) | Ownership-signal precedence, `/claim` × Draft PR semantics, inactivity cadence, and competing PR closure | `AGENTS.md` §15 |

## Component-Specific Suites (Legacy / Scoped)

| Directory | Purpose |
|---|---|
| [`cosh-ng-code-organization/`](cosh-ng-code-organization/) | `cosh-ng` code organization decisions and large-file inventory |
| [`shell-e2e-validation/`](shell-e2e-validation/) | Shell end-to-end validation case definitions |
| [`shell-test-organization/`](shell-test-organization/) | Shell source-heavy test inventory |

> Keep top-level `specs/` flat while repository-wide specs remain under ~15–20
> documents; group logically in this index rather than pre-creating nested
> directories.
