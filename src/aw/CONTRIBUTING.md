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

[AW CI](../../.github/workflows/aw-ci.yml) runs on branch pushes, pull requests,
merge groups and manual dispatch. It checks the candidate commit, including the
merge result for pull requests. Unrelated changes produce an explicit no-op;
scope errors, unexpected skips and mismatched tested commits fail `AW / required`.
Repository administrators must select that check in branch protection to enforce
it. A cancelled workflow is not a passing gate.

Upstream CI uses the self-hosted `anolisa-k8s-general-ci-x64` runner; fork CI
uses GitHub-hosted Ubuntu 24.04. The jobs separate scope selection from full
validation:

- `AW / scope` checks out the gate script and its tests, retaining full Git
  history for the complete PR diff and base-branch changes in the merge result.
  It runs scope and required-result tests with the runner's Python 3.9 or newer;
  it does not install Python or Node.js.
- `AW / contracts` retains a full working tree with shallow history for Cargo's
  package-file discovery. It runs the complete gate, including all gate behavior
  tests, with Python 3.12.3, Node.js 24.15.0 and the pinned Rust toolchain.
- `AW / required` checks out only the gate script and verifies the job results
  and candidate SHA with the runner's Python 3.9 or newer.

Sparse checkout limits downloaded file contents in the two control jobs while
preserving the commits needed for scope selection. Job timeouts remain 5, 25 and
5 minutes respectively.
Local validation also uses Linux ARM64.

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
| `aw-provider-sec-core` | Concrete Provider binary; calls public sec-core CLI through `aw-exec`, with no sec-core crate dependency |
| `aw-package` | Rust Preview build, manifests, installation and dual-Agent configuration; `aw-service` is a test-only dependency |

Keep native framework integration outside these libraries and service; process execution belongs in `aw-exec`.
Keep Provider message parsing and offline admission in `aw-provider`; `aw-host`
owns their execution boundary without adding Provider semantics to raw command
transport or replacing the Core Host/Journal contracts.
Dependency and source-layout checks live in [scripts/check.py](scripts/check.py),
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
and audit-history cleanup. The service uses the local `aw-service/v1alpha2`
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
