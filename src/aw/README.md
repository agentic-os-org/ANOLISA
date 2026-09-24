# AW

[中文版](README_zh.md)

AW provides versioned capability contracts, offline validation and embeddable Core orchestration. `aw-contracts` checks payload shapes and record relationships; `aw-core` executes pinned plans through caller-provided Hosts and journals execution facts. AW has no service process; native Agent control and final tool dispatch remain with the embedding application.

The interfaces are experimental. Tests use synthetic records and do not certify runtime integration.

## Run the checks

Prepare Rust through rustup, Python 3 and Node.js. Rust, rustfmt and Clippy are
pinned in [rust-toolchain.toml](rust-toolchain.toml). Run from the repository root:

```bash
python3 src/aw/scripts/check.py
```

The entry runs CI behavior tests, formatting, Clippy, all locked workspace tests,
the Python/JavaScript digest vectors and rustdoc. Missing tools, empty or fully
ignored contract, plan, Core execution or journal test targets, invalid vectors and command failures return
nonzero. Each command has a timeout and its child process group is cleaned up on
failure or interruption. Logs identify the failing command; individual commands
can be run from `src/aw` for diagnosis.

These checks run as a regular user without an Agent or service login. Cargo
downloads uncached dependencies; schema validation reads only bundled resources.
The runner requires Linux. The library remains portable, but this gate does not
certify other operating systems or minimum supported versions.

[AW CI](../../.github/workflows/aw-ci.yml) runs on branch pushes, pull requests,
merge groups and manual dispatch. It checks the candidate commit, including the
merge result for pull requests. Unrelated changes produce an explicit no-op;
scope errors, unexpected skips and mismatched tested commits fail `AW / required`.
Repository administrators must select that check in branch protection to enforce
it. A cancelled workflow is not a passing gate.

Upstream CI uses the self-hosted `anolisa-k8s-general-ci-x64` runner; fork CI
uses GitHub-hosted Ubuntu 24.04. Both use Python 3.12.3, Node.js 24.15.0 and
the pinned Rust toolchain. Local validation also uses Linux ARM64.

## Core embedding

`aw-core` provides `Core::prepare` and `Core::execute`, trusted Host/Clock/Journal
ports, and a durable Linux `FileJournal`. Preparation checks the complete plan
before any provider call. Execution records each call before dispatch and returns
terminal results only after the journal acknowledges them. Failed or interrupted
events remain reserved; there is no automatic retry or recovery.

See [Core execution and storage](docs/design/core-execution.md) for ownership,
cancellation, failure and embedding contracts. The tests use synthetic Hosts;
this crate does not connect a production Provider or establish native adoption.
The shared check enforces the two-crate dependency boundary and a 700-line Rust
file limit (600-line warning); the existing contract validator remains capped at
711 lines. These checks supplement review, not runtime acceptance.

## Source reference

- [Registered schemas](schemas/) and [synthetic payload examples](tests/fixtures/contracts.json)
- [Public API](src/lib.rs), [record validation](src/validation.rs) and [plan validation](src/orchestration.rs)
- [Encoding tests](tests/canonical.rs), [schema tests](tests/schemas.rs),
  [record tests](tests/contracts.rs) and [plan tests](tests/orchestration.rs)

The Registry includes 21 schema resources. The eight v1 resources in `crates/aw-contracts/schemas/` are reference copies and are not registered. Callers must use matching schema IDs and digests; no automatic version conversion is provided.

Parse incoming bytes with `canonical::parse` before schema validation. Shape checks alone do not validate record relationships or grant authorization. Follow the public API documentation for plan-level checks; callers remain responsible for authenticating evidence and enforcing actions.
