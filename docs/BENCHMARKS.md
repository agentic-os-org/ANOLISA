---
title: Benchmarks
description: tokenless is measured in four layers — component, module, scenario, and end-to-end. This page explains what each layer tests and reports the results available today.
---

tokenless compresses the context an agent sends to a model. A single compression
rate does not tell you enough: a compressor can look excellent on one payload and
still drop something the agent needs three turns later. The benchmark is therefore
split into four layers, each covering ground the layer above it cannot see.

Every rate is recounted with tiktoken over the real output instead of the engine's
own report, so every number here traces back to the compressor itself.

## The four layers

| Layer | Scope | Question it answers | Status |
|---|---|---|---|
| **L1** component | One compressor, one standard fixture | Is each compressor correct, and what does it cost? | Results below |
| **L2** module | One round of tool output, five payload types | On payloads an agent really sees, how much is saved and what is lost? | Results below |
| **L3** scenario | A whole conversation, many scenario types | Does information survive across the full message list, not just one response? | Results below |
| **L4** end-to-end | A complete agent task | Can the task still be completed, and what did it cost in total? | Results below |

Each layer has its own harness and its own reports. Numbers are never mixed across
layers, because the layers do not measure the same thing.

---

## L1 component layer

**What it tests.** Each compressor on its own, against the standard fixture for its
input type. Three things: whether the transform matches the spec, what one call
costs, and how that cost grows with input size. Adversarial and round-trip cases
are included so a compressor cannot pass by quietly degrading its output.

**What it does not test.** Whether the saving is worth it. A rate here belongs to
one fixture; it is not an expected value for production traffic.

### Results

`o200k_base` counting, Linux x86_64.

| Compressor | Input | Hook | Compression | Time |
|---|---|---|---|---|
| **ResponseCompressor** | JSON / API / tool responses | PostToolUse | **65.8%** | 46.93 µs |
| **SchemaCompressor** | OpenAI function-calling tool schemas | BeforeModel | **47.3%** | 11.40 µs |
| **TOON encoder** | JSON to compact TOON | after ResponseCompressor | 17.0% (response) / **−2.3%** (schema) | 3.92 – 10.46 µs |
| **RTK rewriter** | shell command output | PreToolUse · Shell | 58.6 – 98.6% | dominated by process startup |

<Callout type="warning" title="TOON grows schema payloads">
TOON saves 17.0% on response payloads but adds 2.3% on tool schemas. It is a
tabular encoding, so it pays off on homogeneous arrays and loses on the deep,
mixed-field shape of a function-calling schema. Enable it per payload type rather
than globally.
</Callout>

### Where truncation caps the cost

ResponseCompressor truncates arrays at the 32nd item by default. Past that point it
stops walking the array, but total time still rises with input size:

| Array size | Time |
|---|---|
| 10 items | 11.51 µs |
| 31 items | 36.84 µs |
| **32 items (threshold)** | **38.18 µs** |
| 33 items | 38.39 µs |
| 100 items | 54.11 µs |
| 1,000 items | 275.04 µs |

Time scales roughly linearly with payload size: 1 KB 5.50 µs, 10 KB 38.62 µs,
100 KB 104.80 µs, 1 MB 815.01 µs.

<Callout type="info" title="High compression on large arrays comes from truncation">
Past 32 items, the saving is the discarded tail rather than a denser encoding of
the whole array. This is why L2 exists: without knowing what was in the tail, a
truncation rate tells you nothing.
</Callout>

### Test coverage

96 tests pass, including 9 RTK protocol tests that run against a real pinned `rtk`
binary rather than being skipped.

| Compressor | Tests |
|---|---|
| ResponseCompressor | 34 (retention, robustness, worst case, regression guards) |
| SchemaCompressor | 25 |
| TOON encoder | 18 (round-trip and adversarial) |
| RTK rewriter | 9 (real execution) |

Coverage here means the enumerated valid values and rules were exercised. It says
nothing about accuracy on arbitrary input or on production tasks.

---

## L2 module layer

**What it tests.** One round of tool output across the five payload types an agent
actually encounters: `json`, `command`, `grep`, `code`, `diff`. The question shifts
from "is it correct" to "what does the saving cost", so every type is scored on
compression, on two independent measures of preserved meaning, and on latency
against a stated basis.

