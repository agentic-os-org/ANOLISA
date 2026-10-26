# Contributing to AW

[中文版](CONTRIBUTING_zh.md)

This guide covers AW development checks. For repository-wide contribution and
commit rules, see the [repository contribution guide](../../CONTRIBUTING.md).

## Run the checks

Prepare Rust through rustup, Python 3 and Node.js. Rust, rustfmt and Clippy are
pinned in [rust-toolchain.toml](rust-toolchain.toml). Run from the repository root:

```bash
python3 src/aw/scripts/check.py
```

The entry runs CI behavior tests, formatting, Clippy, all locked workspace tests,
the Python/JavaScript digest vectors and rustdoc. Missing tools, empty or fully
ignored configuration, Provider protocol/admission, Provider Host, contract, plan,
Core execution, command execution, local service or journal test targets,
invalid vectors and command failures return nonzero. Each command has a timeout and its child process group is cleaned up on
failure or interruption. Logs identify the failing command; individual commands
can be run from `src/aw` for diagnosis.

The database, inventory, transaction, runtime-lease and package CLI tests also
run under child umasks `0022` and `0002`, including package planning, configuration,
Host and service lease ownership. Deterministic subprocess barriers cover lock
release while unrelated children are between fork and exec. Give fixtures and
copied executables explicit permissions; do not change a parallel test process's umask.

These checks run as a regular user without an Agent or service login. Cargo
downloads uncached dependencies; schema validation reads only bundled resources.
The runner, local service, Provider Host execution, command execution and FileJournal require Linux;
this gate does not certify other operating systems or minimum supported versions.

For pull requests changing AW, Providers, AW workflows or the declared Tokenless consumer inputs,
[AW CI](../../.github/workflows/aw-ci.yml) runs these checks against the candidate
merge commit. Run the command above on Linux before submitting AW changes. If CI
fails, find the failing command in the job log and rerun it locally from `src/aw`.

## Regression matrix and source limits

[tests/regression-matrix.json](tests/regression-matrix.json) maps each registered
Agent, Hook event/effect and lifecycle to exact runnable Rust tests. The compiled
Adapter catalog must match the matrix and configuration schema. Shared PreTool
and PostTool cases iterate all matrix Agents with explicit native payloads;
adding an Agent without its fixture, omitting a Hook cell, deleting a referenced
test or marking it ignored fails the existing `AW / required` gate.

Security coverage includes denial, scan errors, cancellation and offline
observation. Tokenless coverage includes fresh-call retry ownership, rejection
of in-place mutation, final text replacement, unsupported/error/no-savings
results, managed RTK isolation and optional view isolation. These Provider
fixtures and callback tests run on every relevant push without credentials.
Real SDK/CLI adoption and real Tokenless/RTK package acceptance remain separate,
explicit checks; do not reference ignored native tests as required coverage.

The gate builds workspace test harnesses once with Cargo's JSON artifact output,
lists each executable directly and reuses the build for the full workspace test
run. Every integration target must contain runnable tests, including newly added
targets. The explicitly declared `opt_in_targets` retain installed-package
acceptance outside this gate; they must exist and contain only ignored tests. Scope selection, candidate SHA verification and fail-closed required
status remain unchanged; shared security and Tokenless tests are never skipped
based on the changed Adapter.

Rust, Python and JavaScript (`.js`, `.mjs`, `.cjs`) sources, scripts and fixtures
have a 699-line ceiling, with an advisory at 600 lines. Only the existing
`crates/aw-contracts/src/validation.rs` retains its fixed 711-line ceiling. Split by responsibility
and preserve entrypoints and assertions; do not exempt test fixtures.
Dependency/layout checks are in [check_layout.py](scripts/check_layout.py),
compiled inventories in [check_inventory.py](scripts/check_inventory.py), and
matrix/README consistency in [check_matrix.py](scripts/check_matrix.py).

After changing supported capabilities, regenerate both README grids and run the
gate from `src/aw`:

```bash
python3 scripts/check_matrix.py --write-readmes
python3 scripts/check.py
```

