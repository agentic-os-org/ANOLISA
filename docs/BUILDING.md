# Building ANOLISA from Source

[中文版](BUILDING_zh.md)

This guide is for contributors working from an ANOLISA checkout. It describes
the repository-wide build entry point, the boundary of the aggregate test
runner, and the local build and test commands for all twelve components. A
component README remains the source for component-specific dependencies and
runtime setup.

## 1. Prepare a checkout

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa
```

The common prerequisites are Git, Bash 4.3 or newer, `make`, a C compiler for
native Rust or Python extensions, and a working network connection for package
downloads.
Platform-specific requirements are listed in the component matrix below. The
repository does not define one global Rust version. Use the `rust-toolchain.toml`
or `rust-version` declared by the component you are changing.

## 2. Repository layout

The `src/` tree currently contains these twelve components:

| Component | Directory | Platform and role |
|-----------|-----------|-------------------|
| copilot-shell (`cosh`) | [`deprecated/copilot-shell`](../deprecated/copilot-shell/README.md) | TypeScript terminal assistant; Linux, macOS, and Windows |
| cosh-ng | [`src/cosh-ng`](../src/cosh-ng/README.md) | Rust Agent OS CLI and shell; full Linux build, limited macOS source build |
| agent-sec-core | [`src/agent-sec-core`](../src/agent-sec-core/README.md) | Rust sandbox plus Python security CLI; Linux |
| agentsight | [`src/agentsight`](../src/agentsight/README.md) | Rust/eBPF observability; full tracing on Linux, `trace` and `serve` on macOS |
| tokenless | [`src/tokenless`](../src/tokenless/README.md) | Rust token and command-output optimization; Linux source build, with cross-compiled npm artifacts for macOS |
| agent-memory (`memory`) | [`src/agent-memory`](../src/agent-memory/README.md) | Rust MCP memory server; Linux |
| os-skills (`skills`) | [`src/os-skills`](../src/os-skills/README.md) | Static skill definitions and scripts; all platforms supported by each skill |
| anolisa | [`distribution/anolisa`](../distribution/anolisa/README.md) | Rust component lifecycle CLI; Linux and macOS arm64 |
| SkillFS (`skillfs`) | [`src/skillfs`](../src/skillfs/README.md) | Rust FUSE skill filesystem; Linux |
| ws-ckpt | [`src/ws-ckpt`](../src/ws-ckpt/README.md) | Rust workspace checkpoint daemon and TypeScript adapters; Linux system service |
| ktuner | [`src/ktuner`](../src/ktuner/README.md) | Rust kernel-tuning engine; Linux |
| blaze | [`src/blaze`](../src/blaze/README.md) | Rust per-host sandbox orchestrator; Linux |

The repository-level instructions are in [`AGENTS.md`](../AGENTS.md). Read a
component's `AGENTS.md` before changing that component and use its README for
architecture and runtime details.

## 3. Toolchains and native dependencies

The build script can install common dependencies on supported Linux
distributions, but it cannot make an unsupported platform build a Linux-only
component.

| Need | Source of truth |
|------|-----------------|
| Node.js | `deprecated/copilot-shell/package.json` requires Node.js `>=20.0.0`; npm is also used by the agent-memory, agentsight, agent-sec-core, tokenless, and ws-ckpt plugin builds. |
| Python and uv | `src/agent-sec-core/agent-sec-cli/pyproject.toml` requires Python `==3.11.6`; use `uv` for that project. Do not replace this with a repository-wide Python minimum. |
| Rust | `src/agent-sec-core/linux-sandbox/rust-toolchain.toml` pins `1.93.0`; `distribution/anolisa/rust-toolchain.toml` pins `1.93.1`; `src/blaze/rust-toolchain.toml` pins `1.88.0`; `src/cosh-ng/rust-toolchain.toml` follows `stable`. Other components use the `rust-version` in their `Cargo.toml` when one is declared. |
| cosh-ng | Linux source builds need `pkg-config` and OpenSSL development files. |
| agent-sec-core | Linux sandbox runtime and integration checks may need bubblewrap, GnuPG, and `jq`. |
| agentsight | Linux eBPF builds need clang, LLVM, libbpf and ELF development headers, kernel headers, and a BTF-enabled kernel. `make build-mac` builds the macOS local viewer without eBPF. |
| tokenless | `just` is used to fetch and patch RTK; npm is needed for the OpenClaw plugin. |
| agent-memory | Linux builds need CMake and libsystemd development headers; npm builds the bundled OpenClaw adapter. |
| SkillFS | FUSE 3 and `/dev/fuse` are needed for the smoke test; ordinary Cargo tests do not mount FUSE. |
| ws-ckpt | Installing the daemon requires Linux systemd and root privileges. Its user-mode Makefile target intentionally does not install the service. |

When a component has a pinned toolchain, run its commands from that component
directory so rustup can select the pin automatically. For an unpinned
component, check its `Cargo.toml` and the installed stable toolchain before
building.

uv-managed Python runtimes default to the official
`astral-sh/python-build-standalone` downloads on GitHub. If that endpoint is
unreachable, set `UV_PYTHON_INSTALL_MIRROR` to the base URL of a compatible
mirror before building:

```bash
export UV_PYTHON_INSTALL_MIRROR="https://your-mirror.example/python-build-standalone"
```

## 4. Unified build script

`scripts/build-all.sh` is a convenience entry point, not a complete monorepo
builder. It currently knows eight components:

- Default six: `cosh`, `skills`, `sec-core`, `tokenless`, `ws-ckpt`, and
  `memory`.
- Optional two: `cosh-ng` and `sight`. Add `--all` or select them with
  `--component`.

The four components outside this script are `anolisa`, `skillfs`, `ktuner`, and
`blaze`; build those with their local commands in section 5.

The default install profile is user mode. Component files install under
`~/.local` and user-scoped Copilot Shell directories without `sudo`. Initial
system dependency installation may still request `sudo`. Use `--system` (or
`--install-mode system`) for system paths; the script stages files and may
invoke `sudo`. `--no-install` builds and stages artifacts without installing
them.

Before installing component files, the script collects and checks the runtime
contract for every selected component. In user mode, missing native runtime
packages are reported together with one package-manager command and the script
exits without installing component files. In system mode, installable native
runtime packages are handled in one transaction, while missing language
runtimes or platform capabilities stop the run before package mutation. A
system install that needs Node.js requires Node.js 20 or newer in the standard
system PATH.

```bash
# Default six components, user install
./scripts/build-all.sh

