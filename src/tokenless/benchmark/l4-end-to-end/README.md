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

# L4 End-to-End Benchmark

**L4 — the end-to-end layer** of the four-layer tokenless benchmark plan.

> **Results:** the L4 end-to-end numbers this harness produced live in the L4
> section of [`docs/BENCHMARKS.md`](../../../../docs/BENCHMARKS.md). The run trees
> they were computed from are machine-specific and gitignored (see `.gitignore`),
> so this directory ships the harness and the environment definition, not the raw
> runs; regenerate them with the scripts below. Do not infer whole-task token cost
> from the L1–L3 numbers.

L1 through L3 all measure a compressor against a *fixed* payload or a *fixed*
conversation. None of them can observe the feedback loop: a compressed tool
result changes what the agent does next, which changes the next payload. That
compounding effect is only visible over a complete task, which is what this
layer runs.

## How this layer differs from L1–L3

| | L1–L3 | L4 |
|---|---|---|
| Unit under test | a compressor, called directly | a whole agent solving a real bug |
| Harness language | Rust (standalone Cargo workspace per layer) | Python — drives [`benchmark/swe-runner`](../../../../benchmark/swe-runner) |
| Inputs | committed static assets | SWE-bench Lite instances + docker images |
| Determinism | deterministic; repeats collapse to one observation | model sampling and agent trajectories vary |
| Cost per run | free | billed model tokens, wall-clock hours |

Consequently this directory is **not** a Cargo workspace. It keeps the naming and
the `reports/`-isolation convention of the other layers and nothing else.

`swe-runner` deliberately stays where it is rather than moving in here: it is a
general-purpose SWE-bench runner, not a tokenless artefact. The relationship is
the same as L1's to the tokenless crates — this layer configures and drives it.

## Layout

```
l4-end-to-end/
├── assets/
│   ├── configs/          # per-arm agent base configs (--base-config)
│   ├── instances/        # SWE-bench instance shards (n48-all.txt + n48-shard[1-4].txt)
│   └── scripts/          # provisioning, sweep, fleet and analysis (see below)
└── reports/              # gitignored; regenerate or attach to the PR
```

`assets/scripts/` holds, in run order: `remote_sync.sh`, `remote_setup.sh`,
`remote_verify.sh`, `remote_run.sh` (the on-host sweep), `run_pilot.sh` (the
sweep under a spending cap), `remote_fleet.sh` (the multi-host driver),
`run_repeats.sh`, `clean_between_repeats.sh`, `evaluate_all.sh`, and the analysis
scripts `analyze_runs.py`, `analyze_solve_rate.py`, `analyze_reproducibility.py`,
and `audit_prompt_disclosure.py`. `l4_env.sh` is the private environment every
on-host script sources, and `extract_tool_results.py` collects each arm's
pre/post-compression tool results.

## Running

The runs are Linux- and docker-bound and take hours, so everything happens on a
remote host. Credentials come from the environment and are never committed.

```bash
export L4_SSH_HOST=<host>              # remote benchmark box
# Existing default SSH identity is used; optionally set L4_SSH_KEY explicitly.
export HEADROOM_SRC=~/git_repo/headroom  # local comparison-side checkout, or:
# export HEADROOM_REMOTE_CLONE=<checkout on the host> HEADROOM_REV=<commit>

./assets/scripts/remote_sync.sh    # 1. ship sources (+ unit suite once a venv exists)
./assets/scripts/remote_setup.sh   # 2. provision (30-60 min on a cold host)
./assets/scripts/remote_sync.sh    # 3. re-run: now the unit suite actually executes
./assets/scripts/remote_verify.sh  # 4. pre-flight; must exit 0 before any billed run
```

After preflight, continue on the benchmark host, with the provider key already
in its process environment. The following commands are run from the same
workspace; do not put the key value in shell history or script files:

```bash
export L4_REMOTE_WORK=/root/l4
SCRIPTS=$L4_REMOTE_WORK/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts
L4_ARMS=a,b,c L4_DRY_RUN=1 bash "$SCRIPTS/remote_run.sh"
# Billed step: run only after pilot budget approval.
L4_ARMS=a,b,c L4_COST_CAP=300 bash "$SCRIPTS/run_pilot.sh"
# After the pilot exits, score its patches; this step makes no model calls.
L4_EVAL_REPEATS=1 L4_EVAL_ARMS="a b c" bash "$SCRIPTS/evaluate_all.sh"
```

Keep the pilot terminal connected, or launch the pilot itself with `setsid nohup`
and retain its PID for cancellation. TERM/INT and normal exit reap the detached
sweep process group; exit 2 means unreliable metering and exit 3 means the cap
stopped it. These are diagnostic outcomes, not successful full runs. Do not
start an uncapped full campaign merely because a pilot started or completed.

Back in this directory on the operator's machine, collect before analysing:

```bash
L4_HOSTS="$L4_SSH_HOST" ./assets/scripts/remote_fleet.sh collect
python3 assets/scripts/analyze_solve_rate.py --evals runs --runs runs
python3 assets/scripts/analyze_runs.py --runs runs --repeat 1 --configs-dir assets/configs
```

For a fleet, set `L4_HOSTS` in shard order and evaluate on each host before
collection. Inspect every exit code, the solve report's `evaluation_complete`,
and the economics report's validity/provenance gates before citing results.

`remote_setup.sh` is safe to re-run: output checks resume completed steps, but
Tokenless is rebuilt incrementally and a changed Headroom revision or constraints
fingerprint recreates its venv. The resolved dependency freeze is recorded.
`remote_verify.sh` is the gate that must pass before a run is worth paying for —
each of its checks guards a failure mode that would otherwise produce plausible
but meaningless numbers rather than an error.

**The host may be shared.** Everything is installed under `$L4_REMOTE_WORK`
(default `/root/l4`): Node and openclaw, the Rust toolchains, the venvs, and a
private `HOME` holding the benchmark's `~/.openclaw`, `~/.local` and
`~/.tokenless`. System packages are verified, never installed, and no file under
the host user's home, `/usr/local` or `/opt` is written. `l4_env.sh` defines that
environment once. Two parts of it are not obvious: the inherited `PATH` is
replaced, because swe-runner and the tokenless plugin both resolve binaries from
`PATH` first and a host user's `~/.local/bin` may hold another tokenless; and
tokenless resolves its default state, tool-ready spec and env-fix script from the
passwd home rather than `$HOME`, so each is redirected explicitly
(`TOKENLESS_DATA_DIR`, `TOKENLESS_TOOL_READY_SPEC`, `TOKENLESS_ENV_FIX_SCRIPT`)
and the three `config.json` toggles are pinned through their environment
overrides. `remote_verify.sh` check 0 proves every lookup lands in the workspace.
The container sweep in `clean_between_repeats.sh` matches every OpenClaw sandbox
on the host, so it runs only with `L4_CLEAN_CONTAINERS=1`. If containers remain
without this opt-in, repeat cleanup refuses to proceed; inspection or removal
failure is not reported as a clean testbed. Each cleanup records the observed
container presence and opt-in in `logs/cleanup.provenance`, with repeat/phase
and started/completed outcomes. Repeats require completed pre- and post-clean
records for that repeat; absent or partial records set `cleanup_incomplete`.
Log collection failures return non-zero rather than silently losing provenance.
The preflight verifies
both the deployed Tokenless executable and the plugin bundle against their
freshly built counterparts.