Gate-only development tests need Python 3.11+ and no Rust or Node runtime:

```bash
python3 -B -m unittest discover -s tests -p 'test_ci_*.py'
```

## Daily CI and nightly integration

AW/Provider and AW workflow changes run the full gate. The daily gate retains all
credential-free security, Tokenless, lifecycle and Agent/Hook matrix contracts,
including native-inventory validation and source-size checks. Other components'
internal changes are owned by their own CI. Tokenless CLI/protocol/runtime/statistics
source, its AW Provider and real fixtures, RTK setup/patches, dependency/toolchain
inputs, shared Provider protocol/executor source and package-builder inputs select
real Tokenless compatibility (the exact paths are in
`scripts/check.py`). It uses the existing package builder, then explicitly runs
four named real-CLI Provider tests, including the attributed RTK retry and
PostTool contract. Missing/ignored tests, failures and mismatched tested SHAs fail
the required result. Other backend public contracts need explicit compatibility acceptance.

Scope selection starts independent AW and Tokenless jobs on the same candidate
SHA. Each job has its own runner, build directory and cache key; the Tokenless
job does not install Node.js. The existing `AW / required` check always joins
both job results and their tested SHAs. Failures, cancellations, unexpected skips
and missing evidence block it, including failures after a test output was published.
Only an explicit unselected scope permits a skipped job. AW checks retain a
35-minute deadline; the real backend build and contracts have a separate
60-minute deadline. Parallel elapsed time depends on available runners and caches.
Ordinary Agent, service, CLI, installer/transaction, documentation and shared-crate
test/example changes run the AW fixture gate without building real Tokenless/RTK.
Mixed changes still select real contracts when any consumed input changes. Manual
dispatch and forced pushes select both jobs. Tokenless's own component, adapter
and prebuilt CI workflows retain their triggers and full coverage independently.

Run this gate locally from a clean, committed checkout:

```bash
python3 -B src/aw/scripts/check_tokenless.py --output /absolute/new/evidence-directory
```

The output must be new; it retains package manifests, binary hashes, test logs
and cleanup records. No hosted credentials are required. Remove that exact
evidence directory after review; normal component build caches remain reusable.

[AW Integration](../../.github/workflows/aw-preview.yml) runs every night at 03:23
Asia/Shanghai and accepts a full 40-character `candidate_sha` for manual
pre-release validation. It builds all four Preview packages once, exercises real
sec-core health and Tokenless/RTK contracts, and verifies installation, service
lifecycle, side-by-side prefixes for rollback, and uninstall. Side-by-side
acceptance uses the same candidate build; it does not certify cross-version data
migration. Native jobs reuse checksummed binaries from that exact build.

The Python-free package stage uses `scripts/check_packages.sh` to cap test
parallelism at two, retaining real AW payloads and the 30-second CLI deadline.
Nested runner containers can expose more CPUs than their effective quota. Each
Cargo suite has a 600-second outer deadline; failures stop later suites. The
always-uploaded `test-evidence/` contains commands, elapsed times, exit codes and
before/after cgroup CPU, memory and I/O counters. CLI fixture failures identify
the exact command and working directory without dumping the environment.
Python is installed only after this stage for package inventory comparisons.
Installed prefixes and their database live in one private `/tmp` directory;
cleanup removes them while retaining demo evidence in the artifact directory.

[tests/native-matrix.json](tests/native-matrix.json) records fixed runtime versions,
owners, bounded commands and the precise acceptance boundary for every registered
Agent. The nightly job matrix is generated from this inventory. All eight Agents
have automated suites. OpenClaw covers official Hook runners; full OpenClaw
Gateway model flows still require explicit acceptance.

Qoder CLI 1.1.64 and QwenPaw 2.2.2b4 / AgentScope 2.0.9 each require four real
sec-core cases: `allow`, `scan-deny`, `backend-error` and `timeout-block`. The jobs
reuse the candidate's checksummed `aw`, Provider, `agent-sec-cli` and
`agent-sec-daemon` binaries. Each case starts a private Ubuntu 24.04 container,
pinned by the inventory to a multi-platform manifest digest, with the real daemon.
The container has no network and only its socket directory is writable on the
host. The resolved platform image ID is also recorded. The scheduled run selects
`github.sha` once; containers never fetch a moving branch or a separately built
nightly image.

