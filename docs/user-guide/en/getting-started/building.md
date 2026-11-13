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

The common prerequisites are Git, Bash 4.4 or newer, `make`, a C compiler for
native Rust dependencies, a system `python3` for build helpers, and a working
network connection for package downloads.
Platform-specific requirements are listed in the component matrix below. The
repository does not define one global Rust version. Use the `rust-toolchain.toml`
or `rust-version` declared by the component you are changing.

## 2. Repository layout

The following table lists component locations in the repository:

| Component | Directory | Platform and role |
|-----------|-----------|-------------------|
| cosh-ng | [`src/cosh-ng`](../../../../src/cosh-ng/README.md) | Rust Agent OS CLI and shell; full Linux build, limited macOS source build |
| agent-sec-core | [`src/agent-sec-core`](../../../../src/agent-sec-core/README.md) | Rust sandbox and V2 security CLI/daemon, with framework adapters; Linux |
| agentsight | [`src/agentsight`](../../../../src/agentsight/README.md) | Rust/eBPF observability; full tracing on Linux, `trace` and `serve` on macOS |
| tokenless | [`src/tokenless`](../../../../src/tokenless/README.md) | Rust token and command-output optimization; Linux and Windows x64 source builds, with cross-compiled npm artifacts for macOS |
| agent-memory (`memory`) | [`src/agent-memory`](../../../../src/agent-memory/README.md) | Rust MCP memory server; Linux |
| os-skills (`skills`) | [`src/os-skills`](../../../../src/os-skills/README.md) | Static skill definitions and scripts; all platforms supported by each skill |
| anolisa | [`distribution/anolisa`](../../../../distribution/anolisa/README.md) | Rust component lifecycle CLI; Linux and macOS arm64 |
| SkillFS (`skillfs`) | [`src/skillfs`](../../../../src/skillfs/README.md) | Rust FUSE skill filesystem; Linux |
| ws-ckpt | [`src/ws-ckpt`](../../../../src/ws-ckpt/README.md) | Rust workspace checkpoint daemon and TypeScript adapters; Linux system service |
| ktuner | [`src/ktuner`](../../../../src/ktuner/README.md) | Rust kernel-tuning engine; Linux |
| AW (`aw`) | [`src/aw`](../../../../src/aw/README.md) | Unified Agent entry point; Linux |
| `aw-provider-sec-core` | [`providers/agent-sec-core`](../../../../providers/agent-sec-core/README.md) | AW bridge to agent-sec-core; Linux |
| `aw-provider-tokenless` | [`providers/tokenless`](../../../../providers/tokenless/README.md) | AW bridge to Tokenless; Linux |

The repository-level instructions are in [`AGENTS.md`](../../../../AGENTS.md). Read a
component's `AGENTS.md` before changing that component and use its README for
architecture and runtime details.

## 3. Toolchains and native dependencies

The build script can install common dependencies on supported Linux
distributions, but it cannot make an unsupported platform build a Linux-only
component.

| Need | Source of truth |
|------|-----------------|
| Node.js | The script uses Node.js 22 or newer and npm for AgentSight's embedded dashboard and for standalone framework plugin builds. |
| Python | System `python3` runs build helpers and installation checks. The default agent-sec-core V2 build does not require uv, a virtual environment or a fixed Python runtime. Python SDK wheels and Python-specific tests have their own requirements. |
| Rust | AW and its two Providers pin `1.97.1`; agent-sec-core's linux-sandbox pins `1.93.0`; cosh-ng follows `stable`. Follow each component's `rust-toolchain.toml` and minimum `rust-version`; a minimum version does not require installing a separate toolchain. |
| cosh-ng | Linux source builds need `pkg-config` and OpenSSL development files. |
| agent-sec-core | Linux sandbox runtime and integration checks may need bubblewrap, GnuPG, and `jq`. |
| agentsight | Linux builds need clang, LLVM, libbpf and ELF development files. Running eBPF tracing requires kernel support including BTF. `make build-mac` builds the macOS local viewer without eBPF. |
| tokenless | `just` is used to fetch and patch RTK; npm is needed for the OpenClaw plugin. |
| agent-memory | Linux builds need CMake and libsystemd development headers; npm builds the bundled OpenClaw adapter. |
| SkillFS | Linux builds need FUSE development files. FUSE 3 and `/dev/fuse` are needed for the smoke test; ordinary Cargo tests do not mount FUSE. |
| ws-ckpt | The daemon uses Linux systemd and btrfs at runtime. File installation uses the system profile; `DESTDIR` can stage the files without starting the service. |