**Pilot under a cap.** `run_pilot.sh` runs one repeat (default arms A, B, C on
the first instance of each shard; E and F duplicate C, and D needs a canary
rollout) and stops the sweep once the spend reaches
`L4_COST_CAP` minus a margin. Spend is read from the live transcript stores and
priced as an upper bound (cached input at the full input price); the ledger in
`out/repeat-1/pilot-cost.json` also carries a discounted view at the published
cached price and a full-scale estimate. An unreadable existing store or a known
per-turn/aggregate mismatch stops the sweep with `stop_reason=metering_failure`
and exit 2; a cap stop is separately recorded and exits 3. Sweep failures retain
a non-zero exit status. Required input/output/cache-read usage fields must be
non-negative integers; missing or null fields are not fabricated zeroes.
Missing data is not a zero-cost observation.

**Readiness is not compression.** `L4_DRY_RUN=1` checks local proxy endpoints and
wiring without sending a completion or warming a model. Only the subsequent
instance run proves actual compression; its first provider call remains in the
instance's billed usage. All paired arms default to `L4_WORKERS=1`: shared proxy
counters can prove per-instance intervention only for serial runs, and a sweep
containing C–F rejects a different worker count.

**Tool-result capture.** With `L4_CAPTURE_TOOL_RESULTS=1` (the default) every arm
writes `tool-results.json`: arm B from tokenless' stats store, arms C–F from the
proxy's message log, for which the proxies run with `--log-messages`. The flag is
identical for C–F; it only enlarges the line each proxy already logs per request,
a cost A and B do not carry, so per-request latency is not compared across that
boundary. Each row is one changed tool result — `{instance, arm, call_id,
tool_name, original, compressed}` — the input of the LLM semantic evaluation.
Tokenless capture includes both `compress-response` and `compress-toon`. When
recovery payloads cannot be exported, `retrieval` is null and
`recovery_observation` is `unobservable`, not an observed empty recovery store.
Tokenless retrieval-event rows, where available, are recorded separately as
`recovery_events`; they are not substitutes for the original payload. Headroom's
recovery-tool count reads the current session's SQLite transcript when JSONL is
absent; missing transcript sources yield null rather than zero.

## Arms and their configuration

Six arms: A is the reference, B is the component under test, and C is the
comparison side at its defaults. D, E, and F each apply exactly one
comparison-side proxy switch relative to C — **not cumulatively** — so a
difference from C is attributable to that single switch.

For the refreshed Headroom version, the proxy default is `cache`, not `token`,
and code-aware compression is on by default (`--code-aware/--no-code-aware`,
default enabled once tree-sitter grammars are installed). The C/D/E/F startup
expectations follow both defaults. F's explicit `--mode cache` and E's explicit
`--code-aware` therefore duplicate C's default configuration -- their startup
banners differ only in the port -- and neither is an independent compression
contrast; do not report them as new mechanisms. D's interceptor requires an
explicitly selected `canary`/`dev` rollout. Stable rollout is rejected before an
inactive D arm can produce baseline-like numbers, and rollout selection must be
reported as an experimental condition. These changes do not authorize a billed
sweep before compressor/adapter preflight and pilot validation.

| Arm | Base config | Plugin wiring | Extra proxy flags |
| --- | --- | --- | --- |
| A | `arm-a-baseline.json` | none declared | — |
| B | `arm-b-tokenless.json` | `tokenless.enabled` | — |
| C | `arm-c-headroom-default.json` | `headroom` on `contextEngine` slot, port 8801 | none (defaults) |
| D | `arm-d-headroom-tool-results.json` | same, port 8802 | `--intercept-tool-results` |
| E | `arm-e-headroom-code-aware.json` | same, port 8803 | `--code-aware` (not `--intercept-tool-results`) |
| F | `arm-f-headroom-cache.json` | same, port 8804 | `--mode cache` (not `--intercept-tool-results`) |

The configs are strict JSON, not JSON5: the runner copies the file verbatim to
`openclaw.json` inside each per-instance profile, so a comment would have to
survive that copy. That is why the differences are documented here instead.

