# AW development contract

Read the repository AGENTS.md and [CONTRIBUTING.md](CONTRIBUTING.md) first.
AW is a Linux component. The checkout directory name does not select another
component's skills or validation workflow.

## Integrated user experience

The preferred entrypoint is `aw run --config aw.yaml --agent <target>`.
One configuration composes native Agent targets, security, Tokenless, optional
observation and user Hooks. Keep validation, launch, inspection and cleanup
coherent with that entrypoint. New integrations should extend these contracts
instead of adding a parallel launcher or requiring users to assemble internal
service RPCs and callback commands.

Frameworks retain their model configuration, UI, scheduling, permissions and
session lifecycle. AW owns configuration admission, bounded Provider invocation,
event correlation and metadata audit. An Adapter translates native callbacks
and adopts supported effects; a Provider implements policy through its protocol.
Do not put framework-specific code into the service, Core or Provider Host.
Optional panel views must not participate in tool admission or execution.

Security denial and native permission denial retain precedence. Tokenless
PreTool suggestions block the original call and require a fresh call through
security and native authorization. They must not rewrite arguments in place or
emit approval. PostTool compression changes only an authorized text slot at the
final replacement step; preserve the native envelope, failure status and audit.
Keep unsupported capabilities visible at admission. Configuration vocabulary
and roadmap entries are not evidence of native support.

## Provider collaboration and framework architecture

**Concrete Providers MUST live in the independent collaboration directory
at the repository root, `providers/<provider>/`. Reserve `src/aw/crates/` for the reusable AW framework.**
Backend-specific security, compression and other integrations belong in this
Provider space, not in new `src/aw/crates/aw-provider-<backend>/` directories or framework
modules. The `aw-provider` crate is the shared protocol library, not an integration.

`providers/` accepts first-party and community implementations, each with its own
source, schemas, examples and tests. Providers may also live in independent
repositories, use other languages and ship separately. Runtime integration uses
`aw-provider/v1alpha1` over stdio and the configured `transport.argv`; it must not
require a Provider to join this source tree. Follow the
[Provider contribution guide](../../providers/README.md).

Each bundled Rust Provider owns its Cargo workspace, lockfile and toolchain.
The AW workspace contains framework packages only; the gate combines independent
test inventories without adding Provider members or dependencies to AW.
Provider owners own dependencies, tests and releases; the OS distributor selects
versions, validates composition and delivers installation and upgrades. Target
out-of-tree Provider builds against pinned public AW libraries. Third parties
need only the public process protocol and YAML, not a Rust SDK or OS checkout.
The bundled implementations still use local SDK paths. Preview delivers one
build set and checks matching versions and source commits for common origin.
Independent SDK publication and native RPM/DEB delivery remain later work.
Local package changes use the central database, read-only plans and durable
recovery in `aw-package`; the Linux gate builds a pinned historical installer
to verify record migration. Preserve existing package and binary
names when moving source directories. New Provider directories
do not automatically join the installer: inclusion requires explicit packaging
and lifecycle validation.

The root Cargo manifest is a virtual workspace. Present the ten framework
crates separately from concrete Providers:

| Responsibility | Framework crates | Boundary |
| --- | --- | --- |
| Build and install | `aw-package` | Package composition, installation and configuration generation |
| Native dialects | `aw-adapter` | Catalog, registration, callbacks, native phases and effect adoption |
| Unified user entrypoint | `aw-cli` | CLI parsing, presentation, compatibility and command orchestration |
| Local runtime | `aw-service` | Client, bindings, events, deadlines, cancellation and audit |
| Hook execution | `aw-host`, `aw-exec` | Prepare Providers and execute steps; bound process lifetime and cleanup |
| Shared validation | `aw-config`, `aw-provider`, `aw-contracts` | Configuration, Provider protocol/admission, schemas and record validation |
| Plan API and journal | `aw-core` | Independent plan execution API and FileJournal |

The Hook path is Adapter -> service -> Host -> executor -> Provider process.
Results return to the Adapter for native adoption. The service uses
`aw-core::journal::FileJournal`, not `Core::prepare/execute`; do not insert the
Core plan API into this path or equate its `ProviderHost` port with `aw-host::Host`.
Framework crates must not depend on concrete Provider implementations, including
through dev-dependencies. `aw-adapter` must not depend on `aw-service`, `aw-host`
or CLI internals. Keep Client, wire DTOs and full Binding in `aw-service`;
serialized HookBinding, argument parsing, launch signal handling and CLI-owned
resources belong in `aw-cli`. The CLI calls `aw-package` Rust APIs directly for
software installation and offline configuration; libraries must not depend on
the CLI or print its user-facing output. The standalone `aw-package` executable
has been removed; do not reintroduce a second installer CLI. Bundles carry `aw`
at their root for bootstrap, and software installation requires an explicit
absolute `--bundle` path. `aw-build` remains in the `aw-package` Rust crate.
Preserve legacy native `aw install --config ... --agent ...` semantics while
offering `aw agent connect`; reject mixed package/native options before writes.
Pass only NativeContext and typed options into the Adapter. The caller supplies
cancellation and launch-file access; the Adapter returns plans, comparison data
and native replies. Raw native verification must retain strict decoding and
cannot trust a callback's self-reported verification state.

