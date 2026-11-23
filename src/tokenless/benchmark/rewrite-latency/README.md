# PreTool rewrite latency

[中文版](README_zh.md)

Measure the QwenWork macOS Hook, Protocol v2 `pre_tool`, and the RTK command
line separately, using staged plugins or an installed plugin. This benchmark
does not execute the rewritten commands or measure PostTool output compression.

Run from `src/tokenless` with Python 3.9 or later:

```bash
python3 benchmark/rewrite-latency/bench.py \
  --variant installed="$HOME/.qwenworkcn/plugins-custom/tokenless" \
  --cwd "$PWD" --samples 30 --warmup 5 \
  --output benchmark/rewrite-latency/reports/installed.json
```

For a source comparison, build each revision with the same Rust toolchain,
release profile and target. Stage each binary alongside its matching adapter
resources; the shared hooks must be copied, not left linked to the working tree:

```bash
mkdir -p target/rewrite-bench/before
cp -RL adapters/tokenless/qwenwork/. target/rewrite-bench/before/
mkdir -p target/rewrite-bench/before/bin
cp target/release/tokenless target/rewrite-bench/before/bin/tokenless
```

After building the change, repeat the staging commands with `after` instead of
`before`, then compare both in one run:

```bash
python3 benchmark/rewrite-latency/bench.py \
  --variant before=target/rewrite-bench/before \
  --variant after=target/rewrite-bench/after \
  --cwd "$PWD" --samples 50 --warmup 5 \
  --output benchmark/rewrite-latency/reports/comparison.json
```

To include the report's v0.9.1 baseline, build tag `tokenless/v0.9.1` with its
pinned RTK source and patches. Stage that revision's adapter plus both
`bin/tokenless` and `bin/rtk`, then add `--variant v0_9_1=PLUGIN_ROOT` to the same
comparison. The script recognizes v0.9.1's standalone RTK and its older grep
policy. It records the expected action per variant and compares commands only
when those actions match. Rebuilt sources do not reproduce the signing or
cross-compilation of the original distributed artifact.

The script derives both Hook and Core inputs from the same command and supplies
the same working directory to every process. Git and compound-command cases
must replace arguments; `find`, `grep`, unsupported commands and empty input
must pass through on current versions. v0.9.1 must rewrite the grep case; do not
attribute its transition to passthrough to rewrite performance. The find and
grep inputs match the original report's direct-Core and full-Hook inputs,
respectively. Unexpected exits, stderr, malformed responses, lost arguments
or changed rewrites invalidate the run. Rewrites are compared after normalizing
only runtime paths, receipt IDs, the entrypoint's correlation ID and the
standalone `rtk` versus `tokenless rtk` launcher.

Each variant explicitly selects its own temporary stats and stash database,
overriding inherited database paths and removing DSH-managed path aliases.
The temporary root is canonicalized so symlinked paths match Core's emitted
state paths during cross-variant command comparison.
Statistics are enabled and SLS is disabled. One call ID per case and entrypoint
keeps state cardinality constant.
Existing RTK and project configuration is read; a configuration that changes
the expected actions will make the benchmark fail rather than measure another
path. The Bash launcher selects `python3` from `PATH`; keep it unchanged across
variants. The full QwenWork launcher requires macOS.

Warmup invocations are excluded. A seeded shuffle interleaves variants, cases
and entrypoints to reduce order effects. Reports contain every measured sample,
median, mean, nearest-rank p90/p95, versions and binary/adapter fingerprints.
These are fresh-process measurements with warmed filesystem caches, not
first-install cold-start measurements. Run on an otherwise idle machine.

The benchmark parent stays alive across samples, but each full Hook starts a
new Python parent for Core. Direct CLI samples therefore do not include costs
specific to the first launch from a new Hook parent. When profiling macOS,
check this separately by comparing first and repeated `--version` launches
inside fresh parent processes. Time stages within the same Hook invocation;
do not add direct CLI timings to Python timings to reconstruct the total.

Compare a case and entrypoint directly between variants. Do not subtract
different cases to attribute time to Python or I/O, and do not interpret a
passthrough as faster rewriting. Reports are generated, ignored artifacts.