`diff` is the intended way to audit them. A and B differ only by B's added
tokenless plugin declaration; C, D, E, and F differ only in the proxy port, which
appears in two fields (`baseUrl` and `proxyUrl`). Everything else that separates
C–F lives in how the proxy for that port was started — the proxy flags in the
table above — not in the agent config.

### Four wiring traps these configs exist to avoid

**`autoStart` must be `false`.** The headroom plugin defaults it to `true` and
will launch its own proxy. Every difference between C, D, E, and F is a proxy
*launch* argument, so an auto-started proxy would collapse all four arms into a
single default-configuration arm while still producing four sets of plausible
numbers. Each arm therefore pins `proxyUrl` to a proxy started out of band on
its own port.

**`slots.contextEngine` is load-bearing, and `enabled` alone is not enough.**
The slot is exclusive: only one registered context engine resolves per run, and
other enabled context-engine plugins still load without being consulted. A
config that enables headroom but leaves the slot unclaimed measures the baseline
under a headroom label. This is not hypothetical — the provisioned host was
found with `tokenless.enabled: true` alongside `slots.contextEngine: "headroom"`
while headroom itself was disabled.

The runner already defends this, and the defence — not the config — is
authoritative. `--headroom` makes it claim the slot; omitting the flag makes it
*delete* any inherited claim. An arm therefore is not defined by its config
alone but by the config **and** the flags it is launched with, and the two must
agree. The `plugins` blocks in these files are a declaration of intent that
matches the flags for that arm; where they disagree, the flags win.

Isolation itself rests on something stronger than either. A globally installed
plugin is invisible to a profile until the runner links its extension directory
in, so a profile without that link cannot load the plugin regardless of what the
copied config says. Fields the runner does not manage — `proxyUrl`, `autoStart`,
`gatewayProviderIds` — survive untouched, which is why the proxy wiring has to
live in the config and cannot be passed as a flag.

That layering is also what turns config/flag disagreement into a *detectable*
fault instead of a silent one. Each config declares only the plugin its own arm
links — arm A declares none, arm B only tokenless, arms C–F only headroom — and
OpenClaw warns `plugin not found: <id> (stale config entry ignored)` whenever a
config names a plugin the profile never linked. So the warning appears exactly
when an arm has quietly degraded into the baseline, which makes its absence a
free per-run acceptance check: **any run whose log carries that warning must be
discarded, not interpreted.** Declaring the other side as `enabled: false` would
have traded this signal for permanent noise.

**The two sides do not intervene at the same layer.** tokenless is a hook
plugin (`activation.onCapabilities: ["hook"]`) that rewrites and compresses tool
results. headroom declares `kind: "context-engine"` and takes over the context
pipeline. The comparison is between two different intervention points, not
between two implementations of one mechanism, and the report must say so.

headroom also declares `contracts.tools: ["headroom_retrieve"]`, so arms C–F
expose one tool that A and B do not. That is a behavioural difference beyond
compression and belongs in the limitations section alongside the toolchain
deviation.

**What arm B exercises under the pinned OpenClaw.** tokenless rewrites a command
only when the host hands its PreTool hook a stable tool-call id, and keeps the
command unchanged otherwise. In sandboxed runs on OpenClaw 2026.8.2 no rewrite
has fired, although replaying the same commands through `tokenless compress`
rewrites about one in five, and the plugin rewrites them as soon as it is given
an id; the host evidently supplies none on this path. `tokenless_retrieve` does
not reach the agent's tool list either. Arm B therefore measures PostTool
compression of tool results alone, which on SWE-bench Lite's short shell outputs
mostly ends in `no_savings` or a pass-through. A strong activation record proves
the hooks ran, not that the model saw anything different, so the report counts
changed results separately. A release that does supply the id adds a
requirement: rewritten commands name the host's tokenless executable and data
directory by absolute path, and both must be mounted into the sandbox before
such a command can run there.

