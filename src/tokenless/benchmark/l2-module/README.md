# L2 Module Comparison Benchmark

Paired comparison of **tokenless** (Rust) vs **headroom** (Python + PyO3)
on identical one-round tool outputs, across five payload categories:
`json`, `command`, `grep`, `code`, `diff`.

The harness is pure Rust (`src/l2/`, binary `l2_compare`); headroom runs as a
resident Python worker (`assets/worker/headroom_worker.py`) speaking a
line-delimited JSON protocol, so both compressors see byte-identical inputs
in the same round.

## Metrics

| Metric | Definition |
|---|---|
| Compression rate | `1 - tokens_after / tokens_before`, counted with **tiktoken-rs** on `o200k_base` (headline) and `cl100k_base` (side report) |
| Retention | Ground-truth items (substring/regex) still present after compression; Wilson 95% interval over pooled counts of the **independent** observations |
| Semantic score S | `retained / correct_uncompressed` — the share of questions the ORIGINAL text answered that the compressed text still answers, per question, at `temperature=0`; `None` when no key or zero denominator |
| Latency | p50/p95/p99 in ms — **bases differ per side, see below** |
| Sample size N | Independent observations behind compression/retention. Static categories compress deterministically, so repetitions of one sample count **once**; feeding every copy into bootstrap/Wilson would narrow the intervals without adding payload. Repetition count is sized from a 5-run pilot as `N = ceil((1.96·CV/0.05)²)` clamped to `[5, 50]` and still drives latency percentiles; bootstrap 95% CIs (10000 resamples, seed 42) |
| Comparability | `code` is reported but **not** cross-side comparable: tokenless' engine only accepts JSON values, so that payload reaches it inside a `{"content": ...}` envelope while headroom sees raw text. The paired gap is withheld and the report says why; each side's own rate still stands |

### Latency bases (not cross-comparable)

| Side | Basis |
|---|---|
| tokenless (json/code) | **in-process**: `Instant` around the `JsonCompressor` call |
| tokenless (command/grep/diff) | **wrapped-minus-raw wall clock**: rtk-wrapped run minus raw run (includes rtk process startup) |
| headroom | **worker-internal**: `perf_counter` around `router.compress` inside the worker (pipe/JSON framing excluded) |

## Running (three steps, all remote)

Everything builds and runs on a remote Linux host — tokenless is Linux-only
and headroom needs a Linux PyO3 build. Never build or run locally on macOS.

```bash
export L2_SSH_HOST=<host> L2_SSH_PASS=<password>   # L2_SSH_USER defaults to root
# Non-root account: also set L2_REMOTE_WORK to a writable dir, e.g.
#   export L2_SSH_USER=ubuntu L2_REMOTE_WORK=/home/ubuntu/work
./assets/scripts/remote_sync.sh                            # 1. rsync anolisa + headroom sources
./assets/scripts/remote_setup.sh                           # 2. idempotent env + builds
DASHSCOPE_API_KEY=<key> ./assets/scripts/remote_run.sh     # 3. resync, rebuild, run + pull reports/
```

`remote_run.sh` re-runs sync + setup so the measured binaries always match the
synced revision; setup is idempotent (it reuses a working headroom venv and
only does incremental cargo rebuilds), so the repeat is cheap and never
destroys the environment.

Reports land in this workspace's own `reports/` directory (`report.json` +
`L2_MODULE_COMPARISON_REPORT.md`), keeping L2 results separate from the L1
layer's. That directory is gitignored: reports are run/machine-specific
artifacts and are never committed — regenerate them or attach them to the PR as
CI artifacts.
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
| `RTK_BIN` | l2_compare | rtk binary; default discovery: vendored release build, then `PATH` |

## Degradation matrix

The harness never aborts on a missing toolchain; it degrades and records the
degradation in the report:

| Missing | Effect |
|---|---|
| headroom worker (spawn/import failure) | headroom side skipped everywhere; one-sided report |
| rtk binary | tokenless side of command/grep/diff skipped; raw commands still feed headroom |
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
