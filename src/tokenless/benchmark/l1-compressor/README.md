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

# Tokenless Benchmark Suite

Reproducible component measurements for Tokenless compression. The suite covers
structured content, tool schemas, search/path lists, tables, build logs and HTML,
plus encoding and command-rewrite compatibility. It exports complete candidate
outputs and recovery records so offline facts and optional model questions can
be checked against the same input.

This suite is **L1 — the component layer** of the four-layer tokenless
benchmark plan: every bench/test target measures a single compressor in
isolation and carries the `l1_` prefix. Higher layers (L2 module-level
comparison against other products, etc.) land separately under their own
prefixes.

This is a **standalone Cargo workspace** (see `Cargo.toml`) so it does not
inherit the main tokenless `panic = "abort"` profile (which breaks criterion)
and does not pull benchmark-only deps into the main `cargo test --workspace`.
It depends on the tokenless crates by path.

> tokenless is Linux-only. Build and run this on Linux.

## Layout

```
l1-compressor/
├── fixtures/                     # canonical inputs (generated, committed)
│   ├── records.json              #   1000 uniform records
│   ├── record_reduction.json     #   middle critical-record fixture
│   ├── tool_response.json        #   canonical response payload
│   └── schema_search.json        #   canonical function schema
├── src/lib.rs                    # payload generators; loads fixtures for canonical inputs
├── src/metrics.rs                # in-process compression-rate measurement + cost analysis
├── src/bin/compression_rate.rs   # prints/emits the rate report (--json)
├── benches/                      # L1 performance (criterion, 5 files)
│   ├── l1_schema_latency.rs      #  10 measurements (incl. canonical)
│   ├── l1_response_latency.rs    #  15 measurements (incl. canonical)
│   ├── l1_toon_latency.rs        #  15 measurements (encode/decode/roundtrip + canonical)
│   ├── l1_pipeline_latency.rs    #   7 measurements (stacked in-process)
│   └── l1_rtk_latency.rs         #   7 measurements (subprocess, ms-scale)
├── tests/                        # L1 quality (5) + adversarial (3) + rate guards
│   ├── l1_schema_retention.rs    #   8 tests
│   ├── l1_response_retention.rs  #  11 tests
│   ├── l1_toon_roundtrip.rs      #   8 tests
│   ├── l1_rtk_format_compat.rs   #   9 tests (skip if tokenless absent)
│   ├── l1_pipeline_retention.rs  #   6 tests (end-to-end field retention)
│   ├── l1_adversarial_schema.rs  #  14 tests
│   ├── l1_adversarial_response.rs#  16 tests
│   ├── l1_adversarial_toon.rs    #  10 tests
│   ├── l1_worst_case.rs          #   9 tests
│   └── l1_compression_rate.rs    #   5 rate-regression guards
├── python/                       # fixture generation
│   └── gen_fixtures.py           #   single source of truth for input data
├── reports/                      # L1 result reports (gitignored, never committed)
└── run-benchmarks.sh             # orchestrator script
```

Reports live in this workspace's own `reports/` directory
(`L1_COMPRESSOR_BENCHMARK_REPORT.md`, plus
`L1_COMPRESSOR_BENCHMARK_REPORT_DIFF.md` when comparing against a previous
run), keeping L1 results separate from the L2 layer's and following the same
`L<n>_<LAYER>_..._REPORT.md` naming. That directory is gitignored: reports are
run/machine-specific artifacts and are never committed — regenerate them or
attach them to the PR as CI artifacts.

## Prerequisites

- Rust stable >= 1.91 (edition 2024), matching the benchmark CI toolchain.
- `tokenless` for `l1_rtk_format_compat.rs` and the RTK report (optional —
  tests skip if missing). They run `tokenless rtk` from the Tokenless release
  build (`just setup-rtk && cargo build --release` in the tokenless tree),
  from `TOKENLESS_BIN`, or from `tokenless` on `PATH`.
- Python 3.11+ for fixture regeneration only — `gen_fixtures.py` uses the
  standard library exclusively, no third-party packages
  (fixtures are committed; Python is NOT required for benchmarks or tests).

## Running

```bash
# Everything (build + tests + benches + compression rate report):
./run-benchmarks.sh

# Fast path (no criterion benches):
./run-benchmarks.sh --quick

# Individually:
cargo test --release --locked                  # quality + adversarial + rate guards
cargo bench --locked                          # component performance
cargo run --release --locked --bin compression_rate  # component-domain measurements
cargo run --release --locked --bin compression_rate -- --json  # machine-readable JSON output
```

Additional outputs for independent reporting:

```bash
cargo run --release --locked --bin component_cases -- --output reports/components.json
# Optional same-input comparison to a separately collected L2 measurement file:
cargo run --release --locked --bin component_cases -- --shared <measurements.json> --output reports/shared.json
```