**Memory is on by default, and it is the one trap that corrupts everything
downstream.** `plugins.slots.memory` defaults to `memory-core`, and OpenClaw
ingests ordinary sessions automatically — the documented exclusions cover cron,
heartbeat, and subagent sessions, not normal agent turns. The consequence is not
a measurement artefact but an invalidated experiment: after arm A works through
the instance set, the store holds knowledge from those solutions, and arm B
starts with it. Arm B then looks stronger for a reason that has nothing to do
with compression, and the effect is indistinguishable from the one being
measured. Worse, it is directional — whichever arm runs later benefits — so it
fabricates a difference rather than adding noise.

All six configs therefore set `plugins.slots.memory: "none"`, the documented
value for disabling memory plugins outright. Because it is identical across arms
it introduces no between-arm difference of its own. It does narrow what the
results describe: these numbers characterise a memoryless agent, so they say
nothing about how either side behaves once recall is in play. That belongs in the
limitations section, not in a footnote.

### Model snapshot and cost evidence

Every arm now selects `qwen3.8-max-0902`, the fixed snapshot Alibaba Cloud's
model-release notes also call `qwen3.8-max-2026-09-02`. The `0902` id is the one
the compatible endpoint's `/v1/models` lists, and the drift sentinel in
`remote_run.sh` looks the configured id up in that list, so the dated spelling
would stop the sweep before its first call. `qwen3.8-max-prime`, listed beside
it, is a higher-throughput SKU of the same model rather than a newer snapshot.
The old `qwen3-coder-plus` rolling-alias run is historical evidence, not the
configuration or cache-accounting evidence for this rerun.

The real pilot must verify the snapshot is callable through the selected region
and OpenAI-compatible endpoint, and record prompt/completion/cached usage fields.
A missing cache field is unknown, not a zero-cache observation. Model `created`
metadata remains recorded for traceability but does not prove identical weights.
Zero model-pricing placeholders have been removed from the arm configs; actual
cost requires region-specific provider prices and reported usage.

The staged snapshot configuration and offline tests do not constitute a billed
pilot or a successful L4 run. A missing provider key blocks both, without falling
back to another model or reusing the old report.

### Unverified assumptions in these configs

The wiring above is derived from the bundled OpenClaw documentation and the two
plugin manifests. Three values in it have not been executed yet and must be
confirmed by the smoke run before any billed run starts. They are listed here
rather than presented as settled facts.

**Model metadata is authored because the provider does not expose it.**
`contextWindow: 1000000` and `maxTokens: 65536` were written from intent. This
is not an omission that could be corrected by asking: DashScope's `/v1/models`
returns only `id`, `object`, `created`, and `owned_by` per entry, with no window
or output-limit metadata, so there is nothing to read back. For a custom provider
both fields are optional and OpenClaw falls back to `200000` for context
budgeting when they are absent, so a wrong value here silently changes budgeting
behaviour instead of erroring. Nor can it be checked through OpenClaw cheaply:
every `openclaw models *` subcommand resolves secrets through the gateway and
refuses to run without gateway credentials or a paired device, while the runner
deliberately uses `openclaw agent --local`, which bypasses the gateway entirely.
The values therefore stay authored, and the residual risk is that all six arms
share the same wrong budget — which biases absolute numbers but not the
between-arm comparison, since every arm reads the same declaration.

**The `${DASHSCOPE_API_KEY}` placeholder is honoured, but only from the process
environment.** OpenClaw resolves the placeholder and, when the variable is
absent, names it precisely: `missing env var "DASHSCOPE_API_KEY" at
models.providers.bench-qwen.apiKey`. Two consequences follow. First, the syntax
and the config path are confirmed correct. Second, this is a *warning*, not a
hard error — it continues with "feature using this value will be unavailable",
so a missing key surfaces only later as a failed model call. The key must
therefore reach the runner's own environment: the per-instance profile directory
contains no `.env`, so OpenClaw's global runtime dotenv
(`$OPENCLAW_STATE_DIR/.env`) is not a usable channel here. Following the L2
convention, the key travels through the ssh session environment and is never
written to a file on either machine.