**What it does not test.** Anything that spans turns. If information survives this
response but is needed five turns later, that belongs to L3.

### Compression and preserved meaning

`N` is the number of independent observations, keyed on the payload actually
measured: if a command produces byte-identical output across repetitions, it counts
once no matter how many times it ran.

| Category | Compression ± 95% CI | N | Before (tokens) | Retention | Semantic |
|---|---|---|---|---|---|
| **diff** | **76.0%** [37.8, 97.6] | 5 | 37,775 | 15/25 | **0.64** ⚠ |
| **json** | **44.6%** [18.0, 62.2] | 3 | 1,603 | 8/11 | **0.80** ⚠ |
| grep | 0.0% [0.0, 0.0] | 8 | 776 | 40/40 | 1.00 |
| command | 0.0% [0.0, 0.0] | 2 | 1,429 | 6/6 | 1.00 |
| code | 0.0% [0.0, 0.0] | 9 | 509 | 37/37 | 1.00 |

<Callout type="info" title="Zero-compression rows say nothing about quality">
For `command`, `code`, and `grep` the rate is 0%: output equals input, so perfect
retention follows automatically. Those `1.00` scores mean "nothing changed", not
"nothing was lost".
</Callout>

### Two measures of meaning

Either one alone can lead to the wrong conclusion, so both are used:

| Measure | What it checks | How it is decided |
|---|---|---|
| **Retention** | Are the original's critical facts still present verbatim? | Deterministic substring or regex match |
| **Semantic score (S)** | Of the questions the original answers, how many still get answered? | Per question, scored on the compressed output |

S is scored per question, so losing one fact cannot be made up by guessing another.
Anything below 0.85 is flagged.

**The two types that compress are also the two that lose information.** diff and
json are the only types where compression actually happens, and they are the only
two below the gate. This is not sampling noise but a consequence of the strategy:
array truncation discards tail entries, and referenced facts sometimes live there.

<Callout type="warning" title="Valid output is not complete output">
Compressed output stays well-formed. On the aggressive read path a ~9 KB code diff
comes back as 167 bytes of valid text, with every changed function name and
constant already gone. The caller cannot tell from the output that anything is
missing, which is worse than an outright error.
</Callout>

### Compression depends on the entry point

RTK picks its filter by subcommand, so the same content behaves very differently
depending on how it is called. Measured on one committed code diff of about 9,200
bytes carrying 5 code identifiers:

| Entry point | Output | Compression | Identifiers kept |
|---|---|---|---|
| full read | 9,211 B | 0.0% | 5/5 |
| structured diff filter | ~8 KB | 91% | 5/5 |
| aggressive filter | 167 B | **98.2%** | **0/5** |
| two-line summary | 47 B | **99.5%** | **0/5** |

When reasoning about a specific call path, use the figure for that entry point, not
the aggregate. "diff compresses over 90%" only holds once you name the entry point.

### Latency

| Category | p50 | p95 | p99 | Basis |
|---|---|---|---|---|
| code | 0.006 ms | 0.006 ms | 0.008 ms | in-process |
| json | 0.007 ms | 0.039 ms | 0.049 ms | in-process |
| command | 5.668 ms | 6.114 ms | 6.125 ms | wrapped minus raw |
| grep | 6.089 ms | 6.925 ms | 6.994 ms | wrapped minus raw |
| diff | 9.400 ms | 19.346 ms | 20.239 ms | wrapped minus raw |

In-process compression runs in microseconds. The millisecond figures for
`command`, `grep`, and `diff` come mostly from RTK process startup, not from
compression: the same engine called in-process stays under 0.05 ms at p99. For
`diff`, p99 sits close to p50, which points to a steady fixed cost rather than a
long tail.

<Callout type="info" title="Latency bases cannot be compared across rows">
In-process timing brackets the compress call only. "Wrapped minus raw" is the whole
wrapped process's wall clock minus the raw command. Compare within a basis.
</Callout>

### Whole-task rollup

Three multi-step agent tasks, totalled across every interaction rather than per
response. This is as close to a task view as L2 gets; the real task view is L4.

