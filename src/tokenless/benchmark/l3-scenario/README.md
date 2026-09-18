<!-- Copyright 2026 Alibaba Cloud

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License. -->

# L3 Scenario Benchmark

**L3 — the scenario layer** of the four-layer tokenless benchmark plan.

Where L2 compares the two compressors on a *single* tool output, L3 compares
them on a *whole conversation*: the message list an agent actually sends to the
model. The measured quantity is total conversation tokens before → after.

## Reference system

The scenarios mirror the reference implementation's own two benchmark suites and
**keep its structure**, so results can be cross-referenced against the
reference's published numbers instead of being reorganised into a taxonomy only this repo uses. The
two suites are separate top-level directories:

| Suite | Mirrors | Points |
|---|---|---|
| `assets/pipeline/` | `bench_transforms.py::TestPipelinePerformance` | 3 |
| `assets/scenario/` | `bench_latency.py::generate_scenarios()`, grouped by the reference's own `content_type` | 34 |

### `assets/pipeline/` — whole-conversation shapes, with the reference's latency targets

| Scenario | Shape | reference target |
|---|---|---|
| `simple` | 3 messages: system prompt with a date, one question, one answer | < 5 ms |
| `agentic` | 50 turns × ~2 tool calls, 50 items per tool response (233 messages, 100 of them tool) | < 30 ms |
| `rag` | ~20K tokens of injected context documents + 5 Q&A turns | < 50 ms |

### `assets/scenario/` — one content type per point, across size steps

| `content_type` | Points | Size steps |
|---|---|---|
| `json` | 14 | search results 100/500/1K/5K, api 500, db rows 1K, string array 100/500/1K, number array 200/1K, mixed 250, flat object 100 keys, nested 600 |
| `logs` | 4 | 100/500/1K/5K entries |
| `agentic` | 4 | 10/25/50/100 turns |
| `code` | 4 | ~50/~200/~500/~1K lines |
| `text` | 4 | 1K/5K/20K/50K tokens |
| `rag` | 3 | 5K/20K/50K context |
| `schema` | 1 | 20 tool definitions — **the one asset not taken from the reference** |

Every asset records its origin in a `source` block, including
`headroom_native: true|false`. Only `assets/scenario/schema/tools_20.json` is
`false`: the four-layer plan's L3 table defines the single-turn case as "system
prompt + 20 tool schemas + one exchange" targeting SchemaCompressor, but
the reference's fixtures carry no tools array (it compacts tool schemas in its
proxy layer, not in the message transforms its benchmarks exercise). Without it,
neither side's schema compaction would appear anywhere in L3. The reference's own
fixtures are left untouched.

## What each side compresses

Both sides receive the **byte-identical message list**. What they do with it
differs, and that difference is the finding — not a measurement artifact.

### The comparison side runs in two pipeline variants, and both are reported

The reference's own benchmark fixture assembles only `CacheAligner -> SmartCrusher`,
noting that ContentRouter is "omitted here to keep the fixture pure-stage". But
ContentRouter is what the reference's own docstring calls "the recommended
entry point" for its compression, and the content-specific compressors
(CodeAware, Log, Diff, Search, Html, TextCrusher) only run underneath it.
Reporting either variant alone misrepresents the reference, so both run:

| Variant | Stages | Answers |
|---|---|---|
| `pure_stage` | CacheAligner + SmartCrusher | how the reference's published benchmark is configured, comparable to its 5/30/50 ms targets |
| `router` | the above + ContentRouter | what the reference can actually do |

The difference is not cosmetic. Measured on `scenario/text/documentation_5000`,
`pure_stage` compresses **0.0%** and `router` compresses **70.6%**. Publishing
only `pure_stage` would have supported the false conclusion that the reference cannot
compress prose either.

### tokenless

`SchemaCompressor` over the tools array (its `BeforeModel` hook) and
`ResponseCompressor` over each JSON tool response (its `PostToolUse` hook), both
with default configuration — the same settings L1 and L2 measure. Its other two
compressors are out of scope for these assets by construction: TOON is a
pipeline step chained after `ResponseCompressor`, and rtk rewrites shell
commands, which these conversations do not carry.

Consequence worth stating plainly, and measured rather than assumed: tokenless'
four compressors all take **structured** input — tool schemas, JSON values,
shell commands. Verified against the shipped binary, a tool message carrying
raw Python or raw prose is rejected with `JSON parse error`, and the production
hook takes the same path (it calls `skip()` when the payload does not parse as
JSON). So `code`, `text` and `rag` points have no applicable tokenless entry
point at all.

That is a **capability gap, not a compression failure**, and the two must not be
reported as the same thing. The report states gaps prominently right after the
summary rather than burying them: what each side cannot do is the primary output
of this layer, since it feeds the L1 decision about which compressor to add
next. Applicability is decided per point from the payload actually measured (can
the tool message be parsed as JSON, is a tools array present) — never inferred
from the message role alone.

## Why the scenarios are committed as static assets