**Arms C–F may lose usage accounting.** OpenClaw advertises streaming usage
compatibility for a fixed list of native DashScope hosts. The headroom plugin
rewrites the upstream base URL in memory to a loopback address, which is not on
that list. Token counts are the primary metric, so if the rewrite moves the
route off the compatible path and usage stops being reported, arms C–F produce
no usable numbers. Verify that a C-arm run reports non-zero prompt and
completion tokens before extending the run to D, E, and F.

## Environment facts that the provisioning encodes

These are not incidental: each is the difference between "this reproduces" and
"this does not", and each is why the setup script looks the way it does. The
target is **Alibaba Cloud Linux 4** (dnf/rpm). Node, Rust and npm upstreams
answer directly; Hugging Face remains blocked and uses a configurable mirror.
An earlier evaluation host needed broader mirroring and was apt-based, and the
script header records that narrower current boundary.

| Fact | Consequence in `remote_setup.sh` |
|---|---|
| Target OS is Alibaba Cloud Linux 4 (dnf/rpm), not Debian/apt | the build prerequisites (gcc, make, git, rsync, openssl headers, python3 venv) are checked and a missing one fails the setup; nothing is installed through dnf, and `patchelf` comes from PyPI into a private tools venv |
| `nodejs.org`, `static.rust-lang.org`, and `registry.npmjs.org` answer directly; `huggingface.co` remains blocked | Node/Rust/npm fetches go direct; datasets and the comparison-side tokenizer use `${HF_ENDPOINT:-https://hf-mirror.com}`. pypi and crates.io stay mirrored purely for transfer speed |
| dnf ships no Node in openclaw's required range | the pinned `L4_NODE_VERSION` tarball is fetched from `nodejs.org` and unpacked to `$L4_REMOTE_WORK/node`, reached through `PATH` only |
| `npm install -g openclaw` succeeds while skipping install scripts | bundled plugins are never materialised and `koffi` / `tree-sitter-bash` never build native bits; the install passes an explicit `--allow-scripts` list |
| npm's latest OpenClaw moves under the runner: 2026.11.1 rejects the `agents.list` profiles swe-runner writes, which 2026.8.2 accepts with a deprecation warning | the release is pinned in `l4_env.sh` (`L4_OPENCLAW_VERSION`, default `2026.8.2`, the release the runner's transcript reader was built against); setup installs exactly it and refuses a workspace config another release wrote, and `remote_verify.sh` checks the release and that a runner-shaped profile loads |
| `registry.npmjs.org` can time out mid-tarball under load | npm is given generous retries and a raised timeout rather than a registry substitution |
| tokenless links RTK as a library from `third_party/rtk`, a gitignored checkout that `just setup-rtk` clones and patches | `just setup-rtk` runs before the one `cargo build`, against the pinned tag and tracked patches, so the embedded RTK is the pinned one; its revision is recorded in `logs/rtk_commit.txt` |
| tokenless' `detect.sh` exits 1 when `~/.openclaw` does not exist | the profile dir is created and `install.sh` is invoked directly, which is equivalent |
| `tree-sitter-language-pack` 1.16.x ships a 2.4 MB wheel and downloads grammars at first use | pinned to `0.13.0` (~20 MB, grammars built in) so `--code-aware` parses offline instead of failing closed |
| pip/cargo mirror and toolchain settings must persist across steps and shells | written to `$L4_REMOTE_WORK/pip.conf` and `$L4_REMOTE_WORK/cargo/config.toml`, which only the workspace's environment reads; no shell rc file and no `/etc/environment` is written |

### Upstream URL composition

When the comparison side proxies to an OpenAI-compatible endpoint, the base URL
is given as `OPENAI_TARGET_API_URL` and the proxy appends the concrete path
itself. Its resolver strips a trailing `/v1` before doing so, so
`.../compatible-mode`, `.../compatible-mode/v1` and `.../compatible-mode/v1/` all
resolve to the same request URL. Verified by calling the resolver directly on the
benchmark host rather than inferred from the code.