When a component has a pinned toolchain, run its commands from that component
directory so rustup can select the pin automatically. For an unpinned
component, check its `Cargo.toml` and the installed stable toolchain before
building.

### Download sources

When Node.js or rustup needs installation, the script uses official HTTPS
sources by default. Configure a trusted alternative when those sources are
unreachable:

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NODE_DIST_MIRROR` | `https://nodejs.org/dist` | Node distribution base URL; the script appends `latest-v22.x/` and checks the archive against that source's `SHASUMS256.txt`. |
| `RUSTUP_INIT_SCRIPT_URL` | `https://sh.rustup.rs` | URL of the rustup bootstrap script. |

Both overrides require HTTPS URLs without embedded credentials. For example:

```bash
NODE_DIST_MIRROR=https://npmmirror.com/mirrors/node \
  ./scripts/build-all.sh --all --deps-only
```

Rust toolchain downloads still honor `RUSTUP_DIST_SERVER` and
`RUSTUP_UPDATE_ROOT`. Component builds continue to use your npm registry
configuration (including `npm_config_registry`), Cargo configuration and Git
configuration. The earlier automatic global mirror rewrites are removed;
configure these sources explicitly for your environment.

## 4. Unified build script

### Repository Makefile

Prepare the selected components' dependencies and use their supported build
platform. From the repository root, run:

```bash
make list-components
make build COMPONENTS="cosh-ng tokenless"
# Optionally install the build outputs
make install COMPONENTS="cosh-ng tokenless"
```

### Dependency setup and installation script

`scripts/build-all.sh` prepares the selected Linux build dependencies, builds
component deliverables through their Makefiles, and installs their files. With
no component selection, or with `--all`, it selects these backends and AW:

`cosh-ng`, `skillfs`, `ws-ckpt`, `tokenless`, `agentsight`, `agent-sec-core`,
and `aw`.

Each selected backend also includes its matching AW Provider, when one exists
under `providers/<canonical-backend>/` with a `Makefile` and a `Cargo.toml`
whose `[package].name` starts with `aw-provider-`. The Provider name comes from
the manifest rather than the backend name: `agent-sec-core` includes
`aw-provider-sec-core`, and `tokenless` includes `aw-provider-tokenless`.
The checkout currently has nine default targets in total. New matching
Providers join their backends automatically; unselected backends and their
Providers are not added.

When `aw` is selected, including in `--all`, the script builds and stages the
selected components' core files, bundled skills, and matching AW Providers.
It skips the standalone `-plugins` payloads for Tokenless, sec-core, ws-ckpt,
AgentSight, and agent-memory. When the selection includes `cosh-ng`, the script
also delivers the native Cosh extensions for the selected sec-core and Tokenless
backends. These provide Cosh security Hooks and shell input and output
optimization. AgentSight still builds its embedded dashboard. To build a
component with its full standalone adapter bundle, select that component
without `aw`.

```bash
# Prepare dependencies, build the default targets and install system files
./scripts/build-all.sh --all

# Compile all targets without installing component files
./scripts/build-all.sh --all --no-install

# Install the supported user components, reporting skipped system components
./scripts/build-all.sh --all --user --supported-only

# Check the user subset without changing the host
./scripts/build-all.sh --all --user --supported-only --check-deps

# Compile all default targets in user mode without installing their files
./scripts/build-all.sh --all --user --no-install

# Explicitly allow system build dependencies while keeping component files in user paths
./scripts/build-all.sh --component tokenless --user --install-system-deps

# Build Tokenless and its matching AW Provider
./scripts/build-all.sh --component tokenless --no-install

# Build selected backends and their matching AW Providers
./scripts/build-all.sh --component agent-sec-core --component tokenless --no-install

# Build and stage a package root using distribution package paths
./scripts/build-all.sh --all --prefix /usr --destdir "$PWD/target/pkgroot"

# Prepare dependencies only, or check them without installing anything
./scripts/build-all.sh --all --deps-only
./scripts/build-all.sh --all --check-deps

# Reuse a provisioned host, or inspect the planned commands
./scripts/build-all.sh --component tokenless --ignore-deps --no-install
./scripts/build-all.sh --all --dry-run
./scripts/build-all.sh --help
```

