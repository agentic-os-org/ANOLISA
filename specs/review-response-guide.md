# ANOLISA Review Response Guide (Author Side)

> Author-side workflow for triaging and replying to PR review findings, routed
> from [`AGENTS.md`](../AGENTS.md) §14.
> For reviewer-side methodology (deeper self-review checklists beyond CI gates),
> see [`independent-review-guide.md`](independent-review-guide.md).

## 1. Triage Before Acting

AI reviewers (`qoderai`, `qoder`) generate findings automatically. Before fixing:

1. **Verify the claim**: Read the actual code at the cited line. AI reviewers hallucinate — they may reference wrong line numbers, misread logic, or flag non-issues.
2. **Assess severity**: Only P0 (security) and P1 (correctness) warrant immediate code changes. P2 (style/docs) can be batched. P3+ can be deferred.
3. **Check if pre-existing**: If the issue existed before this PR, it needs `Fixes:` attribution to the original commit. If it is a design limitation (not a bug), reply with rationale rather than patching.

## 2. Response Format

When replying to review findings:

- **Fixed**: State what was done and which commit it landed in.
- **Deferred**: Explain why it is out of scope for this PR and what the plan is.
- **Not a real issue**: Explain why the finding is incorrect or not applicable. Cite specific code/design rationale.

Never blindly accept and fix every AI finding. Some are false positives, and some are architectural suggestions that require design discussion.

## 3. Inline Reply Discipline

- Every inline review comment MUST receive exactly one thread reply via `in_reply_to`.
- Reply at the same time as the code fix push — never push silently.
- For stale comments resolved by a newer revision: reply `"Resolved in latest revision"` (one sentence).
- Never duplicate replies on already-answered threads.
- Before replying, query the API to identify which comments lack a response — avoid both omissions and repetition.

## 4. Common False Positive Patterns

- `"X is not tested"` when X has a default impl that returns `Err` (intentional no-op).
- `"Missing error handling"` when the `?` operator already propagates.
- `"Config field unused"` when it is reserved for a future phase.
- `"Version mismatch"` referencing stale file contents from a previous push.