# Build and stage without installing
./scripts/build-all.sh --no-install

# Use system paths instead of the default user profile
./scripts/build-all.sh --system

# Include cosh-ng and agentsight as well
./scripts/build-all.sh --all

# Select one or more of the eight supported names
./scripts/build-all.sh --component cosh --component sec-core
./scripts/build-all.sh --component cosh-ng --component sight

# Reuse already-installed dependencies
./scripts/build-all.sh --ignore-deps

# Install dependencies only, or print the plan without changing the system
./scripts/build-all.sh --deps-only
./scripts/build-all.sh --dry-run

# Explicit non-interactive mode and help
./scripts/build-all.sh --non-interactive
./scripts/build-all.sh --help
```

`--ignore-deps` skips both dependency setup and runtime dependency
verification. Use it only on a pre-provisioned host; the caller is responsible
for ensuring that installed components have all required runtimes.

Valid `--component` names are `cosh`, `skills`, `sec-core`, `tokenless`,
`ws-ckpt`, `memory`, `cosh-ng`, and `sight`. The script may build a component
in `target/` before installing it, and a component's own install policy still
applies. For example, `ws-ckpt` requires `--system` to install its daemon;
selecting it in the default user profile does not create a user service.

## 5. Component build and test entry points

Run the commands from the repository root unless a `cd` is shown. These are
the smallest useful local gates; read the linked README and scoped developer
guide before changing component internals.

| Component | Build | Test and quality gate |
|-----------|-------|-----------------------|
| [copilot-shell](../deprecated/copilot-shell/README.md) | `cd deprecated/copilot-shell && make deps && make build` | `cd deprecated/copilot-shell && make lint && make test` |
| [os-skills](../src/os-skills/README.md) | `cd src/os-skills && make build` | No compilation target. Validate changed `SKILL.md` files and run changed scripts with their documented interpreter. |
| [agent-sec-core](../src/agent-sec-core/README.md) | `cd src/agent-sec-core && make build-all` | `cd src/agent-sec-core && make test` runs Python, Rust sandbox, and OpenClaw plugin tests. Python uses uv with Python 3.11.6. |
| [agentsight](../src/agentsight/README.md) | Linux: `cd src/agentsight && make build-all`; macOS local viewer: `cd src/agentsight && make build-mac` | Linux: `cd src/agentsight && make lint && make test`; macOS: run the tests relevant to the local viewer and trajectory collector. |
| [tokenless](../src/tokenless/README.md) | `cd src/tokenless && make build` | `cd src/tokenless && make lint && make test` |
| [agent-memory](../src/agent-memory/README.md) | `cd src/agent-memory && make build` | `cd src/agent-memory && make fmt-check && make lint && make test`; `cd src/agent-memory && make smoke` covers the MCP stdio path. Linux only. |
| [ws-ckpt](../src/ws-ckpt/README.md) | `cd src/ws-ckpt && make build` | `cd src/ws-ckpt && make test`; install and service checks require Linux system mode. |
| [cosh-ng](../src/cosh-ng/README.md) | `cd src/cosh-ng && cargo build --workspace` | `cd src/cosh-ng && cargo fmt --all -- --check`, then select the closest targeted test from its [contribution guide](../src/cosh-ng/CONTRIBUTING.md). Full local gates are reserved for explicitly requested large or cross-cutting validation. |
| [anolisa](../distribution/anolisa/README.md) | `cd distribution/anolisa && cargo build --release --locked` | `cd distribution/anolisa && cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked` |
| [SkillFS](../src/skillfs/README.md) | `cd src/skillfs && cargo build --workspace --release` | `cd src/skillfs && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`; on Linux, `cd src/skillfs && scripts/test.sh` adds the FUSE smoke test. |
| [ktuner](../src/ktuner/README.md) | `cd src/ktuner && cargo build --release` | `cd src/ktuner && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test` |
| [blaze](../src/blaze/README.md) | `cd src/blaze && cargo build --workspace --release` | `cd src/blaze && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` |

For a public API or rustdoc change, add `cargo doc --workspace --no-deps` to
the affected Rust component's gate. `ktuner tune`, Blaze Firecracker paths,
FUSE mounts, eBPF tracing, and system daemons need host privileges or kernel
features beyond a normal unit-test run.

The [CI workflow](https://github.com/alibaba/anolisa/blob/main/.github/workflows/ci.yaml) adds coverage, packaging,
frontend, adapter, and integration checks for selected components. Those jobs
are stricter than the smallest local commands in the matrix, so consult the
workflow when a change touches a generated package or a framework adapter.

## 6. Aggregate tests and pull-request gates

`tests/run-all-tests.sh` is a partial convenience runner. With no filter it
invokes exactly five components: copilot-shell, agent-sec-core, agentsight,
tokenless, and agent-memory. It does not test cosh-ng, os-skills, ws-ckpt,
anolisa, SkillFS, ktuner, or blaze.

```bash
./tests/run-all-tests.sh
./tests/run-all-tests.sh --filter shell
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

## 7. Further documentation

- [User installation guide](user-guide/en/installation.md)
- [Developer guide index](developer-guide/en/README.md)
- [Component onboarding specification](../specs/component-onboarding.md)
- [Documentation standard](../specs/documentation-standard.md)

Component-specific build details, generated artifacts, and runtime setup belong
in the component README or its linked developer guide. Keep this page focused
on repository-wide entry points and update it when the component list or script
interfaces change.