| Task | Interactions | Before | After | Saved | Rate |
|---|---|---|---|---|---|
| review | 3 | 91,478 | 9,046 | 82,432 | **90.1%** |
| bug-hunt | 4 | 94,008 | 11,576 | 82,432 | **87.7%** |
| api-debug | 4 | 9,354 | 3,600 | 5,754 | **61.5%** |

The two large tasks are dominated by their diff payloads, which is where the 82,000
tokens of savings come from, so the semantic cost recorded above applies to those
two tasks as well. `api-debug` is JSON-led and an order of magnitude smaller; its
lower rate is simply the absence of oversized arrays to truncate.

---

## L3 scenario layer

**What it tests.** A whole conversation, meaning the message list an agent actually
sends to a model, rather than a single response. 37 scenarios (34 from the scenario
suite, 3 from the pipeline suite) across 8 content types, each at a few size steps,
totalling about 3.44M tokens before compression.

**Why this layer is needed.** L2 measures one round. But an agent's context is a
message list built up over turns, and a per-response rate says nothing about what
happens to that list as a whole: information can survive the response it arrived in
and still be gone by the time the model needs it.

**What it does not test.** The feedback loop. A compressed tool result changes what
the agent does next, which changes the next payload; that belongs to L4.

24 of the 37 scenarios enter a compression path; the remaining payload shapes fall
outside the current applicable range (see "Applicability" below).

### Compression and critical-item retention

Only the 24 scenarios that enter a compression path are counted. `N` is the number
of scenarios in that type with an entry point; intervals come from bootstrap across
scenarios. A "critical item" is a fact labelled in advance as must-survive — record
counts, error entries, boundary values — checked by deterministic match against the
compressed output.

| Content type | N | Compression ± 95% CI | Per-scenario range | Critical items kept |
|---|---|---|---|---|
| **logs** | 4 | **91.2%** [79.6, 98.3] | 73.8 – 99.4% | **13/20 = 65.0%** ⚠ |
| **json** | 14 | **82.7%** [68.5, 92.8] | 1.5 – 99.4% | **18/35 = 51.4%** ⚠ |
| agentic | 5 | 41.7% [30.5, 47.6] | 19.4 – 48.0% | 667/694 = 96.1% |
| schema | 1 | 36.9% (collapses to a point) | — | 41/41 = 100% |

Weighting content types equally, retention is **78%**.

<Callout type="warning" title="Do not use the pooled total">
Adding the four types' critical items together gives 739/790 = 94%, but agentic
alone supplies 88% of that denominator (694 items) and happens to retain the best
(96.1%). The two worst types, json (51.4%) and logs (65.0%), together make up only
6.9% and are diluted out of sight. This page reports per content type throughout,
with an equal-weighted mean of 78%, and does not use that 94%.
</Callout>

### Applicability

The tokenless compressors accept structured payloads: JSON tool responses go
through `compress-response`, tool schemas through `compress-schema`. This layer
also records that boundary, and applicability is decided by the payload measured,
not the message role:

| Content type | Scenarios | Enter a compression path | Notes |
|---|---|---|---|
| json | 14 | 14 | structured JSON tool responses |
| agentic (multi-turn tool traces) | 5 | 5 | JSON tool messages within the message list |
| logs | 4 | 4 | structured log arrays |
| schema (tool schemas) | 1 | 1 | function-calling schemas |
| code | 4 | 0 | payload is raw code, outside structured input |
| text (prose docs) | 4 | 0 | payload is raw prose, same as above |
| rag (retrieved context) | 4 | 0 | payload sits in system/user prose: no tool message and no tools array |
| simple | 1 | 0 | same as above |

Bringing these four types into a compression path requires a non-JSON payload path.

<Callout type="info" title="These four are excluded from the compression figures">
They record "outside the applicable range", not "compressed but saved nothing", so
they are not filed as a 0% rate in the table above; conversely, their perfect
retention on an unmodified original is likewise excluded from the retention figures.
</Callout>

### The semantic cost of compression

Both L2 measures apply: deterministic critical-item matching and a semantic probe.
The probe asked 48 questions, of which the original answers 31; **23 (74%)** are
still answered after compression.

The losses concentrate in two kinds of information:

- **Record counts.** `api_responses_500`, `database_rows_1k`,
  `search_results_500` / `_1000`, and `structured_100` can no longer answer "how
  many records in total".