The default source installation uses the system profile, `PREFIX=/usr/local` and
`AWPRJ=anolisa`. In system mode, missing system packages and system file
installation may request `sudo`; run the build script as your ordinary user so
user-owned compilation caches remain usable. Component installation copies files and
renders configuration and service files. Start services and connect Agent
adapters separately.

The unified build entry point no longer uninstalls components. Use the
component-specific removal procedure for the installed layout, including any
legacy paths and runtime data.

Repeated `--component` options select and deduplicate targets. The aliases
`sec-core`, `sight`, `skills` and `memory` remain accepted for `agent-sec-core`,
`agentsight`, `os-skills` and `agent-memory`. The last two are optional targets
outside the default selection. Build `anolisa` and `ktuner` with their local
commands in section 5. Matching Provider discovery uses the canonical backend
name, so `--component sec-core` also selects `aw-provider-sec-core`. Explicit
Provider-only selections remain supported and do not select their backend.

Choose the installation profile with `--system` (default) or `--user`, also
accepted as `--install-mode system` or `--install-mode user`. User mode defaults
to `PREFIX="$HOME/.local"`. File installation supports these profiles:

| Available components | File installation profiles |
|------------------------------------|----------------------------|
| `cosh-ng`, `tokenless`, `aw`, `aw-provider-sec-core`, `aw-provider-tokenless` | system and user |
| `skillfs`, `ws-ckpt`, `agentsight`, `agent-sec-core` | system only |

`--all --user` reports all four unsupported components together and exits
before dependency preparation. Add `--supported-only` to report and skip them,
then attach Providers only to the retained backends. The default user subset
currently contains `cosh-ng`, `tokenless`, `aw`, and `aw-provider-tokenless`;
skipping `agent-sec-core` also omits its Provider. An explicitly selected
Provider can still be built and installed separately. This flag requires user
mode and also filters explicit `--component` selections; an empty selection
after filtering is an error. It also works with `--no-install`, `--deps-only`
and `--check-deps` to prepare or inspect the same user subset. Without the
flag, `--all --user --no-install` compiles all default backends, AW and their
matching Providers.

User mode does not install system packages or invoke `sudo` by default. Missing
system build packages are reported together and stop preparation. Provision
those packages separately, or explicitly add `--install-system-deps` to let the
script install them; component files still use the user profile. Once system
prerequisites are present, `--deps-only --user` can prepare user-owned Rust and
Node toolchains and caches. `--check-deps` stays read-only, including when
`--install-system-deps` is present.

`--destdir` requires an absolute package-root path and stages the selected
profile's files without runtime capability checks or service activation. Use
`--prefix /usr` for distribution package paths as shown above. A
Provider selected explicitly supplies the AW bridge without selecting its
backend or granting the backend's required privileges.

Start with an empty `--destdir` for each package. Reusing a package root leaves
previously staged standalone adapters in that root.

Dependency preparation covers the selected build requirements. It keeps
component compilation outputs and caches for subsequent builds, and does not
prepare uv or a fixed Python environment. RTK and ActPlane source preparation
remains part of their component builds. The script invokes AW and Provider
Makefiles directly; the repository Makefile above retains its own component
list.

AW and its Providers support `CARGO_TARGET_DIR` for custom build outputs; the
wrapper resolves relative values from their component directory. Agent-sec-core
uses `BUILD_DIR` to collect installation artifacts; its value is forwarded
unchanged to preserve native relative-path semantics. Each variable is passed
explicitly to build and installation, including through `sudo`, with the same
component directory for both phases. Other backends retain their own output
layouts; these variables are not shared output-root options for the stack.

Before a host installation, declared runtime dependencies are checked
separately. System mode can install missing native runtime packages together;
user mode reports them without installing system packages. Missing language
runtimes or host capabilities are reported before component files are installed.
Building and package staging do not require the build host to run
the selected services. `--ignore-deps` skips both build environment preparation
and runtime checks; use it on a host you have already provisioned.

### Migrate an installed host to AW