Check `component_cases --help` for the input flags. No L2 build or Python model
service is needed for synthetic L1 measurements. The Headroom single-component
worker under `assets/worker/` is optional and requires an independently installed
reference checkout; its stdlib-only protocol tests run through
`make -C ../../ test-benchmark-l1-worker` and are included in CI's
`test-integration`. Reports stay ignored. `component_cases` requires `--output`
because its files contain complete tool text and stash originals; keep those
files private and never paste them into public issues, shared logs or chats.
Shared evidence accepts measurements schema 1 and schema 2's SHA-256 `text_store`
references. IDs and token counts are validated strictly; malformed identities
abort loading instead of inventing a row. Rust regex syntax is required (no
lookaround or backreferences); row-level fact failures remain invalid-evidence
and do not discard other valid measurements. No provider cost is inferred from
compressed input lengths.

## Methodology

- **Token counting**: new domain/candidate rows use `o200k_base`. Historical
  `(bytes + 3) / 4` estimates remain under `legacy_estimates`, not headline or
  billing data. File identity and recovery contents are retained for checks.
- **Criterion**: runs 100 samples per benchmark (RTK benchmarks use 20 due to
  subprocess overhead). For report-grade numbers, recommended practice is 3 runs
  taking median average (not mandatory).
- Legacy latency benches construct the compressor outside the timed closure.
  The content-domain target measures the complete isolated wire invocation,
  including per-call construction and temporary stash setup; tokenizer and
  corpus generation are excluded. Compare each latency against its stated
  basis rather than treating different setup scopes as interchangeable.
- **Single input source.** `python/gen_fixtures.py` generates canonical
  `fixtures/*.json`; content-domain synthetic bytes come from the shared Rust
  generators in `src/lib.rs`, used by both domains and component cases.
  The Rust canonical benches embed the committed fixtures via
  `include_str!` and use the same files, so latency and compression rate are
  measured on **byte-identical** payloads. The `canonical` bench point in
  `l1_response_latency`/`l1_schema_latency` compresses exactly the payload whose
  compression rate the `compression_rate` binary reports — the two numbers are
  attributable to one input. Regenerate with `python3 python/gen_fixtures.py`
  (fixtures are committed; no need to run it before building).
- **Candidate versus final output**: this layer calls each compressor directly.
  A smaller candidate is not proof that the production entry accepts it; recovery,
  source and savings gates are measured by L2/L3. Fact checks distinguish format
  changes from missing content, and controls verify that removed information fails.

## Known Limitations

1. **Corpus scope**: synthetic inputs span content domains and sizes. Optional
   shared inputs come from a caller-supplied measurement file. Neither collection
   certifies all real workloads; hash variants of one command are not independent tasks.
2. **TOON roundtrip limitation**: Root-level scalar keys after large mixed-type arrays are not
   recovered by the TOON decoder. Documented in `tests/l1_pipeline_retention.rs`.
3. **RTK output filtering**: Runtime output compression data from one-time SSH collection.
   Not automatically reproducible. Serves as reference only.
4. **Cost**: no provider bill or end-to-end cost is measured; the old pricing
   projections are no longer emitted as the primary report.
5. **Recovery modes**: default/no-recovery/recovery configurations are explicitly
   named per row. Recovery records only prove that stored originals can be read,
   not that an agent did retrieve them or that retrieval had no latency/cost.
6. **RTK version metadata**: `benchmark_identity.json` records `rtk_version` as a
   best-effort traceability field. A version is recorded only when the
   tokenless reference (`$TOKENLESS_BIN` — a path or a bare command name
   resolved through `PATH` — or the Tokenless release build) points at an
   executable *and* `tokenless rtk --version` exits 0 within
   `RTK_VERSION_TIMEOUT_SECS` (default 5).
   Every other outcome records `"unavailable"`: unresolvable or non-executable
   binary, non-zero exit, empty stdout, or an expired deadline — including the
   case where the binary printed something before the deadline, since an
   abandoned probe cannot vouch for the version it started printing. The probe
   runs under `timeout(1)` when the host has one, escalating to `SIGKILL` 1s
   after the deadline so a binary that ignores `SIGTERM` cannot stall the
   runner; a host with no `timeout`/`gtimeout` helper falls back to an
   unbounded probe. None of this affects the benchmark results.

## Behavior note

`JsonCompressor` now selects a complete-data JSON/TOON candidate before trying
bounded transformations. The canonical records contain enough removable
diagnostic data to cross the 15% lossless threshold, so the `items/*` curves
retain all records. `fixtures/record_reduction.json` separately covers the
recoverable 32-record base budget and its middle critical record.
