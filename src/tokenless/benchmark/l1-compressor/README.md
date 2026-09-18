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

Reproducible benchmark, quality, and cost-analysis suite for the tokenless
compression engine (schema compression, response compression, TOON encoding,
RTK rewrite compatibility). Rebuilt to match the V5 benchmark reports.

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
│   ├── l1_rtk_format_compat.rs   #   9 tests (skip if rtk absent)
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

- Rust stable (edition 2024, >= 1.85)
- `rtk` binary for `l1_rtk_format_compat.rs` (optional — tests skip if missing).
  Build it from the tokenless tree: `just setup-rtk && cargo build --release
  --manifest-path third_party/rtk/Cargo.toml`, then point tests at it with
  `RTK_BIN=/path/to/rtk` or place it at `../../third_party/rtk/target/release/rtk`.
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
cargo test --release                          # quality + adversarial + rate guards (96 tests)
cargo bench                                   # performance (criterion, 54 benchmark points)
cargo run --release --bin compression_rate    # in-process compression rates + cost analysis
cargo run --release --bin compression_rate -- --json  # machine-readable JSON output
```

Note: `run-benchmarks.sh` calls `cargo run --release --bin compression_rate` for
the rate/cost report (no Python dependency). All post-processing (cost model,
stacking report) is computed in Rust in-process.

## Methodology

- **Token counting**: `(bytes + 3) / 4` (div_ceil) — identical to the engine's
  own `estimate_tokens_from_bytes` heuristic. Not a real tokenizer; exists only
  for consistent in-suite comparison.
- **Criterion**: runs 100 samples per benchmark (RTK benchmarks use 20 due to
  subprocess overhead). For report-grade numbers, recommended practice is 3 runs
  taking median average (not mandatory).
- Latency benches construct the compressor **once** (outside the timed closure)
  and reuse it; only the `compress()` / encode call is measured. Compressor
  setup and teardown are **excluded** so the figures line up with headroom,
  which likewise does not count startup/teardown time.
- **Single input source.** `python/gen_fixtures.py` is the one generator for all
  benchmark data; it writes `fixtures/*.json`. The Rust benches embed these via
  `include_str!` and use the same files, so latency and compression rate are
  measured on **byte-identical** payloads. The `canonical` bench point in
  `l1_response_latency`/`l1_schema_latency` compresses exactly the payload whose
  compression rate the `compression_rate` binary reports — the two numbers are
  attributable to one input. Regenerate with `python3 python/gen_fixtures.py`
  (fixtures are committed; no need to run it before building).
- **Compression rate is measured in Rust, in-process.** `src/metrics.rs` counts
  tokens (bytes/4, same heuristic as the engine) immediately before and after
  each compressor call — no CLI subprocess in the measurement loop — and
  `tests/l1_compression_rate.rs` pins the rates as regression guards, so any
  compression change is traceable to a commit via `cargo test`. The
  `compression_rate` binary emits the full report including cost analysis
  (`--json` for machine-readable output).

## Known Limitations

1. **Single canonical fixture**: Compression rates measured on one synthetic fixture per compressor.
   Not representative of all real-world tool responses. Multi-fixture corpus planned for a future revision.
2. **TOON roundtrip limitation**: Root-level scalar keys after large mixed-type arrays are not
   recovered by the TOON decoder. Documented in `tests/l1_pipeline_retention.rs`.
3. **RTK output filtering**: Runtime output compression data from one-time SSH collection.
   Not automatically reproducible. Serves as reference only.
4. **Cost projections**: Arithmetic extrapolation from single fixture × linear scaling.
   Not suitable for production cost planning without real workload validation.
5. **Default config only**: All measurements use default compressor settings.
   Production adapters may use different configurations with different results.
6. **RTK version metadata**: `benchmark_identity.json` records `rtk_version` as a
   best-effort traceability field. A version is recorded only when the rtk
   reference (`$RTK_BIN` — a path or a bare command name resolved through
   `PATH` — or the vendored release build) points at an executable *and*
   `<rtk> --version` exits 0 within `RTK_VERSION_TIMEOUT_SECS` (default 5).
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
