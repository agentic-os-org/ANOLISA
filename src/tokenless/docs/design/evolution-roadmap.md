# Tokenless Evolution Roadmap

[中文版](evolution-roadmap_zh.md)

Canonical reference for the evolution of the Tokenless unified compression
pipeline. The section numbers cited across the crates (`§4.1`–`§6`), the
design principles, and the milestone markers (`M1`, `M4`) refer to this
document. It consolidates the roadmap as encoded in the shipped crates and
the merged implementation PRs; status is current as of Tokenless 0.7.14,
including the post-tool pipeline restructure (PR #2974) and the protocol
v2 lifecycle (PR #2978) merged after that release.

## Goal

One shared Rust compression pipeline serving the CLI hooks, the in-process
Runtime, and the framework adapters. A single versioned compatibility
boundary replaces adapter-specific payloads: protocol v2 carries four typed
lifecycle operations — BeforeModel, PreTool, PostTool, and Retrieve — in
attributed envelopes, so an adapter builds only the operation-specific
request and gets back the typed result facts it needs to build its
host-specific envelope. Protocol v1's generic `CompressionRequest` /
`CompressionResponse` pair (shipped in 0.7.13) is superseded and no longer
parsed. UI or business objects that must remain unmodified never enter the
protocol.

Compression stays an optional optimization throughout: no pipeline failure
may fail the request, and every non-applied outcome emits the original
content unchanged.

## Design principles

The principles are numbered as the shipped code cites them; numbers without
a shipped citation are intentionally not restated here.

- **Principle 2 — Route by content, constrain by seam.** A compressor runs
  only when it supports the detected content type, runs at the request's
  seam, and the adapter declares every capability it needs. Every
  capability defaults to `false`, so an adapter that declares nothing gets
  passthrough rather than an unemittable candidate: a host that cannot
  replace model-visible output never runs a response-shaping compressor,
  and the stash attaches only where the host publishes a retrieve tool, so
  retrievable-lossy markers are never dead ends. The shipped JSON domain
  compressor (§5.3) runs at `post_tool` and gates on `replace_output`: it
  still runs on hosts without a retrieve tool and claims its recoverability
  from what actually happened — truncations that could not be stashed are
  reported as `unrecoverable`, and a lossy candidate is rejected outright
  whenever no trusted Retrieve path exists (principle 5).
- **Principle 5 — Explicit recoverability claims.** Every applied
  transformation reports its recovery state — `lossless`, `retrievable`, or
  `unrecoverable` — and a lossy candidate is rejected
  (`recoverability_unavailable`) whenever the host publishes no trusted
  retrieve tool.
- **Principle 6 — Fail-open, bounded diagnostics.** A failing compressor
  never fails the request; the first failure is kept as a diagnostic bounded
  to `DIAGNOSTIC_MAX_BYTES` (4 KiB), and `output` always carries exactly
  what the adapter must emit.

## Architecture (§4)

### §4.1 Protocol boundary

`tokenless-protocol` defines the compatibility boundary. Protocol v1
(shipped in 0.7.13, PR #2783) carried a generic
`CompressionRequest` / `CompressionResponse` pair with `protocol_version`,
the `Seam`, `Capabilities`, `Disposition`, and `Reversibility` types.
Protocol v2 (PR #2978) supersedes it with four typed lifecycle operations —
`before_model` (model-bound tool declarations), `pre_tool` (tool arguments
before execution), `post_tool` (one completed tool result), and `retrieve`
(one visible stash marker) — carried by `RequestEnvelope` /
`ResponseEnvelope` together with the request `Attribution`. Compatibility
rules:

- Payloads parse strictly: unknown fields are rejected rather than ignored,
  so every wire change is deliberate.
- `RequestEnvelope::from_json` / `ResponseEnvelope::from_json` validate the
  version before the shape, so an unsupported version reports
  `UnsupportedVersion` instead of a misleading shape error.
- A response must carry the operation its request selected
  (`OperationMismatch` otherwise); an incompatible shape requires a new
  `protocol_version`, never a parallel adapter-specific payload.
- In-process callers use the operation-specific payload types directly; the
  envelopes exist only for CLI and other cross-process transports.

### §4.2 Content detection and domain dispatch

The Runtime's `post_tool` module carries the protocol `ContentType`
taxonomy (`json`, `search_results`, `build_log`, `stack_trace`, `diff`,
`html`, `tabular`, `source_code`, `plain_text`, `unknown`) and the
deterministic bounded-cost detector. Phase one dispatches only the JSON
domain to a compressor; every other detected domain passes through
unchanged until its compressor is deliberately wired.

Detection is a pure function of the content and never fully parses any
format; expensive parsing stays inside the selected compressor. The
inspection bound is the leading 64 KiB prefix plus, for the JSON bracket
sniff alone, at most a trailing 64 KiB window; line-based checks stop
after 200 lines. Checks run from the most distinctive shape to the most
general, and detection is conservative by design: HTML is only a document
that starts as one, source code needs a shebang or several
declaration-keyword lines, and binary-like input is `unknown` (milestone
M4 policy: ambiguous fragments are not classified).

### §4.3 PostTool execution and end-to-end arbitration

The Runtime's `PostToolPipeline` takes a post-tool request through bounded
detection, domain dispatch, and one final arbitration that compares the
original and the candidate once. A candidate that does not remove
normalized tokens, violates required reversibility, or exceeds the timeout
budget is rejected as a whole; its tentative Stash writes are rolled back
by `(key, generation)` and the original content is emitted unchanged. The
request also carries the content origin observed by the adapter
(`ContentOrigin`): the origin selects the truncation thresholds — command
output and API/file content differ by more than an order of magnitude —
and file content passes through untouched.

### §4.5 Adapter boundary

Adapters own their private host contracts. Each lifecycle request carries
only the operation-specific facts, and the response carries the typed
result (`disposition`, detected `content_type`, `applied_operations`,
`recoverability`, `before_tokens`/`after_tokens` with `tokenizer_id`,
`stash_keys`, and bounded tool-error context) the adapter needs to build
its host envelope. Adapters need no local fallback logic: `output` is
always emittable.

### §4.6 Seams

The agent loop exposes four lifecycle seams, each one protocol operation:
`before_model` (model-bound tool declarations, e.g. schema compression),
`pre_tool` (tool arguments before execution, e.g. RTK command rewrite),
`post_tool` (the primary compression seam), and `retrieve` (restoring one
visible stash marker, authorized against the markers visible at retrieval
time). Only Stash keys of committed, applied results appear in a response;
rolled-back candidates never leak keys.

## Decisions and contracts (§5)

### §5.1 Token counter decision

All token counts use the character-class heuristic `heuristic-v1` (CJK ≈ 1
token per char, other ≈ 1 token per 4 chars), implemented once in
`tokenless-protocol` and re-exported by `tokenless-stats` — not a provider
tokenizer. Counts
are normalized tokens for arbitration and attribution, not billing
estimates. Any change to the estimator's character classes or ratios
requires a new counter ID, and rows produced under different IDs must never
be merged into one series without an explicit per-counter breakdown.

### §5.2 Routing contract

Unknown or ambiguous content routes to passthrough, as does any seam
without an implementation yet. Detection routes only record-shaped JSON
(`{...}` / `[...]`) to the JSON compressor; scalar roots pass through
unchanged, and detected domains without a wired compressor pass through
until their compressor lands. Misclassification degrades to the fail-open
passthrough path by design.

### §5.3 Response cleanup behind the PostTool pipeline

The pre-existing JSON response cleanup is implemented by the JSON domain
compressor (`JsonCompressor` in `tokenless-compressors`), and the shared
path behind the `post_tool` lifecycle operation, the CLI
`compress-response` command, `TokenlessRuntime::compress_response`, and
the Python binding routes through the Runtime-owned `PostToolPipeline`.
One timeout budget (10 s in-process) guards the run; on expiry the
original is returned and Stash writes are rolled back. Recoverability is
claimed from what actually happened: no truncation → lossless, all
truncations stashed → retrievable, otherwise → unrecoverable.

### §5.4 Single external-hook entry point

The four decisions previously duplicated across the common Python hooks and
the CLI subcommands — JSON detection, tool threshold selection, TOON
selection, and final size acceptance — move into one shared seam router:

- a `tokenless compress` subcommand carrying protocol envelopes (stdin
  `RequestEnvelope` → stdout `ResponseEnvelope`) and the in-process Runtime
  lifecycle methods, both routed through the same entry point;
- external hooks become envelope-only adapters that build one request,
  spawn at most one `tokenless` subprocess, and translate the response into
  their host's envelope;
- adapter contract fixtures cover the five behavior classes (passthrough /
  replacement / no-savings / timeout / malformed) per migrated agent;
- new routing behavior is gated by a runtime configuration toggle, default
  off, introduced with the wiring change.

Status: shipped in 0.7.14 (PR #2844). The common Python hooks
(`compress_response_hook.py`, `compress_schema_hook.py`) are migrated to
the unified entry and now send protocol v2 envelopes (PR #2978); codex /
hermes / openclaw / dsh / SDK adapters keep their current direct-API paths
until migrated.

### §5.5 Statistics migration

Attribution columns (`agent_id`, `session_id`, `tool_use_id`) and retrieve
events landed in the statistics schema (PR #2885); protocol v2 derives the
stats rows from operation results in the core, and attribution reaches
statistics with the request envelope instead of separately (PR #2978). The
legacy dry-run measurement channel (`CompressResult.compressed_output`,
which records the predicted candidate text) persists for the adapters still
on the direct API and is removed once they migrate.

### §5.6 Shared vocabulary and parity

CLI, Runtime, and language bindings share one set of disposition names and
wire strings (the protocol `Disposition` enum), and all counting goes
through the same `heuristic-v1` estimator, keeping every arbitration on
identical numbers. The milestone M1 exit gate requires CLI and Runtime to
agree on this vocabulary; behavior parity is asserted across the five
behavior classes for every migrated agent.

## §6 Compressor pack

Content-domain compressors live in `tokenless-compressors` as stateless
engines that return complete outcomes; the Runtime owns content routing,
final arbitration, and Stash commit or rollback. Phase one wires only
`JsonCompressor` into the PostTool pipeline. A new domain compressor joins
by wiring its engine into the Runtime deliberately, never speculatively —
unwired domains pass through unchanged until then.

## Milestone markers

- **M1** — exit gate: CLI and Runtime agree on the shared disposition
  vocabulary (§5.6). Met when response compression moved behind the
  registry and the Runtime's pre-protocol disposition enum was retired.
- **M4** — conservative detection policy: ambiguous fragments are not
  classified (§4.2). Encoded in the shipped detector.

## Implementation status

| Section | Deliverable | Status | Reference |
|---------|-------------|--------|-----------|
| §4.1 | Protocol boundary: v1 compression envelope, superseded by the v2 lifecycle operations | v1 shipped in 0.7.13; v2 merged post-0.7.14 | PR #2783, PR #2978 |
| §4.2 | Content taxonomy, detector, domain dispatch | Shipped in 0.7.13, restructured post-0.7.14 | PR #2788, PR #2974 |
| §4.3 | Runtime-owned PostTool execution and arbitration | Shipped in 0.7.13, restructured post-0.7.14 | PR #2799, PR #2974 |
| §5.3 | Response cleanup behind the PostTool pipeline | Shipped in 0.7.13, restructured post-0.7.14 | PR #2816, PR #2974 |
| §5.4 | Unified external-hook entry, contract fixtures, runtime toggle | Shipped in 0.7.14 | PR #2844 |
| §5.5 | Statistics attribution migration | Attribution columns and retrieve events shipped; legacy dry-run channel pending adapter migration | PR #2885, PR #2978 |
| §6 | Domain compressor pack (JSON first) | Merged post-0.7.14 | PR #2974 |

Legacy `compress-response` / `compress-schema` / `compress-toon`
subcommands and the pre-pipeline Python helpers stay until every consumer
has migrated to the unified entry; their removal is a dedicated later step.