`assets/scripts/gen_scenarios.py` writes `pipeline/` and `scenario/` **once**,
and they are committed rather than built at run time. This is a correctness
requirement, not convenience: the reference's `generate_agentic_conversation` mints
tool-call ids with `uuid.uuid4()`, which `random.seed(42)` does not constrain, so
regenerating would produce a different payload on every run — different token
counts, nothing comparable across runs or across the two sides.

Each asset records the reference revision it was generated from, so a scenario
can be traced back to the generator that produced it.

Regenerate deliberately (and review the diff) with:

```bash
python3 assets/scripts/gen_scenarios.py --headroom <path-to-reference-checkout>
```

It writes into `assets/pipeline/` and `assets/scenario/`.

## Metrics

| Metric | Definition |
|---|---|
| Compression rate | `1 - tokens_after / tokens_before` over the whole conversation, counted with **tiktoken-rs** (`o200k_base` headline, `cl100k_base` side report) — same scale as L2 |
| Probe success rate | Share of scenario probe questions answered correctly. The L3 gate is the **drop** between original and compressed conversation |
| Retention | Scenario-critical items (dates, tool-call ids, ground-truth facts) still present after compression |
| Latency | p50/p95/p99 per side, each under its own basis (see below) |
| N | Scenarios contributing to the group. The assets are static and both compressors deterministic, so each scenario counts exactly once and no repetition loop exists to inflate it |

The quality gate for this layer is **probe success-rate drop < 5%**, per the
four-layer plan.

### Two token counts, and which one is authoritative

There are two counts in play, they disagree, and the report **must** say so
rather than presenting one silently:

| Count | Produced by | Used for |
|---|---|---|
| **Authoritative** | tiktoken-rs in this harness, over both sides' output | every published compression rate |
| reference self-report | the reference's own estimator, inside the worker | corroboration only |

The worker injects the **chars/4 estimator the reference's own benchmark
fixtures use**, so its crushing decisions and latency stay on the terms it
publishes for these scenarios (its 5/30/50 ms pipeline targets). A side effect
is that the reference's self-reported `tokens_before` differs from the authoritative
count for the identical payload — measured, for `scenario/json/search_results_1000`:

| | `tokens_before` |
|---|---|
| reference self-report (chars/4) | 80362 |
| reference self-report (its real tokenizer registry) | 100461 |
| authoritative (tiktoken `o200k_base`) | 93420 |

Three consequences the report has to state explicitly:

1. **Compression rates are unaffected**, because they are computed from the
   authoritative count on both sides. A rate never mixes the two bases.
2. **The estimator does change the reference's behaviour**, not just its bookkeeping:
   crush decisions compare token counts against `model_limit`, so a payload the
   estimator puts on the other side of the limit is crushed differently. Any
   reference rate therefore reflects "the reference as its own benchmark
   configures it", which is the intent, and not "the reference in production".
3. **Latency shifts with the estimator too** — counting is part of the measured
   work. The real registry cost 649 ms on `pipeline/agentic` versus 73 ms for
   `search_results_1000` under chars/4, so latency is only meaningful against
   the reference's targets when the estimator matches the one those targets were set
   with.

### Latency bases (not cross-comparable)

| Side | Basis |
|---|---|
| tokenless | in-process: `Instant` around the per-tool-message compress calls, summed per conversation |
| comparison | worker-internal: `perf_counter` around `pipeline.apply`, excluding pipe/JSON framing |

## Layout

```
l3-scenario/
├── assets/                    # all data; code stays in src/, as in L1 and L2
│   ├── pipeline/              #   suite 1: simple, agentic, rag
│   ├── scenario/              #   suite 2, by reference content_type:
│   │   ├── json/ logs/ agentic/   #     tokenless has an entry point
│   │   ├── code/ text/ rag/       #     tokenless has none (measured, above)
│   │   └── schema/               #     the one asset not from the reference
│   ├── probes/                #   per-scenario probe questions
│   ├── worker/                #   comparison pipeline worker (line-delimited JSON)
│   └── scripts/               #   one-time scenario generator
├── src/lib.rs
├── src/l3.rs                  # module root (no mod.rs, per project convention)
├── src/l3/                    # asset, tokenless_side, headroom_side,
│                              #   tokenizer, probe, retention, stats, report
├── src/bin/l3_compare.rs      # orchestrator
├── tests/                     # l3_*-prefixed integration tests
└── reports/                   # gitignored run artifacts, as in L1 and L2
```

## Running

Linux only — tokenless is Linux-only, and the comparison side needs a Linux
PyO3 build.

```bash
HEADROOM_PYTHON=/path/to/headroom-venv/bin/python \
DASHSCOPE_API_KEY=<key> \
  cargo run --release --bin l3_compare -- --report-dir reports
```

`--no-probe` skips the semantic probe; a missing comparison worker degrades to a
one-sided run and the report records the degradation rather than aborting.

`L3_NO_PROBE` and `L3_REPORT_DIR` are accepted as equivalents to `--no-probe` and
`--report-dir`, so scripts can set them in the environment. An explicit flag wins
over the variable. An unrecognised flag is an error rather than being ignored: a
silently dropped `--no-probe` would send probe requests the caller asked to skip.

Reports are run/machine-specific artifacts and stay out of git.
