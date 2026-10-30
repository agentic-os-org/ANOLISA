# AW architecture boundaries

[中文版](../zh/architecture-boundaries.md)

The internal Adapter library compiles with AW; concrete Providers connect through
a process protocol. AW owns runtime integration and Provider configuration;
component owners build their backends, and distribution selects and packages them.

## Directories and ownership

```text
ANOLISA/
├── src/aw/                         # Virtual workspace: ten framework crates
│   ├── Cargo.toml / Cargo.lock
│   ├── crates/
│   │   ├── aw-adapter/
│   │   │   ├── src/                # Dialects, catalog, native validation and replies
│   │   │   └── native/             # Python / JavaScript bridge assets
│   │   ├── aw-contracts/           # src/, schemas/, tests/
│   │   ├── aw-config/              # Configuration and static validation
│   │   ├── aw-provider/            # Common Provider protocol and admission
│   │   ├── aw-exec/                # Bounded process execution
│   │   ├── aw-host/                # Provider preparation and invocation
│   │   ├── aw-core/                # Plan API and Journal
│   │   ├── aw-cli/                 # Unified aw command frontend
│   │   ├── aw-service/             # Client, service and runtime state
│   │   └── aw-package/             # AW extension management and config editing
│   ├── tests/                     # Regression matrix, native inventory and shared checks
│   └── scripts/                   # Unified gate
├── providers/                     # Independent concrete Provider collaboration
│   ├── agent-sec-core/            # Own workspace, lockfile, source and tests
│   └── tokenless/                 # Own workspace, lockfile, source and tests
├── src/agent-sec-core/            # Security engine and public product interfaces
├── src/tokenless/                 # Compression engine and public product interfaces
└── distribution/                  # Product installation and composition
    └── packaging/aw/              # Build/package scripts and concrete Provider templates
```

`providers/` accepts first-party and community implementations. External
repositories and other implementation languages are also supported. The two
bundled Rust implementations retain the `aw-provider-sec-core` and
`aw-provider-tokenless` package and binary names. AW has neither normal nor
development dependencies on them.

## Adapter and service interfaces

`aw-cli` owns the unified `aw` command. The standalone `aw-package` executable
has been removed; the Rust crate remains.
It directly calls `aw-package` for local Provider management and offline
editing. File verification and transactions stay in its private `store` module;
runtime leases belong to `aw-exec`. Product installation remains the distributor
or native manager responsibility. `make -C src/aw build` builds the AW CLI, while
`make -C distribution aw-packages VERSION=0.1.0 COMPONENTS='core sec-core tokenless' OUTPUT="$PWD/target/aw-packages"` invokes ordinary build/package scripts in
`distribution/packaging/aw/`. Independent backend delivery and configuration
defaults belong to distribution. `aw-service` provides
the client and service as a library. Libraries do not depend on `aw-cli`.
Both launcher and service use `aw-adapter`, which does not depend on service,
Host or CLI internals.

Local AW extension installation uses `aw install --prefix ... --bundle ...`; native Hook
registration uses `aw agent connect --config ... --agent ...`. The old native
`aw install --config ... --agent ...` remains compatible. Mixed installation
and native-registration arguments fail before mutation. Local packages use
`package.json` and declare the Provider protocol; `manifest.json` and core/backend
bundles are rejected. This path does not fetch URLs, invoke a native package manager or
activate a Provider. Installed Providers also connect directly through
`transport.argv` without an AW package record; RPM-owned files retain RPM ownership.

| Interface or state | Ownership |
| --- | --- |
| `NativeOptions`, `InstallOptions` | Typed options passed after CLI argument parsing |
| `NativeContext` | Target, instance ID and working directory only, for native normalization |
| `LaunchPlan`, `Readiness` | Adapter returns registration; launcher executes and waits for readiness |
| `LaunchFiles`, cancellation flag | Launcher supplies file access and cancellation, retaining directory and process ownership |
| `Client`, full `Binding`, wire types, `HookBinding` | Remain in service and CLI; Adapter owns no connection or RPC state |
| Native association, completion phases and replies | Adapter interprets native fields and returns comparison data or replies; CLI writes output and handles exit |
| Instances, events, claims, budgets, cancellation, audit | Service retains state and enforces constraints |

