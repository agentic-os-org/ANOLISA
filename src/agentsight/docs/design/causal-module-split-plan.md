# Causal Module Split Plan

## Status

Proposed on 2026-10-09; blocking rule satisfied for additions to
`src/server/causal.rs` until executed. Tracked for execution before the next
feature lands in the causal pipeline.

## Context

`src/server/causal.rs` has grown to 2,237 lines (2,180 before the contra
anchor landed), far past the AGENTS.md module budget (< 500 lines excluding
tests) and past the 2,000-line threshold at which the rule requires a split
plan before any addition. The file currently holds seven separable concerns,
each with a clear internal boundary (the section markers in the file):

| Concern | Contents (current sections) | ~Lines |
|---|---|---|
| Cache | `CausalCacheEntry`, `causal_cache`, `next_cache_tick` | 40 |
| Wire types | `CausalRequest/Node/Edge/Case/Finding/Contra/Response` | 120 |
| Evaluator output | `StepVerdict`, `Verdicts`, `Oracle`, `OracleAndVerdicts`, `Attribution`, `AlternativeAttrib`, their parsing and `normalize_attrib` | 210 |
| Endpoint + pipeline | `parse_id_kind`, the actix handler, `run_pipeline` | 400 |
| Prompts | the §5 prompt templates | 220 |
| Claim review | `ClaimReviewItem`, `ClaimReview`, application to the case | 110 |
| Evidence rendering + gating | `render_call_evidence`, `render_findings`, `gate_by_evidence`, `is_alleging` | 220 |
| Transcript access | `open_existing_read_only`, `resolve_to_session_id`, `load_trajectory`, `probe_atif_column`, `find_session_column`, `session_matches`, `slice_round`, step accessors, `extract_task`, `render_steps` | 350 |
| Case build + panel | `build_case`, `build_contra`, `final_delivery`, `quote_appears_in`, text helpers (`truncate`, `clamp_chars`, `collapse_whitespace`) | 290 |

## Decision

Split into a `src/server/causal/` submodule directory, one file per concern:

```
src/server/causal/
  mod.rs             — wire types, endpoint, pipeline orchestration; re-exports
  cache.rs           — in-memory cache
  attribution.rs     — evaluator output types, parsing, normalization
  prompts.rs         — the prompt templates
  claim_review.rs    — semantic claim review
  evidence.rs        — deterministic layer rendering and gating
  transcript.rs      — trajectory/session loading and step accessors
  case.rs            — build_case + build_contra + the delivery anchor
```

`mod.rs` re-exports every name that is `pub` today, so `use
crate::server::causal::…` keeps compiling for the handlers and no HTTP
contract changes. `causal_tests.rs` stays as the module's test file at
`src/server/causal/tests.rs` (moved with it), unchanged.

Execution rules:

1. Mechanical moves only: each concern moves in its own commit with no
   edits beyond `use` paths and `pub(crate)` visibility where the split
   requires it; no behavior changes ride along.
2. Order (each step leaves the suite green): `cache.rs` → `prompts.rs` →
   `attribution.rs` → `claim_review.rs` → `evidence.rs` → `transcript.rs`
   → `case.rs` → what remains becomes `mod.rs`.
3. New causal work lands in the target submodule from the start; until the
   split executes, additions to `causal.rs` carry a pointer to this plan in
   the commit message (as the contra-anchor commit does).

## Consequences

- Each file lands near or below the module budget; the largest
  (`mod.rs` endpoint + pipeline) is expected at roughly 400 lines and can
  shed the pipeline body into `pipeline.rs` if it grows further.
- The transcript concern becomes independently reviewable from the
  evaluation concern, which is where most past defects clustered.
- No public API, HTTP contract, or prompt-content change is part of this
  plan; any such change is a separate PR on top.