### Comparison-side toolchain

The comparison side (`headroom-ai`) builds through maturin, and its
`rust-toolchain.toml` pins an exact Rust version. The provisioning installs that
pin and builds with it: `RUSTUP_TOOLCHAIN` is deliberately left unset so maturin
reads the toolchain file, and `RUSTUP_DIST_SERVER` is not set either, so the pin
is fetched directly from `static.rust-lang.org` rather than through a mirror that
might lack it. The resolved version is recorded in `logs/headroom_toolchain.txt`,
and the report must cite that value. (An earlier evaluation host could not reach
the pin through its mirror and fell back to `RUSTUP_TOOLCHAIN=stable`; that
deviation no longer applies from these hosts and must not be carried into the
report.)

## Design constraints that shape the run plan

Five properties of the harness constrain how runs may be scheduled and what a
repeat actually buys. They are recorded here because each one silently produces
wrong numbers rather than an error.

**A missing per-instance prompt does not fail the run.** `build_openclaw_prompt`
uses the lenient prompt loader: an absent or empty prompt file emits a single
`PER_CASE_PROMPT_SKIPPED` warning and the instance proceeds *unguided*. The task
still completes and still reports tokens, so a partially-missing corpus turns the
guided arm into a diluted mixture of guided and unguided instances. The corpus is
not part of this repository (see below), so this is a live risk on every host.
`remote_verify.sh` check 8 gates on it up front, and every run's logs must be
checked afterwards:

```bash
grep -rc PER_CASE_PROMPT_SKIPPED <run-output>/   # must be 0 for guided arms
```

**Runs of the same instance must not overlap.** The agent container is labelled
`agent:<instance_id>:main`, derived from the instance id alone. Docker labels are
daemon-global, so separate profiles and separate output directories do not
isolate them, and the runner's stale-container cleanup will `docker rm -f` a
concurrently running peer. Parallelism therefore has to come from `--workers`
inside a single run, not from concurrent runs.

**At `temperature=0` the seed is inert.** The runner pins greedy decoding for
byte-comparable regression runs. Under greedy decoding a seed changes nothing, so
N repeats are not N independent samples — they only detect server-side
non-determinism. Measuring run-to-run variance requires raising `--temperature`,
and `--seed` must then vary across repeats or a provider that honours it will
return identical samples anyway.

**Comparisons are paired, and the pairing is the point.** Every arm runs the same
instance set, so per-instance difficulty cancels. Interval estimates must use a
paired method (e.g. McNemar for solve rate); a two-independent-proportions
interval is both wrong and far too wide for this design.

**Compression must be proven per run, not assumed.** An arm whose plugin failed
to load still completes tasks and still reports tokens — it just silently becomes
another baseline. Any arm claiming compression needs positive evidence from the
proxy's own counters, and a run without it is void rather than a data point.

## Offline regression checks

The component target `make test-benchmark-l4 L4_TEST_PYTHON=<venv-python>` runs the
L4 offline pytest suite, the shared SQLite billing regressions, and checks every
L4 shell script with `bash -n`. The
Tokenless CI job invokes it in a private Python 3.12 environment with pytest,
pydantic and tiktoken; it makes no Docker or model calls. Required billing-module
imports fail the tests instead of skipping them when dependencies are missing.

`remote_fleet.sh collect` retrieves `out/`, `logs/` and the complete `eval/` tree,
including `.eval-complete`. `analyze_solve_rate.py --evals <runs-root> --runs
<runs-root>` can read this collected host layout and validate the current
predictions. Report design fields are read from the matching arm configuration
snapshot, not a cached model/cost constant; use `analyze_runs.py --configs-dir
<original-configs>` for historical runs. Workers, timeout and pull registry are
recorded in the sweep summary; missing historical runtime values remain unknown.