Keep crate-local source, schemas and Rust tests together. Every contract schema
in `crates/aw-contracts/schemas/` must be registered and tested. Retain supported
versions together; remove unused drafts and use Git history for reference. Do not
remove a contract merely because its version is older. New Core capability
profiles require explicit schemas, Registry entries and cross-record validation;
Provider-private data belongs to its Provider. Schema moves must preserve active
bytes, IDs, registered names and digest vectors. Update the
[architecture overview](../../docs/developer-guide/en/aw/architecture.md) and
[design index](docs/design/en/README.md) when ownership or call paths change.
Show runtime flow in the overview; keep compilation dependencies in separate
framework and Provider tables, with test-only dependencies identified.

## Adding an Agent or Hook

Update these together in the same change:

1. Register the Adapter and its event/effect profile in
   `crates/aw-adapter/src/adapter/catalog.rs`; use that profile when building
   launch capabilities. Keep the configuration schema aligned.
2. Implement native normalization, registration, effect adoption and teardown in
   the Adapter. Keep process ownership in `aw-exec` and common lifecycle in the
   launcher. Reuse the existing admission and callback helpers.
3. Add exact runnable case references to `tests/regression-matrix.json` for every
   event/effect and for lifecycle, security and Tokenless composition. Extend
   `crates/aw-cli/tests/regression/native.rs` for new native payloads. The
   shared PreTool and PostTool tests iterate the matrix without a fallback
   dialect, so an unimplemented fixture fails.
4. Run `python3 scripts/check_matrix.py --write-readmes` from this directory and
   document native adoption limits in both READMEs. Maintain the roadmap
   separately from supported capabilities.
5. Add pinned native commands, required reports, an owner and the exact acceptance
   boundary to `tests/native-matrix.json`. Unit suites declare counts; custom
   suites declare JSON result files with exact case names or observed counts.
   Both execution and artifact aggregation verify those reports. Every registered
   Agent must be accounted for; new integrations cannot silently opt out of nightly.
6. Run `python3 scripts/check.py`. The gate compares the compiled catalog with
   the matrix and schema, resolves case names against real libtest executables,
   rejects missing or ignored cases and checks both README grids.

Use deterministic service/CLI/Provider fixtures on every relevant push. Keep
real SDK/CLI adoption checks explicit and pinned; describe which native seam
was exercised. A callback transport test does not certify native adoption.
Do not substitute ignored, opt-in or credential-dependent tests for a required
matrix case. Package acceptance separately covers real Tokenless/RTK binaries.

## Layout and CI cost

Keep workspace/native acceptance under `tests/` and runtime bridge assets under
`crates/aw-adapter/native/`. When changing crate boundaries, update `scripts/check_layout.py`
and its behavior tests alongside the architecture documentation.

Rust, Python and JavaScript (`.js`, `.mjs`, `.cjs`) files share a 699-line ceiling,
including tests and embedded fixtures; the gate reports files at 600 lines.
The existing 711-line `crates/aw-contracts/src/validation.rs` allowance is fixed, not a precedent.
Split by responsibility before crossing the ceiling. Keep related helpers in
named sibling modules, preserve test entrypoints and assertions, and avoid
compressing code or adding size exemptions. Never add `mod.rs` except the
repository-approved `tests/common/mod.rs` convention.

Only PRs changing `src/aw/**` or `providers/**` automatically run AW CI. Component internals belong
in component CI; explicitly validate public contract changes consumed by AW.
Keep daily checks credential-free. The nightly integration workflow builds all
Preview packages once and reuses checksummed candidate-SHA binaries for pinned
native acceptance. Preserve exact version evidence, bounded processes, complete
reports and visible manual boundaries. Missing or skipped suites must fail the
integration result. Prepare private native tool environments; do not weaken
admission to accommodate runner loader overrides or writable shared caches.
Rerun on the exact release SHA before release.

CI checks belong to the existing `AW / required` workflow and local entrypoint.
Scope selection must use the complete change and preserve candidate/tested SHA
identity. Compile test harnesses once, enumerate them directly and reuse the
build for the workspace run. Do not save CI time by skipping shared security,
Tokenless or lifecycle tests based only on the changed Adapter. Gate changes
need negative behavior tests for omissions, ignored tests and failure paths.

Keep source files, protocol schemas and test fixtures separate from generated
artifacts. Store build/test output under `target/`, bound process lifetimes and
clean up owned child processes, profiles, sockets and temporary state.
