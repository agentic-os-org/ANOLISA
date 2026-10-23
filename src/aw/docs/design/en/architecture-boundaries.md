# AW architecture boundaries

[中文版](../zh/architecture-boundaries.md)

The internal Adapter library compiles with AW; concrete Providers connect through
a process protocol. This migration aligns source ownership and regression gates
while preserving commands, protocols and Preview installation behavior.

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
│   │   └── aw-package/             # Current Preview build, install and config editing
│   ├── tests/                     # Regression matrix, native inventory and shared checks
│   └── scripts/                   # Unified gate
├── providers/                     # Independent concrete Provider collaboration
│   ├── agent-sec-core/            # Own workspace, lockfile, source and tests
│   └── tokenless/                 # Own workspace, lockfile, source and tests
├── src/agent-sec-core/            # Security engine and public product interfaces
├── src/tokenless/                 # Compression engine and public product interfaces
└── distribution/                  # OS distribution composition
```

`providers/` accepts first-party and community implementations. External
repositories and other implementation languages are also supported. The two
bundled Rust implementations retain the `aw-provider-sec-core` and
`aw-provider-tokenless` package and binary names. AW has neither normal nor
development dependencies on them.

## Adapter and service interfaces

`aw-cli` owns the unified `aw` command. The standalone `aw-package` executable
has been removed; the Rust crate remains.
It directly calls the `aw-package` library for installation and offline editing;
`aw-build` remains a maintenance binary in `aw-package`. `aw-service` provides
the client and service as a library. Libraries do not depend on `aw-cli`.
Both launcher and service use `aw-adapter`, which does not depend on service,
Host or CLI internals.

Software installation uses `aw install --prefix ... --bundle ...`; native Hook
registration uses `aw agent connect --config ... --agent ...`. The old native
`aw install --config ... --agent ...` remains compatible. Mixed installation
and native-registration arguments fail before mutation. Preview still accepts
only its existing local packages; this extraction adds no URL installer, package
manager backend, automatic activation or receipt migration.

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

### This migration and subsequent releases

Independent workspaces, lockfiles and test entrypoints are implemented. Both
Providers still use repository-local AW library paths, and Tokenless composition
tests reference the sec-core Provider. Builds and releases outside the repository
are not yet supported for these implementations.

Preview currently delivers core and Providers as one build set. Installation
therefore checks matching versions and source commits to establish their common
origin. Published SDKs and Provider artifacts will underpin a compatibility matrix
before installation rules change, with migration tests for old receipts. This
migration requires no user configuration or receipt changes.

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