Qoder ordering requirements, Hermes observation/transform phases, OpenClaw
post-replacement observation and framework source aliases belong to the Adapter.
The service retains initial snapshots, digest checks and once-only invocation.
Raw native bytes are corroborated through the service-supplied strict JSON
decoder; a callback's `verified: true` is never accepted as evidence.

The runtime path remains `aw-service → aw-host → aw-exec → Provider`. The Adapter
converts results into native replies for the Agent to adopt. The service reuses
`aw-core::FileJournal` without entering the Core plan API. Native permission,
security denial, PreTool fresh-call requirements and PostTool authorized text-slot
rules remain unchanged.

## Schema ownership and evolution

Capability, plan and evidence contracts belong to `aw-contracts`; configuration
to `aw-config`; public Provider messages and Views to `aw-provider`. Native bridge
data belongs to the Adapter, and private Provider data to its implementation.

`aw-contracts` retains 23 registered contracts. Eight unregistered capability v1
drafts with no repository consumers were removed; Git retains their history,
without a separate `references/` directory. Active v1 plan and evidence contracts
remain. Active schema bytes, `$id`, Registry names and digest vectors are unchanged.

New versions use distinct names and IDs alongside supported older versions, with
semantic validation and positive/negative fixtures. The schema inventory test
requires each contract file to be registered, preventing unused runtime schemas.

## Component owners and the OS distributor

`providers/` is a source collaboration space; each component owns its build and
release. The target is for each first-party Provider to build, test and release
outside ANOLISA using only pinned public AW libraries for its AW integration.
Its owner manages third-party and backend dependencies. Providers must not depend
on service internals, another Provider's source or the complete OS source tree.

| Role | Responsibility and deliverables |
| --- | --- |
| AW maintainer | Public SDK, process protocol, compatibility tests and version compatibility boundaries |
| Provider owner | Implementation, contract tests, pinned dependencies, versioned artifacts, backend requirements and installation instructions |
| OS distributor | Version selection, source and checksum verification, composition testing, installation and upgrades |
| Third-party implementer | An executable using the public process protocol, configured through YAML; language and repository are independent choices |

The OS distributor consumes component artifacts without owning their internal
implementation. Bundled backend dependencies enter distribution validation;
externally installed Providers integrate through their public contract. The SDK
is a source interface, while JSON/stdio is the runtime boundary. This does not
promise a stable Rust binary ABI.

### Independent delivery

The Providers own independent workspaces, lockfiles and test entrypoints.
The bundled implementations still use repository-local AW SDK paths, and
Tokenless composition tests reference the sec-core Provider. Out-of-tree builds
for these implementations still require published, pinned SDK packages.

Integration builds select and test framework, Provider and backend versions
together. Provider bundles declare their own IDs and versions; the AW extension
API requires the process protocol, not matching AW core package records.
Core and backend installation remains outside AW's Provider management API.

## Regression boundary

Run `python3 src/aw/scripts/check.py` from the repository root. It checks formatting,
Clippy, locked tests and rustdoc for all three workspaces, then validates their
combined runnable inventory, capability matrix and Python/JavaScript digest vectors.
Missing Provider workspaces, reverse dependencies, missing matrix cases, ignored
required tests and invalid source layout must fail.

Adapter unit tests move with their implementation. Service, CLI and Provider
composition tests retain budget, cancellation, security and compression coverage.
Native bridge assets keep their bytes; only references move. Real SDK/CLI and
Tokenless/RTK acceptance remain separate integration checks; passing the daily
gate does not certify those native boundaries.

See the [architecture overview](architecture.md)
for dependencies and source entrypoints, and [Contributing](../../../CONTRIBUTING.md)
for native acceptance boundaries.
