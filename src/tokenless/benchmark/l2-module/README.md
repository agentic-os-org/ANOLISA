# L2 Module Comparison Benchmark

Paired comparison of **tokenless** (Rust) vs **headroom** (Python + PyO3)
on paired one-round tool outputs across nine payload categories:
`json`, `command`, `grep`, `code`, `diff`, `buildlog`, `tabular`, `search`, `html`.

The harness is pure Rust (`src/l2/`, binary `l2_compare`); headroom runs as a
resident Python worker (`assets/worker/headroom_worker.py`) speaking a
line-delimited JSON protocol, so both compressors see byte-identical inputs
in the same round.

## Metrics

| Metric | Definition |
|---|---|
| Compression rate | `1 - tokens_after / tokens_before`, counted with **tiktoken-rs** on `o200k_base` (headline) and `cl100k_base` (side report) |
| Retention | Ground-truth items (substring/regex) still present after compression; ordinary literals allow Markdown escaping and wrapped whitespace, while literal backslashes/repeated whitespace and grep path/line identity remain exact; Wilson 95% interval over pooled counts of the **independent** observations |
| Semantic score S | `retained / correct_uncompressed` — the share of questions the ORIGINAL text answered that the compressed text still answers, per question, at `temperature=0`; `None` when no key or zero denominator |
| Latency | p50/p95/p99 in ms — **bases differ per side, see below** |
| Sample size N | Independent observations behind compression/retention. Within a category, byte-identical default payloads count **once**, including different command aliases; feeding every repeated copy into bootstrap/Wilson would narrow the intervals without adding payload. Repetition count is sized from a 5-run pilot as `N = ceil((1.96·CV/0.05)²)` clamped to `[5, 50]` and still drives latency percentiles; bootstrap 95% CIs (10000 resamples, seed 42) |
| Comparability | Static and native-command pairs use identical source bytes. Explicit RTK aggressive/smart conditions carry typed `explicit_rtk=true` on both sides and stay out of default headline pools and paired gaps. Their Runtime input is stored separately as `runtime_input_ref` in private schema-2 evidence; `input_ref` still identifies the raw baseline. Default pairs require equal baseline fingerprints. Comparator rows always consume that raw baseline, never pretend RTK ran on the reference side. |

### Latency bases (not cross-comparable)

| Side | Basis |
|---|---|
| tokenless (static payloads) | **production PostTool**: in-process dispatch, domain detection and final arbitration |
| tokenless (command/grep/diff) | **production entry**: PreTool + PostTool in-process time plus wrapped-minus-raw execution overhead when rewritten |
| headroom | **worker-internal**: `perf_counter` around `router.compress` inside the worker (pipe/JSON framing excluded) |

## Running (three steps, all remote)

Everything builds and runs on a remote Linux host — tokenless is Linux-only
and headroom needs a Linux PyO3 build. Never build or run locally on macOS.

```bash
export L2_SSH_HOST=<host> L2_SSH_PASS=<password>   # L2_SSH_USER defaults to root
# Non-root account: also set L2_REMOTE_WORK to a writable dir, e.g.
#   export L2_SSH_USER=<account> L2_REMOTE_WORK=<workspace>
./assets/scripts/remote_sync.sh                            # 1. rsync anolisa + headroom sources
./assets/scripts/remote_setup.sh                           # 2. idempotent env + builds
DASHSCOPE_API_KEY=<key> ./assets/scripts/remote_run.sh     # 3. resync, rebuild, run + pull reports/
```

`remote_run.sh` re-runs sync + setup so the measured binaries always match the
synced revision; setup is idempotent (it reuses a working headroom venv and
only does incremental cargo rebuilds), so the repeat is cheap and never
destroys the environment.

For credential-free, independent runs from this directory on Linux:

```bash
bash ../../packaging/build/setup-rtk.sh
cargo test --locked --lib --tests
cargo run --locked --bin l2_compare -- --tokenless-only --no-probe --n 5 --mode default --report-dir reports/default
cargo run --locked --bin l2_compare -- --tokenless-only --no-probe --n 5 --mode no-stash --report-dir reports/no-stash
```

`--mode diff-opt-in` selects an explicit diff-enabled condition; current Runtime
already enables diff compression by default, so do not label it an independent
contrast if its resolved settings equal default. Command tests use the current
`tokenless rtk` launcher, not a separate RTK executable. Prepare the pinned RTK
source before building the adjacent Tokenless CLI. The optional reference worker
and all model probes are disabled by `--tokenless-only`, even when an API key is
present. In comparison mode, `--no-probe` separately disables model calls.