- **Error entries.** `search_results_100` / `_500` / `_1000` can no longer quote the
  error reported in the tool output; at the critical-item level, 15 scenarios drop
  at least one `error_entry`.

Two further scenarios (`number_array_200`, `number_array_1000`) drop the array's
extreme values.

The cause is the same as at L2: array truncation discards tail entries, and counts,
error entries, and outliers often live in the tail. Across a whole conversation this
is exactly the information a later turn depends on, so the loss bites harder than it
does in a single round.

The cost grows with size. One payload shape (structured logs) at four size steps:

| Scenario | Before (tokens) | Compression | Critical items kept |
|---|---|---|---|
| structured_100 | 8,612 | 73.8% | 5/5 |
| structured_500 | 40,491 | 94.5% | 3/5 |
| structured_1000 | 80,291 | 97.2% | 4/5 |
| structured_5000 | 398,120 | **99.4%** | **1/5** |

The trend is not strictly monotonic — the 1K step keeps one item more than the 500
step, because which error entries land in the discarded tail is incidental — but the
direction is clear: the rate climbs by discarding the tail, and whatever critical
items sat in the tail go with it.

### Latency

In-process timing, bracketing the compress call only, excluding process startup and
serialisation. Over the 24 scenarios where compression actually ran:

| Statistic | Value |
|---|---|
| p50 | 0.18 ms |
| p95 | 4.69 ms |
| p99 | **13.28 ms** |

p99 comes from the largest scenario (`multi_tool_100t`, 615K tokens before
compression).

<Callout type="info" title="The report file's p50 is lower because it counts scenarios that were never compressed">
`report.json` summarises all 37 scenarios, 13 of which never enter a compression
path and are recorded as 0 ms, so its p50 reads 0.032 ms. The table above covers
only the 24 that were actually compressed, which is the basis that reflects
compression cost. With 24 observations p99 is simply the maximum, so the granularity
is coarse.
</Callout>

### Gate and signals

**Gate.** Scenario probe success rate must not drop by more than 5%. This run
produced 23 signals:

| Kind | Count | Meaning |
|---|---|---|
| retention | 13 | critical-item retention below threshold, all in json (10) and logs (3) |
| probe_drop | 6 | probe success rate dropped by more than 5% |
| capability_gap | 4 | the content type is outside the current applicable range |

### Design choices that affect how the results should be read

- **Scenario assets are generated once and committed**, not built at run time. The
  upstream generator mints tool-call ids with `uuid4`, which a fixed seed does not
  constrain, so generating them per run would change the payload every time and
  leave nothing comparable across runs.
- **Applicability is decided by the payload measured, not by the message role.** A
  tool message carrying raw prose does not enter a compression path; recording that
  as a 0% rate would report an applicability boundary as a compression failure.
  Those scenarios are reported under "Applicability" and excluded from the
  compression and retention figures.
- **Uncertainty is aggregated across scenarios, not across repetitions.** The
  assets are static and the compressors deterministic, so a per-scenario interval
  collapses to a point, and adding repetitions to narrow it would be
  pseudo-replication.
- **The semantic probe uses contains-answer as its verdict, with token-level F1
  alongside**, and counts only questions the original answers, so a question the
  model cannot handle at all is not charged to the compressor. A fixed model answers
  the questions, but **no LLM judges them**: the gate measures a difference between
  two conditions, and judge variance would inflate or hide that difference with no
  way to tell which.

---

## L4 end-to-end layer

**What it tests.** A complete agent task from the first prompt to the final patch,
with compression active throughout — the two things a user ultimately cares about:
whether the task still completes, and what it cost in total. 48 SWE-bench Lite
bug-fix instances, sharded 12 per host across 4 hosts so every host runs every arm,
repeated five times at temperature 0.

Four arms are compared per instance: **A** no compression (baseline), **B**
tokenless, **C** the reference compressor (default), **D** the reference compressor
(tool-results mode).

**Why this layer is needed.** L1 through L3 all measure a compressor against a fixed
payload or a fixed conversation, so none can observe the feedback loop: a compressed
tool result changes what the agent does next, which changes the next payload. That
compounding effect is only visible over a full task — and, as it turns out, it is
what separates a per-request saving from an end-to-end one.