A file upgrade preserves existing adapter files and framework registrations.
Before switching a framework to AW, use its actual service account or Agent
user and the same profile, configuration paths and environment as the running
framework. Record which plugins or Hooks are enabled and where they load from.
Back up the active configuration and keep the previous adapter bundles
available. For a registration managed by ANOLISA, use
[`anolisa adapter status`](anolisa-cli.md#adapter) to identify the source. Use
`anolisa adapter disable <component> <framework>` when retiring the whole
registration. For standalone registrations, use the framework's plugin or Hook
settings.

1. Prepare and validate the [AW configuration](../aw/configuration.md). Match
   its enabled Providers and event and tool selectors against the native
   Hooks. Keep the native Hooks for capabilities that AW has yet to cover for
   this profile. For Cosh, keep the sec-core and Tokenless extensions and
   confirm their sources with [`/extensions list`](../cosh-ng/core/extensions.md).
   Keep [the ws-ckpt plugin](../ws-ckpt/index.md#plugin-installation) on
   OpenClaw or Hermes if you rely on its automatic checkpoints or native
   tools. If you uninstall that plugin, its uninstall script also removes
   the framework's local copy of the Skill. Restore that Skill through the
   framework's Skill directory if you still use it.
2. Stop the old Agent process or Gateway and disable the overlapping native
   registrations before launching it through AW. For example, on the
   [OpenClaw profile used by AW](../aw/configuration.md#openclaw), inspect
   `agent-sec` and `tokenless` with `openclaw plugins inspect <id> --json`.
   If AW now scans `exec` and you accept retiring the native scanner's plugin
   self-protection, set `plugins.entries.agent-sec.config.capabilities.scan-code.enabled`
   to `false` in the selected native JSON. Update any `CODE_SCANNER_HOOK_ENABLED`
   override in the service environment as well. This keeps the other
   `agent-sec` capabilities available. Set
   `plugins.entries.tokenless.enabled` to `false` if the AW Tokenless steps cover
   what the profile uses. Keep the native Tokenless plugin if you need its
   recovery tool and leave the overlapping AW steps disabled for that profile.
3. Start a fresh session or Gateway through AW. Run the
   [security checks](../aw/security.md#verify-checks) and
   [Tokenless checks](../aw/tokenless.md#check-results) for each capability
   you switched. Confirm in the AW records and framework logs that a test
   call invokes the intended check or compression once. Start the required
   services and run the relevant core commands from that same environment,
   such as `agent-sec-cli skillsec status`, `tokenless --version`, or
   `ws-ckpt status`.
   Confirm that the Agent can discover and use the Skills it needs. In
   [Cosh](../cosh-ng/core/skills.md), use `/skills detail ws-ckpt` if the
   checkpoint Skill is installed.

To roll back, exit the AW-managed Agent and stop its AW service. Restore the
saved native configuration and re-enable any registrations you disabled,
using their original installer or framework settings. Start the native Agent
or Gateway with the original profile and repeat the plugin, CLI and Skill checks.

## 5. Component build entry points

Run the commands from the repository root unless a `cd` is shown. Read the
linked README and scoped developer guide for component-specific build details.

| Component | Build |
| ----------- | ------- |
| [AW](../../../../contribute/en/aw/building.md) | `make -C src/aw build` |
| [AW sec-core Provider](../../../../providers/agent-sec-core/README.md) | `make -C providers/agent-sec-core build` |
| [AW Tokenless Provider](../../../../providers/tokenless/README.md) | `make -C providers/tokenless build` |
| [os-skills](../../../../src/os-skills/README.md) | `cd src/os-skills && make build` |
| [agent-sec-core](../../../../src/agent-sec-core/README.md) | `make -C src/agent-sec-core build` |
| [agentsight](../../../../src/agentsight/README.md) | Linux: `make -C src/agentsight build`; macOS local viewer: `cd src/agentsight && make build-mac` |
| [tokenless](../../../../src/tokenless/README.md) | Linux: `cd src/tokenless && make build`; [Windows x64](../../../developer-guide/en/tokenless/windows.md#build-and-test-on-windows) |
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
- [Developer guide index](../../../developer-guide/en/index.md)
- [Component onboarding specification](../../../../specs/component-onboarding.md)
- [Documentation standard](../../../../specs/documentation-standard.md)
- [Commit message standard](../../../../specs/commit-message-standard.md)

Component-specific build details, generated artifacts, and runtime setup belong
in the component README or its linked developer guide. Keep this page focused
on repository-wide entry points and update it when the component list or script
interfaces change.

## Select installed file groups

Where provided, use `install-core` when you need only the backend programs. Add
the component's `install-plugins` or `install-skills` target when you need framework integrations
or skills:

```bash
make -C src/tokenless build
make -C src/tokenless install-core
```

When using AW's Tokenless Provider, install Tokenless core and the separate
Provider; native Tokenless plugins are optional.