A separate required Qoder 1.1.73 TUI case verifies native SessionStart readiness
and empty → added-hook → removed-hook policy activation before dispatch, retaining
the same PID/start ticks. It downloads a separately checksummed release and keeps
the 1.1.64 print lane. Its named metadata report and cleanup are required by the
native inventory. It does not certify hosted TUI tool turns or result replacement.
The TUI fixture removes `CI` and `GITHUB_ACTIONS` only from its child environment
and trusts only its disposable workspace through native `permissions.trustDirectories`.
The runner and print lane keep their environment and permission checks.

The normal case must write its marker. The risk case must match the builtin
`shell-recursive-delete` rule (`warn`, mapped to block), preserve its disposable
deletion fixture and omit the marker. Stopping the actual container tests backend
unavailability; pausing it tests the 2-second Provider deadline and `on_error:
block`. Both must leave the marker absent, including after backend recovery.
Native callbacks, sec-core events, AW audit and zero live bindings are checked
together. QwenPaw's native approval API approves only the exact fixture command
before AW rejects it. Every case stops its processes/container and checks socket,
plugin and port cleanup; profiles and temporary state are removed, never uploaded.
If a fixture exceeds its cleanup grace, the outer Linux subreaper kills and reaps
adopted descendants across sessions and removes containers carrying that command's
unique ownership label. It retains a cleanup report and fails the interrupted run.