### Per-request saving does not become an end-to-end saving

On the requests it touched, the reference saved about 10% of wire input tokens
(C 10.03%, D 10.12%; active on 66–69% of requests), consistent with the L1–L3
per-payload numbers. tokenless's compression hook fired on all 48 instances, but the
harness captured no wire counter on that side, so only activation is proven for it.

Yet none of that compounds into a whole-task token reduction. Pairing per instance
(difficulty cancels) and repeating five times, **every arm's prompt-token ratio
straddles parity in every repeat:**

| arm vs baseline | prompt-token ratio, 5 repeats | end-to-end verdict |
|---|---|---|
| **B** tokenless | 1.10 / 1.01 / 0.94 / 1.00 / 0.97 | no reproducible effect |
| **C** reference (default) | 0.92 / 0.99 / 0.98 / 1.09 / 0.91 | no reproducible effect |
| **D** reference (tool-results) | 1.12 / 1.10 / 1.08 / 1.15 / 1.04 | small, consistent **increase** |

Exactly one 95% CI in the whole matrix excluded 1.0 — tokenless's *request count* in
repeat 1 — and it did not recur in the next four. Arm D is the only monotone
pattern: its tool-results mode sits above 1.0 on every repeat, i.e. it added prompt
tokens rather than removing them on this workload. The end-to-end result is a null:
a saving measured per request did not survive the agent's shifting trajectory.

### Solve rate

Every emitted patch was scored by the SWE-bench test harness (`FAIL_TO_PASS` must
flip to passing, `PASS_TO_PASS` must stay passing). Pooled solve rate is **73–76%
and statistically indistinguishable across all four arms** — compression neither
helps nor hurts whether the bug gets fixed.

That rate is high because the corpus is guided (see the callout), so it is a
property of the corpus, not a capability score, and is not comparable to published
SWE-bench numbers. The informative part is the failing tail: a small, stable set of
instances fails in every repeat and every arm, and they are exactly the ones whose
prompt names the file to edit but discloses none of the fix. On those, the agent
edits the right file yet produces a change that does not make the target test pass
— handing over the location is not enough.

<Callout type="warning" title="The corpus is guided, and it cuts both ways">
These prompts are not blind bug reports: the recorded campaign audit found 80%
matched exactly one gold source-file name under its legacy full-path-or-basename
rule, and about one in five reproduced ≥75% of the gold patch's own added lines.
Common basenames can inflate that 80%; the current audit reports strict full-path
hits and basename-only hits separately, so no strict-path percentage is inferred
without re-running the corpus audit. The guidance still **suppresses the effect
being measured** (a compressor has little to compress when the prompt is
already small and partly pre-solved) and **inflates the solve rate** (the model is
often handed the answer). Both the null token result and the 73–76% solve rate must
be read in that light; neither extrapolates to open-ended agent work.
</Callout>

### Reproducibility floor

At temperature 0 the runs are not reproducible: across the five repeats **no
instance reproduced its own prompt-token total**, and the median per-instance
coefficient of variation was 21–27%. The between-arm differences above (5–12%) are
smaller than that noise floor, so the null is best read as "no effect detectable
above run-to-run noise", not "proven zero". A larger instance set alone does not fix
this; each instance must be averaged over repeats before pairing.

---

## Method

1. **One authoritative counter.** Every headline rate is recomputed with tiktoken
   (`o200k_base`) over the real output. A component's self-reported count is never
   used as a headline figure.
2. **Independence keyed on payload.** Repeated runs of a deterministic command
   count once. Verified: `git log`, `git show`, and `git diff` each produce 1
   unique output over 5 repetitions, while `rg` produces 5.
3. **Intervals from bootstrap** (10,000 resamples, fixed seed), computed over
   independent observations only. Where a category is fully deterministic the
   interval collapses to a point, and that is stated rather than hidden.
4. **No retention credit where nothing was compressed**, because a perfect score on
   an unchanged payload is not evidence.
5. **Content, not format.** Ground truth for diffs comes from identifiers on the
   changed (`+`/`-`) lines, so a compressor that keeps the diff frame and discards
   the body does not score as lossless.
