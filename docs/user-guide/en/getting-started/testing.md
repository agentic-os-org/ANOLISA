# Testing ANOLISA

[中文版](../../zh/getting-started/testing.md)

Select the checks for the components you changed. Prepare their toolchains and
build prerequisites with [Building from source](building.md), then run the
component commands below. The aggregate runner covers only some components and
may skip tests when prerequisites are missing.

## 1. Component tests and quality gates

Run the commands from the repository root unless a `cd` is shown. These are
the smallest useful local gates; read the linked README and scoped developer
guide before changing component internals.

| Component | Test and quality gate |
| ----------- | ----------------------- |
| [AW](../../../../contribute/en/aw/building.md) | `python3 src/aw/scripts/check.py`; Linux only, uses the pinned Rust toolchain, Python 3 and Node.js. |
| [os-skills](../../../../src/os-skills/README.md) | No compilation target. Validate changed `SKILL.md` files and run changed scripts with their documented interpreter. |
| [agent-sec-core](../../../../src/agent-sec-core/README.md) | `cd src/agent-sec-core && make test` runs Python, Rust sandbox, and OpenClaw plugin tests. Python uses uv with Python 3.11.6. |
| [agentsight](../../../../src/agentsight/README.md) | Linux: `cd src/agentsight && make lint && make test`; macOS: run the tests relevant to the local viewer and trajectory collector. |
| [tokenless](../../../../src/tokenless/README.md) | `cd src/tokenless && make lint && make test` |
| [agent-memory](../../../../src/agent-memory/README.md) | `cd src/agent-memory && make fmt-check && make lint && make test`; `cd src/agent-memory && make smoke` covers the MCP stdio path. Linux only. |
| [ws-ckpt](../../../../src/ws-ckpt/README.md) | `cd src/ws-ckpt && make test`; install and service checks require Linux system mode. |
| [cosh-ng](../../../../src/cosh-ng/README.md) | `cd src/cosh-ng && cargo fmt --all -- --check`, then select the closest targeted test from its [contribution guide](../../../../src/cosh-ng/CONTRIBUTING.md). Full local gates are reserved for explicitly requested large or cross-cutting validation. |
| [anolisa](../../../../distribution/anolisa/README.md) | `cd distribution/anolisa && cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked` |
| [SkillFS](../../../../src/skillfs/README.md) | `cd src/skillfs && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`; on Linux, `cd src/skillfs && scripts/test.sh` adds the FUSE smoke test. |
| [ktuner](../../../../src/ktuner/README.md) | `cd src/ktuner && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test` |

Agent Memory's `make smoke` requires Python 3.8+ and checks the release binary in
a temporary userland namespace. It validates MCP responses and read-back data,
fails on server errors or the protocol deadline, and removes temporary data after
stopping the child process. It does not validate privileged mount isolation.
Run `python3 src/agent-memory/tests/test_smoke.py -v` to check the smoke runner's
failure detection without building the component.

For Agent Memory, `make smoke SMOKE_TIMEOUT=10` sets the protocol deadline to 10 seconds (default: 30); forced process cleanup has an additional two-second bound. The sandbox check reads a known existing file outside the temporary namespace and must be rejected.

For a public API or rustdoc change, add `cargo doc --workspace --no-deps` to
the affected Rust component's gate. `ktuner tune`,
FUSE mounts, eBPF tracing, and system daemons need host privileges or kernel
features beyond a normal unit-test run.

The [CI workflow](https://github.com/agentic-os-org/ANOLISA/blob/main/.github/workflows/ci.yaml) adds coverage, packaging,
frontend, adapter, and integration checks for selected components. Those jobs
are stricter than the smallest local commands in the matrix, so consult the
workflow when a change touches a generated package or a framework adapter.

## 2. Aggregate tests and pull-request gates

`tests/run-all-tests.sh` is a partial convenience runner. Use `--filter` to
select agent-sec-core, agentsight, tokenless, or agent-memory. It does not test
cosh-ng, os-skills, ws-ckpt, anolisa, SkillFS, or ktuner.

```bash
./tests/run-all-tests.sh --filter sec
./tests/run-all-tests.sh --filter sight
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

The script currently skips work when prerequisites are missing. It skips
agent-sec-core Python tests without `uv`, skips its sandbox e2e test when
`/usr/local/bin/linux-sandbox` is absent, skips AgentSight without `cargo`,
and skips tokenless only when neither `make` nor `cargo` is available. It also
skips agent-memory outside Linux or when `cargo` is unavailable. It still
prints a success line after these skips,
so a zero exit status does not prove that every test ran. The agent-sec-core
e2e invocation also depends on its current working-directory layout. Use the
component Makefiles for a reliable local gate.

For a pull request, select the rows in the component matrix that correspond to
the changed files and run their build, lint, and test commands. Add platform,
integration, smoke, frontend, or documentation checks when the changed files
require them. Keep the aggregate runner as a quick signal rather than the
pull-request acceptance criterion.

## 3. Further documentation

- [Build from source](building.md)
- [Troubleshooting](troubleshooting.md)
- [Contribution guide](../../../../CONTRIBUTING.md)
- [Developer guide index](../../../developer-guide/en/index.md)
