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

These checks run as a regular user without an Agent or service login. Cargo
downloads uncached dependencies; schema validation reads only bundled resources.
The runner, local service, Provider Host execution, command execution and FileJournal require Linux;
this gate does not certify other operating systems or minimum supported versions.

For pull requests that change `src/aw/**`,
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
`src/validation.rs` retains its fixed 711-line ceiling. Split by responsibility
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

PRs run AW checks only when `src/aw/**` changes. The daily gate retains all
credential-free security, Tokenless, lifecycle and Agent/Hook matrix contracts,
including native-inventory validation and source-size checks. Other components'
internal changes are owned by their own CI. An externally changed public contract
consumed by AW needs explicit compatibility validation on the candidate SHA.

[AW Integration](../../.github/workflows/aw-preview.yml) runs every night at 03:23
Asia/Shanghai and accepts a full 40-character `candidate_sha` for manual
pre-release validation. It builds all four Preview packages once, exercises real
sec-core health and Tokenless/RTK contracts, and verifies installation, service
lifecycle, side-by-side prefixes for rollback, and uninstall. Side-by-side
acceptance uses the same candidate build; it does not certify cross-version data
migration. Native jobs reuse checksummed binaries from that exact build.

[tests/native-matrix.json](tests/native-matrix.json) records fixed runtime versions,
owners, bounded commands and the precise acceptance boundary for every registered
Agent. The nightly job matrix is generated from this inventory. AgentScope,
Claude SDK/CLI, Hermes, OpenClaw, Pi and QwenPaw have automated suites. Qoder remains
manual because CI has no provisioned native CLI/model environment; it appears as
**MANUAL — not tested**, never as a native pass. OpenClaw covers official Hook
runners. The QwenPaw suite covers native AgentScope middleware and AW plugin
registration against a simulated API; QwenPaw plugin loading, App/API startup
and model turns remain manual. Full OpenClaw Gateway model flows also require
explicit acceptance. Deterministic local models need no cloud keys.

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
commit and complete the documented manual native boundaries.

The daily Rust cache is restored for PRs and written only by main pushes; the
heavy build cache is written by nightly. Native Python/npm dependencies are
cached separately, and native concurrency is capped at three jobs. To validate
the new integration workflow on a branch before it reaches main, dispatch the
existing AW workflow with `integration=true`:

```bash
gh workflow run aw-ci.yml --ref <branch> -f integration=true
# After the integration workflow is on the default branch:
gh workflow run aw-preview.yml --ref <branch> -f candidate_sha=<40-character-SHA>
```

Local inventory planning and native execution use the same entrypoints as CI:

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

The binary directory must contain the nightly bundle and checksums. Install the
inventory's Python version, Node 24.15.0 and uv 0.11.13 first. Native execution
owns one short temporary directory for Linux Unix sockets and removes it on exit;
retained evidence belongs under `target/`. Native jobs use standalone Python and
private Hermes tools/environments so runner loader overrides or shared writable
caches do not require weaker native admission. A full run failure is not converted to
success by retrying or omitting a framework.

## Crate boundaries

| Crate | Responsibility |
| --- | --- |
| `aw-contracts` | Versioned capability schemas and cross-record validation |
| `aw-config` | Desired configuration parsing and static validation |
| `aw-provider` | External Provider protocol and capability admission; depends on `aw-config` |
| `aw-core` | Plan execution through trusted runtime ports; depends on `aw-contracts` |
| `aw-exec` | Bounded Linux command transport and owned process-group cleanup; independent of Provider protocols |
| `aw-host` | Compose configuration, Provider admission and bounded transport into local preparation and invocation; depends on `aw-config`, `aw-provider` and `aw-exec` |
| `aw-service` | Standalone service, reusable client and native launcher; uses `aw-host` for Hook execution, `aw-exec` for foreground ownership and `aw-core` Journal for metadata |
| `aw-provider-tokenless` | PostTool Provider binary using the public Tokenless v2 CLI; no Tokenless crate dependency |
| `aw-provider-sec-core` | Concrete Provider binary; calls public sec-core CLI through `aw-exec`, with no sec-core crate dependency |
| `aw-package` | Rust Preview build, manifests, installation and dual-Agent configuration; `aw-service` is a test-only dependency |

Keep native framework integration outside these libraries and service; process execution belongs in `aw-exec`.
Keep Provider message parsing and offline admission in `aw-provider`; `aw-host`
owns their execution boundary without adding Provider semantics to raw command
transport or replacing the Core Host/Journal contracts.
Dependency and source-layout checks live in [scripts/check_layout.py](scripts/check_layout.py),
with regression tests in [tests/test_ci_checks.py](tests/test_ci_checks.py).
Changes to a crate boundary must update both the checks and their tests.

## Build the service

Build the CLI and sample Provider from `src/aw` on Linux:

```bash
cargo build --locked -p aw-service --bin aw
cargo build --locked -p aw-provider --example policy
```

The [user guide](../../docs/user-guide/en/user-entrypoint/aw.md#run-the-local-service-demo)
walks through a foreground service and separate client, including state-directory
and audit-history cleanup. The service uses the local `aw-service/v1alpha3`
protocol; Providers retain `aw-provider/v1alpha1`. They are versioned separately.

## Runtime validation

For focused local service checks:

```bash
cargo test --locked -p aw-service
```

Service tests use real Unix sockets, fixture Provider processes and private
temporary state directories. They need no model, Agent installation or cloud key.
They check shared event budgets, binding and once-only invocation, cancellation
and shutdown, durable audit, stale handles and endpoint ownership. Fixtures wait
for owned processes and remove temporary directories; build artifacts stay in
`target/`. See the [local service contract](docs/design/local-service.md) for the
validation boundary.

For focused Provider Host checks, run from `src/aw` on Linux:

```bash
cargo test --locked -p aw-host
cargo test --locked -p aw-provider-sec-core
```

These tests use local fixture processes to check preparation, request binding,
failure reporting, shared deadlines, cancellation and once-only event steps.
The [local example](docs/design/provider-host.md#local-example) exercises the
sample Provider using synthetic Adapter evidence and tool events. It is not
native Agent acceptance or proof of effect adoption.

Protocol and Core tests use synthetic inputs and Hosts. Native integration needs
separate evidence that callbacks were installed, tools ran or were blocked as
intended, and returned effects were adopted by the Agent. Record validation
commands and results in the pull request; keep experiment logs out of the README.