6. **Self-consistency check.** The uncompressed original must pass the checks
   written against it; a failure is a harness defect and is never recorded as a
   product defect.
7. **A negative control per metric.** A payload that should fail must be shown to
   fail, otherwise a metric can pass by measuring nothing at all.

## Environment

| Item | Value |
|---|---|
| Platform | Linux x86_64 |
| Tokenizer | tiktoken `o200k_base` (headline), `cl100k_base` (sensitivity check) |
| RTK | pinned release build |
| Semantic probe | single model, fixed temperature (qwen3-max at L3) |

## Limitations

Read these before quoting any figure above.

- **Sample diversity is the main constraint at L2.** json has 3 independent
  samples, diff has 5 entry points, command has only 2. For `command` and `code`
  the interval collapses to a point (`[0.0, 0.0]`) because every observation is
  identical: the point estimate is sound, but the interval carries no uncertainty
  information and the result cannot be extrapolated to other output shapes.
- **The diff headline aggregates several entry points; it is not one rate.** The
  76.0% mean spans `[37.8, 97.6]` precisely because the same content ranges from 0%
  to 99.5% depending on the entry point.
- **The json mean hides a wide spread between samples**: 18.0%, 53.5%, and 62.2%
  across three payloads of the same type. The constraint is sample diversity, not
  repetition count.
- **RTK output filtering was collected by hand**, measured once, outside the
  automated harness. It is offered as a measured observation, not a reproducible
  test.
- **The semantic probe depends on a single model.** Temperature is fixed for
  reproducibility, but absolute scores reflect that model's ability; the
  before-and-after comparison is more reliable than the absolute value.
- **L1 rates are per fixture.** Each figure is that compressor on its own standard
  input, not an expected value for arbitrary production payloads.
- **Non-code diffs are measured less precisely.** The content-extraction fallback
  degrades to ordinary English words on documentation and configuration diffs, so
  it discriminates less well there than on code.
- **L3 scenario assets are synthetic and committed**, not captured from production
  traffic. The results hold for these shapes and size steps; extrapolating to real
  workloads needs care.
- **The L3 probe baseline is small.** Of 48 questions the original answers 31, and a
  single scenario carries only 1–3 questions, so a figure like "fell 50%" comes from
  a tiny denominator: read the direction, not the precision.
- **L3 critical items are very unevenly distributed.** agentic alone accounts for 88%
  of the denominator, so any pooled cross-type retention figure is dominated by it;
  this page therefore reports per type plus an equal-weighted mean. schema and simple
  have one scenario each, so their intervals collapse to a point.
- **Two of the 37 L3 scenarios are near-duplicates.** The pipeline suite's `agentic`
  and the scenario suite's `multi_tool_50t`, and the pipeline suite's `rag` and
  `document_qa_20000`, were generated separately from the same configuration and
  differ only in their tool-call ids, so they are not independent observations. 139
  of agentic's 694 critical items (about 20%) come from that near-duplicate pair.
- **L4 numbers describe a guided corpus, not open-ended agent work.** The prompts
  disclose the file to edit and often part of the fix, which both suppresses any
  compression effect and inflates the 73–76% solve rate. Do not read the token null
  as "compression is free end-to-end" in general, nor the solve rate as a SWE-bench
  capability score.
- **L4 runs are not reproducible at temperature 0.** No instance reproduced its own
  prompt-token total across five repeats (median CV 21–27%), so the between-arm
  token differences sit below the noise floor: read them as a null, not a measured
  zero.

## Reproducing results

```bash
# L1 component layer
cd src/tokenless/benchmark/l1-compressor
cargo run --release --bin l1_bench -- --report-dir reports

# L2 module layer
cd src/tokenless/benchmark/l2-module
cargo run --release --bin l2_compare -- --n auto --report-dir reports

# L3 scenario layer
cd src/tokenless/benchmark/l3-scenario
cargo run --release --bin l3_compare -- --report-dir reports

# L4 end-to-end layer (remote fleet; needs staged prompt corpus and provider key)
cd src/tokenless/benchmark/l4-end-to-end
bash assets/scripts/run_repeats.sh      # runs the four arms, five repeats
bash assets/scripts/evaluate_all.sh     # scores patches with the SWE-bench harness
python3 assets/scripts/analyze_solve_rate.py
```
