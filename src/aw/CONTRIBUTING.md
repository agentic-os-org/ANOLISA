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
ignored configuration, Provider protocol/admission, contract, plan, Core execution, command execution or journal test targets,
invalid vectors and command failures return nonzero. Each command has a timeout and its child process group is cleaned up on
failure or interruption. Logs identify the failing command; individual commands
can be run from `src/aw` for diagnosis.

These checks run as a regular user without an Agent or service login. Cargo
downloads uncached dependencies; schema validation reads only bundled resources.
The runner requires Linux. Command execution and FileJournal require Linux;
this gate does not certify other operating systems or minimum supported versions.

[AW CI](../../.github/workflows/aw-ci.yml) runs on branch pushes, pull requests,
merge groups and manual dispatch. It checks the candidate commit, including the
merge result for pull requests. Unrelated changes produce an explicit no-op;
scope errors, unexpected skips and mismatched tested commits fail `AW / required`.
Repository administrators must select that check in branch protection to enforce
it. A cancelled workflow is not a passing gate.

Upstream CI uses the self-hosted `anolisa-k8s-general-ci-x64` runner; fork CI
uses GitHub-hosted Ubuntu 24.04. Both use Python 3.12.3, Node.js 24.15.0 and
the pinned Rust toolchain. Local validation also uses Linux ARM64.

## Crate boundaries

| Crate | Responsibility |
| --- | --- |
| `aw-contracts` | Versioned capability schemas and cross-record validation |
| `aw-config` | Desired configuration parsing and static validation |
| `aw-provider` | External Provider protocol and capability admission; depends on `aw-config` |
| `aw-core` | Plan execution through trusted runtime ports; depends on `aw-contracts` |
| `aw-exec` | Bounded Linux command transport and owned process-group cleanup; independent of Provider protocols |

Keep framework integration outside these libraries; process execution belongs in `aw-exec`.
Dependency and source-layout checks live in [scripts/check.py](scripts/check.py),
with regression tests in [tests/test_ci_checks.py](tests/test_ci_checks.py).
Changes to a crate boundary must update both the checks and their tests.

## Runtime validation

Protocol and Core tests use synthetic inputs and Hosts. Native integration needs
separate evidence that callbacks were installed, tools ran or were blocked as
intended, and returned effects were adopted by the Agent. Record validation
commands and results in the pull request; keep experiment logs out of the README.