Reports land in this workspace's own `reports/` directory. `report.json` uses
`schema_version: 2` and `report_kind: comparison|tokenless-only`; `summary.mode`
is the Runtime mode, not the report kind. Both it and `measurements.json` record
`resolved_entry_options`. Runtime flags are pinned to `RuntimeConfig::default()`;
ignored environment overrides and equivalent diff-opt-in/default settings are
explicit degradations, not independent configurations.

`measurements.json` schema 2 streams rows to disk and stores each input, output
and stash-original text once in `text_store` under its SHA-256. Row `input_ref`,
`output_ref` and stash `original_ref` resolve into that store; readers of schema 1
must be upgraded before using schema 2. Hashes, selected mode, production
outcomes, recovery records and fact-check failures remain per-row. Static payloads go through
PostTool; commands go through PreTool, execution and PostTool. Explicit RTK
aggressive/smart samples remain separate manual-entry conditions, not default
PreTool choices. `additional_context` is counted as model-visible text.
No model solve rate, real billing or compression activation is inferred from an
unrun probe. The result tree stays separate from L1's.

The `reports/` directory is gitignored: reports are run/machine-specific artifacts
and are never committed. Complete tool text and stash originals remain private;
only sanitized reports should be shared.

Cold fixture directories use a leaf-only `fresh_dir` and an argv `{{fresh_dir}}`
placeholder. Every execution receives a new randomly named private directory;
cleanup is owned by its temporary-directory guard, never a spec-supplied path.
Single-line JSON command output receives content facts; a zero fact denominator
is marked `retention-not-measured`, never a successful retention measurement.

Extra arguments to `remote_run.sh` are forwarded to `l2_compare`:
`--categories all|json,command,grep,code,diff`, `--n auto|<int>`,
`--no-probe`, `--model <id>`, `--report-dir <dir>`.

## Environment variables

| Variable | Used by | Meaning |
|---|---|---|
| `L2_SSH_HOST` | scripts | remote host or IP (never hard-coded) |
| `L2_SSH_USER` | scripts | remote user, default `root` |
| `L2_REMOTE_WORK` | scripts | remote workspace root, default `/root/work`. Set this to a writable dir when `L2_SSH_USER` is not root, or the run fails on permissions. |
| `L2_SSH_PASS` | scripts | ssh password (never stored in files) |
| `HEADROOM_SRC` | remote_sync | local headroom checkout, default `~/git_repo/headroom` |
| `DASHSCOPE_API_KEY` | l2_compare | enables the semantic probe; unset ⇒ all S = None |
| `HEADROOM_PYTHON` | l2_compare | python that can `import headroom`, default `python3` |
| `TOKENLESS_BIN` | l2_compare | tokenless run as `tokenless rtk`; default discovery: Tokenless release build, then `PATH` |

## Degradation matrix

The harness never aborts on a missing toolchain; it degrades and records the
degradation in the report:

| Missing | Effect |
|---|---|
| headroom worker (spawn/import failure) | headroom side skipped everywhere; one-sided report |
| `tokenless` | tokenless side of command/grep/diff skipped; raw commands still feed headroom |
| `DASHSCOPE_API_KEY` or `--no-probe` | all semantic scores reported as `None` |
| `/models` discovery failure | silent fallback to `qwen-max` |

## Quality gate

* tokenless trailing headroom by **> 15pp** compression in a category ⇒ L1 candidate signal
* semantic score **S < 0.85** ⇒ flagged
* p99 over budget ⇒ flagged (json < 2 ms, code < 5 ms, command/grep/diff < 10 ms, each on its own basis)

## Security note

> The remote scripts authenticate non-interactive SSH/rsync with
> `SSHPASS="$L2_SSH_PASS" sshpass -e`. `-e` reads the password from the
> environment rather than argv: with `-p` the password appears in the process
> table and any local user can read it out of `ps`. Never hard-code passwords
> or keys anywhere in the repository (see AGENTS.md §10 sensitive-data gate);
> prefer key-based auth when wiring these scripts into any longer-lived
> pipeline.
>
> `-e` narrows the exposure but does not remove it. The password still lives in
> the process environment, so anything running as the same user (or as root) can
> read it from `/proc/<pid>/environ` for as long as the process exists. On a
> shared or multi-tenant runner, treat that as unacceptable and use key-based
> auth instead: `-e` is a fix for the `ps`-visible-to-everyone case, not a way to
> make password auth safe on a host you do not control.
