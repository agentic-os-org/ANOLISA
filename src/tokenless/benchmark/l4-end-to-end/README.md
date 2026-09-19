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
| Harness language | Rust (standalone Cargo workspace per layer) | Python — drives [`src/benchmark/swe-runner`](../../../benchmark/swe-runner) |
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
`remote_verify.sh`, `remote_run.sh` (the on-host sweep), `remote_fleet.sh` (the
multi-host driver), `run_repeats.sh`, `clean_between_repeats.sh`,
`evaluate_all.sh`, and the analysis scripts `analyze_runs.py`,
`analyze_solve_rate.py`, `analyze_reproducibility.py`, and
`audit_prompt_disclosure.py`.

## Running

The runs are Linux- and docker-bound and take hours, so everything happens on a
remote host. Credentials come from the environment and are never committed.

```bash
export L4_SSH_HOST=<host>              # remote benchmark box
export L4_SSH_PASS=<password>
export HEADROOM_SRC=~/git_repo/headroom  # local comparison-side checkout

./assets/scripts/remote_sync.sh    # 1. ship sources (+ unit suite once a venv exists)
./assets/scripts/remote_setup.sh   # 2. provision (30-60 min on a cold host)
./assets/scripts/remote_sync.sh    # 3. re-run: now the unit suite actually executes
./assets/scripts/remote_verify.sh  # 4. pre-flight; must exit 0 before any billed run
```

`remote_setup.sh` is idempotent and safe to re-run: every step is guarded by a
check for its own output, so a resumed run skips what already succeeded.
`remote_verify.sh` is the gate that must pass before a run is worth paying for —
each of its checks guards a failure mode that would otherwise produce plausible
but meaningless numbers rather than an error.

## Arms and their configuration

Six arms: A is the reference, B is the component under test, and C is the
comparison side at its defaults. D, E, and F each apply exactly one
comparison-side proxy switch relative to C — **not cumulatively** — so a
difference from C is attributable to that single switch.

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

### Model id: rolling alias over dated snapshot, and why

DashScope offers `qwen3-coder-plus` alongside dated snapshots
(`qwen3-coder-plus-2025-07-22`, `qwen3-coder-plus-2025-09-23`). The alias moves;
a snapshot does not. Pinning a snapshot is the usual choice for a benchmark, and
it was rejected here for a measured reason.

The two report usage differently. Four alternating minimal calls gave a stable
result: the alias returns `prompt_tokens_details.cached_tokens`, the dated
snapshot omits the field entirely. Cache accounting cannot be given up in this
experiment, because compression rewrites the prompt prefix and prefix rewriting
is exactly what invalidates a prompt cache. An arm could then cut prompt tokens
while raising real cost, and without `cached_tokens` that outcome is
indistinguishable from a win. Reproducibility was traded for the ability to see
it.

The cost of that trade is real and is not hidden: the alias can be repointed
mid-run, which would make earlier and later instances incomparable with no
visible symptom. The only sentinel the API offers is the alias entry's `created`
timestamp. It must be recorded before and after every run and reported alongside
the results; if it changes, the run spans two models and is void. At the time
these configs were written the value was `1785331635`
(`2026-07-29T13:27:15Z`) — within a day of the run itself, which is direct
evidence that this alias does move.

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
target is **Alibaba Cloud Linux 4** (dnf/rpm), and from these hosts the upstreams
answer **directly** — an earlier evaluation host was mirror-routed and apt-based,
and the script header records that reversal.

| Fact | Consequence in `remote_setup.sh` |
|---|---|
| Target OS is Alibaba Cloud Linux 4 (dnf/rpm), not Debian/apt | package names and the installer are dnf's, verified against a live host; `nodejs` is not packaged at a usable version |
| Upstreams answer directly (`nodejs.org`, `static.rust-lang.org`, `huggingface.co`, `registry.npmjs.org`) | fetches go direct; the earlier mirror indirection is gone wherever it changed *what* installs. Only pypi and crates.io stay mirrored, purely for transfer speed |
| dnf ships no Node in openclaw's required range | the pinned `L4_NODE_VERSION` tarball is fetched from `nodejs.org`, unpacked to `/opt/node`, and symlinked into `/usr/local/bin` |
| `npm install -g openclaw` succeeds while skipping install scripts | bundled plugins are never materialised and `koffi` / `tree-sitter-bash` never build native bits; the install passes an explicit `--allow-scripts` list |
| `registry.npmjs.org` can time out mid-tarball under load | npm is given generous retries and a raised timeout rather than a registry substitution |
| tokenless' `make build` calls `just setup-rtk`, which re-clones rtk from GitHub | `just setup-rtk` is run directly against the pinned clone, so the measured rtk is the pinned one |
| tokenless' `detect.sh` exits 1 when `~/.openclaw` does not exist | the profile dir is created and `install.sh` is invoked directly, which is equivalent |
| `tree-sitter-language-pack` 1.16.x ships a 2.4 MB wheel and downloads grammars at first use | pinned to `0.13.0` (~20 MB, grammars built in) so `--code-aware` parses offline instead of failing closed |
| pip/cargo mirror and toolchain settings must persist across steps and shells | written to `/root/.pip/pip.conf`, `/root/.cargo/config.toml`, and `/root/.bashrc`; no `/etc/environment` is written |

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