QwenPaw drives its stock App/API with a deterministic loopback OpenAI-compatible
model, requiring no cloud key. Qoder CLI uses its real hosted Performance model; Qoder SDK 1.0.56 uses its
bundled Worker 1.1.78 and hosted `auto` model. Both require
the repository `QODER_PERSONAL_ACCESS_TOKEN` Secret, as documented in
[Qoder authentication](https://docs.qoder.com/cli/authentication). Dependency
installation does not receive the Secret. The Qoder launcher and its native
callbacks inherit it during execution; backend probes, Docker commands and
version checks explicitly remove it from their environments. Missing or expired
credentials fail acceptance rather than skip it. Forks need their own Secret to run this nightly lane. Qoder downloads have
fixed official URLs, archive checksums and a runtime version check; updates need
an explicit inventory change. PR contract gates remain credential-free.

Every native command declares a report contract: `unittest`/`tap` requires an exact
test count; `json` requires result files with exact case names or observed counts.
Execution and final aggregation both verify the evidence; exit zero or an outer
`passed` status alone cannot certify a suite. Keep reported counts tied to completed
checks rather than constants, and update the inventory when coverage changes.
Missing suites, stale SHAs and incomplete reports fail the final integration job.
Evidence artifacts retain the candidate SHA, component package manifests,
installed SDK versions/npm locks, command/PID/deadline records and per-suite
results. @kongche-jbw owns AW integration failures and routes backend contract
failures to the component CODEOWNERS. Before release, rerun on the exact release
commit and validate any additional release-specific native boundaries.

The daily Rust cache is restored for PRs and written only by main pushes; the
heavy build cache is written by nightly. Native Python/npm dependencies are
cached separately, and native concurrency is capped at three jobs. The full profile requires dispatch from
`main` and a candidate in the fetched `origin/main` history before any candidate
script runs or receives the Qoder Secret. A manual fork dispatch can select
`credential_free=true` for its exact workflow SHA. This profile runs packages, all
six credential-free Agents and the resident container; its summary explicitly
leaves Qoder CLI/SDK hosted-model acceptance to the full trusted-main nightly.
Both the caller and native job withhold the Qoder Secret in this profile.

```bash
gh workflow run aw-ci.yml --ref main -f integration=true
gh workflow run aw-preview.yml --ref main -f candidate_sha=<40-character-SHA>
```

### Minimal resident container

The additional required container job reuses every `tests/sdk_resident.py` command
and exact JSON report in the native inventory: currently 24 suites (16 Claude
and eight Pi) out of 49 native suites (42 in the credential-free profile). They
include 1,280-call sessions, state retirement, resource-growth assertions, callback
drain, session-store reuse and native denial, alongside all earlier lifecycle/MCP
cases. Model-relay and dispatch contract entrypoints remain in the host native lane.

The image contains only allowlisted public fixtures, pinned SDK dependencies and
four candidate binaries: `aw`, `aw-provider-tokenless`, `tokenless`, and `rtk`.
RTK is required by PreTool in the mixed Bash/MCP long-session suite. Ubuntu,
Node and uv images use manifest digests; Python and SDK versions come from the
native inventory, and Pi uses its committed npm lock. The build context excludes
the repository checkout, deployment files and user configuration. Execution uses
an unprivileged test user, synthetic data, loopback model fixtures, no external
network, no host mounts and no forwarded credentials. One image archive is shared by all four Agent/regular-or-long shards through a
one-day Actions artifact; its SHA-256, image ID and source revision are checked
after loading. No image is pushed to a registry.
Only declared result JSON, runtime versions, input digests and command/cleanup records are retained;
private profiles and per-task data remain in the removed container. Aggregation
rechecks every report, candidate SHA, runtime version and container/image removal.

Run the fork candidate profile explicitly:

```bash
gh workflow run aw-ci.yml --repo <owner>/anolisa --ref <branch> \
  -f integration=true -f credential_free=true
```

Run only the container locally from `src/aw`, using the matching nightly bundle:

```bash
python3 -B scripts/resident_container.py --candidate-sha "$(git rev-parse HEAD)" \
  --bin target/native-binaries --output target/resident-evidence
```

The output directory must be new and image inputs must be committed. The local
command builds once and runs all four shards; CI builds once and runs up to two
shards concurrently. Each shard deadline sums its declared case deadlines with
cleanup allowance; its job reserves another 15 minutes for image load, evidence
and cleanup. Host native job budgets likewise follow the inventory plus setup.
Aggregation requires all shards and the same cleaned-up build identity. The runner
removes its context, container and unique image tag on success or failure.
Image build/load interrupts wait for the bounded Docker operation to settle before
tag removal. A timed-out image operation records incomplete cleanup because the
daemon may still publish its tag; use the recorded command to check and remove it.
Retain
`target/resident-evidence` for inspection; remove it with
`rm -rf -- target/resident-evidence` after review. The container record includes
exact stop/removal commands if Docker becomes unavailable during cleanup.

Resident composition with real sec-core, SDK saturation/recovery, request ingress/drain
and supervisor restart remain separate acceptance work. The resident suite preserves
fail-close, original permissions and metadata-only evidence; it does not certify
production endpoints, traffic or deployment-specific shutdown integration.

Local inventory planning and native execution use the same entrypoints as CI:

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

To run only the pinned OpenClaw loader/runner suite from `src/aw`, reuse the
inventory's isolated npm installation shape:

```bash
npm install --prefix target/openclaw-native --ignore-scripts --no-audit --no-fund openclaw@2026.9.6
OPENCLAW_PACKAGE_DIR="$PWD/target/openclaw-native/node_modules/openclaw" \
  node --test --test-reporter=tap crates/aw-cli/tests/openclaw-native.mjs
```

All eight TAP tests must pass without skips. The suite loads the actual AW
entrypoint and manifest through OpenClaw's official loader, then exercises its
registered hooks and middleware. Policy aliases come from the pinned runtime.
Missing SDK dependencies or loader failures fail the suite. This focused command
does not replace native evidence aggregation or certify Gateway/model turns.

The binary directory must contain the nightly bundle and checksums. Install the
inventory's Python version, Node 24.16.0 and uv 0.11.13 first. Qoder and QwenPaw
also require a working local Docker daemon; Qoder requires its PAT in the
environment. Select either Agent with `--agent qoder` or `--agent qwenpaw` in the
same native entrypoint above. Native execution
owns one short temporary directory for Linux Unix sockets and removes it on exit;
retained evidence belongs under `target/`. Native jobs use standalone Python and
private Hermes tools/environments so runner loader overrides or shared writable
caches do not require weaker native admission. A full run failure is not converted to
success by retrying or omitting a framework.

## Crate boundaries

Local package changes enter `aw-package::plan`, which derives a read-only plan,
then `plan::execute` uses the shared durable transaction backend. Queries and
dry runs must not create records or locks. Exercise these paths on Linux:

```bash
cargo test --locked -p aw-package --test management --test database
cargo test --locked -p aw-package transaction::tests
cargo test --locked -p aw-exec --test packages
```

The package tests cover independent tools, exact same-prefix dependencies,
legacy record archival, conflicts and replay. Package leases in `aw-exec` are
reused by the CLI, service and Host without adding a runtime dependency on the
package database. Keep database-before-prefix lock ordering in mutation and
configuration publication. System scope is explicit raw management, not a native
package backend. See [local package management](docs/design/en/packaging/package-management.md)
for commit/recovery rules and remaining runtime discovery limits. The focused
commands do not replace the full AW gate or native acceptance.

Framework crates live in `src/aw/crates/<name>/`, concrete Providers in
the repository-root `providers/<provider>/` with independent workspaces; the root manifest only organizes the workspace. See
[Provider collaboration](../../providers/README.md) for contribution and distribution,
[Architecture](docs/design/en/architecture.md) for ownership and
dependencies, and the [design index](docs/design/en/README.md) for protocol reading order.

Keep native dialects and bridge assets in `aw-adapter`; `aw-cli` owns argument
parsing, RPCs, signals and resources. The service owns state, budgets and audit.
Both reuse the Adapter without exposing service internals to it.
`aw-provider` owns offline protocol validation; process execution uses `aw-exec`.
Changes to crate boundaries must update [check_layout.py](scripts/check_layout.py),
its [regression tests](tests/test_ci_checks.py), and the
[architecture tables](docs/design/en/architecture.md).

## Build the service

Build the CLI and sample Provider from `src/aw` on Linux:

```bash
cargo build --locked -p aw-cli --bin aw
cargo build --locked -p aw-provider --example policy
```

The [user guide](../../docs/developer-guide/en/aw/native-agents.md#run-the-local-service-demo)
walks through a foreground service and separate client, including state-directory
and audit-history cleanup. The service uses the local `aw-service/v1alpha7`
protocol; Providers retain `aw-provider/v1alpha1`. They are versioned separately.

## Runtime validation

For focused local service checks:

```bash
cargo test --locked -p aw-service -p aw-cli
```

Service unit tests and CLI integration tests use real Unix sockets, fixture Provider processes and private
temporary state directories. They need no model, Agent installation or cloud key.
They check shared event budgets, binding and once-only invocation, cancellation
and shutdown, durable audit, stale handles and endpoint ownership. Fixtures wait
for owned processes and remove temporary directories; build artifacts stay in
`target/`. See the [local service contract](docs/design/en/runtime/local-service.md) for the
validation boundary.

For focused Provider Host checks, run from `src/aw` on Linux:

```bash
cargo test --locked -p aw-host
cargo test --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
```

These tests use local fixture processes to check preparation, request binding,
failure reporting, shared deadlines, cancellation and once-only event steps.
The [local example](docs/design/en/runtime/provider-host.md#local-example) exercises the
sample Provider using synthetic Adapter evidence and tool events. It is not
native Agent acceptance or proof of effect adoption.

Protocol and Core tests use synthetic inputs and Hosts. Native integration needs
separate evidence that callbacks were installed, tools ran or were blocked as
intended, and returned effects were adopted by the Agent. Record validation
commands and results in the pull request; keep experiment logs out of the README.

The Qoder SDK native inventory runs both ordinary `aw run` and `--reloadable`
lanes. The latter adds existing-Query policy addition/removal, another session's
busy gate and held native callback cleanup through real `aw update` RPCs. Both
lanes use caller-owned `startDaemon()`, require the runtime-only PAT and assert
unchanged daemon identity and zero bindings/events after cleanup.