Run `python -m pytest src/tokenless/benchmark/l4-end-to-end/tests -q` from the
repository root. The checks use synthetic run trees and stub build/evaluation
commands; they do not connect to a host, invoke Docker or call a model.

An arm-level token-report refusal excludes the whole cell from the clean pool;
a current report naming aggregate mismatches excludes only those instances.
Each report attempt uses a unique candidate file, so an older report cannot
supply evidence for a failed retry. Provisioning always invokes incremental
Cargo builds and records the Tokenless executable SHA-256 rather than trusting
that an executable already exists.

Evaluation completion is bound to the predictions SHA-256. Changed predictions
are scored under a distinct run id; pending or failed attempts cannot reuse a
prior completion marker. Solve-rate collection selects only the matching report
for fingerprinted cells, without changing the official FAIL_TO_PASS/PASS_TO_PASS
criterion. The collector also compares the marker with the current `preds.json`;
use `--runs <collected-run-root>` when runs and evaluations were collected into
separate directories. Unbound historical reports require explicit
`--allow-legacy` and are labeled `legacy_unverified`; without a selected-task
denominator they can be inspected but cannot yield a solve rate. The denominator
comes from `selection.json` or run metadata, not just submitted patches, so
preparation failures, timeouts and patch-extraction failures remain unresolved.
A completed attempt with no predictions contributes zero resolved tasks; an
unfinished attempt or patches lacking a current evaluation remain `unscored`
and suppress the pooled rate. Every retry archives the old run tree before
creating new predictions, results and profiles. A newly started but unfinished
sweep invalidates its prior clean verdict, report and scoring marker.

## Inputs this repository does not contain

The per-instance prompt corpus lives in a separate internal `swe-runner` clone,
not here, and `swe-runner`'s own default prompt directory
(`src/swe_runner/prompts/`) does not exist in this monorepo. The guided arms
therefore depend on an input that a fresh clone of ANOLISA cannot produce, and
the corpus must be staged on the remote host and passed explicitly. **An outside
reader cannot reproduce the guided arms from this repository alone** — that
belongs in the report's limitations, not in a footnote.

To keep the two trees distinguishable, the remote workspace names them by role
rather than by origin:

| Path | Role |
|---|---|
| `$L4_REMOTE_WORK/runner` | symlink to the synced monorepo `swe-runner` — the revision under test |
| `$L4_REMOTE_WORK/swe-runner` | the corpus clone; its `src/swe_runner/prompts/` holds the 70 prompt files |

Both trees are called `swe-runner` at their origin, and conflating them would
install a different runner revision than the one being reported. The corpus path
must be passed to the runner explicitly via `--prompts-dir`; the default resolves
to the *installed* package's own `prompts/` directory, which does not exist in
this monorepo.

## Coverage ceiling

The guided instance set is skewed by construction: it is the subset for which
per-instance prompts exist, and the repositories are not evenly represented. No
amount of repetition fixes a skewed instance set — repeats reduce uncertainty
about the mean *on these instances*, while breadth is what governs whether the
result generalises. These are different quantities and must not be traded against
each other in the report.

The counterpart condition is the **same** instances run without per-instance
prompts. Keeping the instance set fixed makes the prompt condition the only
variable, so the difference is attributable; swapping in a different, larger
instance set would change the repository mix, the difficulty mix and the prompt
condition simultaneously, and no single difference could then be explained.

## Method

The L1–L3 method rules in [`docs/BENCHMARKS.md`](../../../../docs/BENCHMARKS.md)
("Method") continue to apply, in particular: one authoritative token counter, no
retention credit where nothing was compressed, and a negative control per metric.
Two rules are specific to this layer:

- **Task outcome comes from the dataset's own tests**, never from the agent's
  self-report.
- **Wall-clock is reported alongside tokens.** A per-call microsecond saving must
  not be presented as an end-to-end win.
