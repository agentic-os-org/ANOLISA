# Building ANOLISA from Source

[中文版](../../zh/getting-started/building.md)

This guide explains how to build and install ANOLISA components from a source
checkout. It covers the shared build script and each component's build entry
point. A component README remains the source for component-specific dependencies
and runtime setup. For validation commands, see [Testing](testing.md).

## 1. Prepare a checkout

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa
```

The common prerequisites are Git, Bash 4.3 or newer, `make`, a C compiler for
native Rust or Python extensions, and a working network connection for package
downloads.
Platform-specific requirements are listed in the component matrix below. The
repository does not define one global Rust version. Use the `rust-toolchain.toml`
or `rust-version` declared by the component you are changing.

## 2. Repository layout

The following table lists component locations in the repository:

| Component | Directory | Platform and role |
|-----------|-----------|-------------------|
| cosh-ng | [`src/cosh-ng`](../../../../src/cosh-ng/README.md) | Rust Agent OS CLI and shell; full Linux build, limited macOS source build |
| agent-sec-core | [`src/agent-sec-core`](../../../../src/agent-sec-core/README.md) | Rust sandbox plus Python security CLI; Linux |
| agentsight | [`src/agentsight`](../../../../src/agentsight/README.md) | Rust/eBPF observability; full tracing on Linux, `trace` and `serve` on macOS |
| tokenless | [`src/tokenless`](../../../../src/tokenless/README.md) | Rust token and command-output optimization; Linux source build, with cross-compiled npm artifacts for macOS |
| agent-memory (`memory`) | [`src/agent-memory`](../../../../src/agent-memory/README.md) | Rust MCP memory server; Linux |
| os-skills (`skills`) | [`src/os-skills`](../../../../src/os-skills/README.md) | Static skill definitions and scripts; all platforms supported by each skill |
| anolisa | [`distribution/anolisa`](../../../../distribution/anolisa/README.md) | Rust component lifecycle CLI; Linux and macOS arm64 |
| SkillFS (`skillfs`) | [`src/skillfs`](../../../../src/skillfs/README.md) | Rust FUSE skill filesystem; Linux |
| ws-ckpt | [`src/ws-ckpt`](../../../../src/ws-ckpt/README.md) | Rust workspace checkpoint daemon and TypeScript adapters; Linux system service |
| ktuner | [`src/ktuner`](../../../../src/ktuner/README.md) | Rust kernel-tuning engine; Linux |

The repository-level instructions are in [`AGENTS.md`](../../../../AGENTS.md). Read a
component's `AGENTS.md` before changing that component and use its README for
architecture and runtime details.

## 3. Toolchains and native dependencies

The build script can install common dependencies on supported Linux
distributions, but it cannot make an unsupported platform build a Linux-only
component.

| Need | Source of truth |
|------|-----------------|
| Node.js | npm is used by the agent-memory, agentsight, agent-sec-core, tokenless, and ws-ckpt plugin builds. Follow each component's `package.json` and build instructions for its Node.js requirements. |
| Python and uv | `src/agent-sec-core/agent-sec-cli/pyproject.toml` requires Python `==3.11.6`; use `uv` for that project. Do not replace this with a repository-wide Python minimum. |
| Rust | `src/agent-sec-core/linux-sandbox/rust-toolchain.toml` pins `1.93.0`; `distribution/anolisa/rust-toolchain.toml` pins `1.93.1`; `src/cosh-ng/rust-toolchain.toml` follows `stable`. Other components use the `rust-version` in their `Cargo.toml` when one is declared. |
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

`scripts/build-all.sh` is a Linux convenience entry point, not a complete
monorepo builder. Select the maintained components you need with repeated
`--component` options: `skills`, `sec-core`, `tokenless`, `ws-ckpt`, `memory`,
`cosh-ng`, and `sight`.

Build `anolisa`, `skillfs`, and `ktuner` with their local commands in
section 5; they are not handled by this script.

The default install profile is user mode. Each component's install rules select
paths within the user's home directory. Initial system dependency installation
may still request `sudo`. Use `--system` (or
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
# Build and install selected components
./scripts/build-all.sh --component sec-core --component tokenless

# Build and stage without installing component files
./scripts/build-all.sh --component sec-core --no-install

# Use system paths instead of the default user profile
./scripts/build-all.sh --component ws-ckpt --system

# Select cosh-ng and agentsight
./scripts/build-all.sh --component cosh-ng --component sight

# Reuse already-installed dependencies
./scripts/build-all.sh --component sec-core --ignore-deps

# Install dependencies only, or print the plan without changing the system
./scripts/build-all.sh --component sec-core --deps-only
./scripts/build-all.sh --component sec-core --dry-run

# Explicit non-interactive mode and help
./scripts/build-all.sh --component sec-core --non-interactive
./scripts/build-all.sh --help
```

`--ignore-deps` skips both dependency setup and runtime dependency
verification. Use it only on a pre-provisioned host; the caller is responsible
for ensuring that installed components have all required runtimes.

The script may build a component in `target/` before installing it, and a
component's own install policy still applies. For example, `ws-ckpt` requires
`--system` to install its daemon;
selecting it in the default user profile does not create a user service.

## 5. Component build entry points

Run the commands from the repository root unless a `cd` is shown. Read the
linked README and scoped developer guide for component-specific build details.

| Component | Build |
| ----------- | ------- |
| [AW](../../../developer-guide/en/aw/building.md) | `cd src/aw && cargo build --locked -p aw-cli -p aw-service -p aw-package -p aw-provider -p aw-host --bins --examples` |
| [os-skills](../../../../src/os-skills/README.md) | `cd src/os-skills && make build` |
| [agent-sec-core](../../../../src/agent-sec-core/README.md) | `cd src/agent-sec-core && make build-all` |
| [agentsight](../../../../src/agentsight/README.md) | Linux: `cd src/agentsight && make build-all`; macOS local viewer: `cd src/agentsight && make build-mac` |
| [tokenless](../../../../src/tokenless/README.md) | `cd src/tokenless && make build` |
| [agent-memory](../../../../src/agent-memory/README.md) | `cd src/agent-memory && make build` |
| [ws-ckpt](../../../../src/ws-ckpt/README.md) | `cd src/ws-ckpt && make build` |
| [cosh-ng](../../../../src/cosh-ng/README.md) | `cd src/cosh-ng && cargo build --workspace` |
| [anolisa](../../../../distribution/anolisa/README.md) | `cd distribution/anolisa && cargo build --release --locked` |
| [SkillFS](../../../../src/skillfs/README.md) | `cd src/skillfs && cargo build --workspace --release` |
| [ktuner](../../../../src/ktuner/README.md) | `cd src/ktuner && cargo build --release` |

After building, use the component gates in [Testing](testing.md) to validate
your changes.

## 6. Further documentation

- [Testing](testing.md)
- [User installation guide](installation.md)
- [Developer guide index](../../../developer-guide/en/README.md)
- [Component onboarding specification](../../../../specs/component-onboarding.md)
- [Documentation standard](../../../../specs/documentation-standard.md)
- [Commit message standard](../../../../specs/commit-message-standard.md)

Component-specific build details, generated artifacts, and runtime setup belong
in the component README or its linked developer guide. Keep this page focused
on repository-wide entry points and update it when the component list or script
interfaces change.
