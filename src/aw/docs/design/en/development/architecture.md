# AW framework and Provider development rules

[中文版](../../zh/development/architecture.md)

These are required development rules for the tasks indexed in [AGENTS.md](../../../../AGENTS.md). Read the relevant sections before editing.

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
[Provider contribution guide](../../../../../../providers/README.md).

Each bundled Rust Provider owns its Cargo workspace, lockfile and toolchain.
The AW workspace contains framework packages only; the gate combines independent
test inventories without adding Provider members or dependencies to AW.
Provider owners own dependencies, tests and releases; the OS distributor selects
versions, validates composition and delivers installation and upgrades. Target
out-of-tree Provider builds against pinned public AW libraries. Third parties
need only the public process protocol and YAML, not a Rust SDK or OS checkout.
The bundled implementations still use local SDK paths. Integration builds select
and test framework, Provider and backend versions together.
Independent Provider SDK publication remains later work. Native RPM/DEB delivery and
product installation belong to the distributor or native package manager. Local AW extension changes
use read-only plans and durable recovery through `aw-package`, whose private
`store` module owns file verification, records and transactions. Preserve existing package and binary
names when moving source directories. New Provider directories
do not automatically join the installer: inclusion requires explicit packaging
and lifecycle validation.

The root Cargo manifest is a virtual workspace. Present the ten framework
crates separately from concrete Providers:

| Responsibility | Framework crates | Boundary |
| --- | --- | --- |
| Provider management and configuration | `aw-package` | Admitted local Provider extensions, private file transactions and AW configuration |
| Native dialects | `aw-adapter` | Catalog, registration, callbacks, native phases and effect adoption |
| Unified user entrypoint | `aw-cli` | CLI parsing, presentation, compatibility and command orchestration |
| Local runtime | `aw-service` | Client, bindings, events, deadlines, cancellation and audit |
| Hook execution | `aw-host`, `aw-exec` | Prepare Providers and execute steps; bound process lifetime and cleanup |
| Shared validation | `aw-config`, `aw-provider`, `aw-contracts` | Configuration, Provider protocol/views, schemas and record validation |
| Admission, plan API and journal | `aw-core` | Hook policy admission, independent plan execution API and FileJournal |

The Hook path is Adapter -> service -> Host -> executor -> Provider process.
Results return to the Adapter for native adoption. The service uses
`aw-core::journal::FileJournal`, not `Core::prepare/execute`; do not insert the
Core plan API into this path or equate its `ProviderHost` port with `aw-host::Host`.
Framework crates must not depend on concrete Provider implementations, including
through dev-dependencies. `aw-adapter` must not depend on `aw-service`, `aw-host`
or CLI internals. Keep Client, wire DTOs and full Binding in `aw-service`;
serialized HookBinding, argument parsing, launch signal handling and CLI-owned
resources belong in `aw-cli`. The CLI calls `aw-package` Rust APIs directly for
local AW extension management and offline configuration; libraries must not depend
on the CLI or print its user-facing output. The standalone `aw-package` executable
has been removed; do not reintroduce a second installer CLI. Local extension
installation requires an explicit absolute `--bundle` path. Every input uses
`package.json` and declares `aw-provider/v1alpha1`; `manifest.json`, AW core and
ordinary backend packages are rejected. The private `aw-package::store` engine is not a public software installation API.
Backend source builds,
component composition and concrete Provider defaults belong to distribution's
Makefile and `distribution/packaging/aw/` scripts. `make -C src/aw build` builds only
the AW CLI. Component-root Makefiles include their own packaging/build.mk and
install.mk; install consumes prebuilt files with PREFIX, AWPRJ and DESTDIR, without
backend installation, AW records, Hook registration or service activation. Providers
keep their aw/providers/<id> identity below any optional AWPRJ namespace. No
compiled distribution builder belongs to the framework.
AW package queries and mutations reject foreign-manager locators. Offline
configuration may use an externally managed prefix when no AW packages are
required; templates requiring AW packages reject that prefix. This exception
does not read or adopt the foreign package inventory. Runtime file leases belong
to `aw-exec` and work without reading a package database. Do not duplicate
ownership or automatically transfer files between databases.
Installed Providers connect through explicit `transport.argv` and their process
protocol without an AW package record. RPM-owned Providers and backends stay
under their native manager; local raw Provider registration is optional.
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
[architecture overview](../architecture.md),
[architecture boundaries](../architecture-boundaries.md) and
[design index](../README.md) together in both languages when ownership or call
paths change. Keep schema ownership and evolution in the overview's
[canonical section](../architecture.md#schema-ownership); other pages link to it.
Show runtime flow in the overview; keep compilation dependencies in separate
framework and Provider tables, with test-only dependencies identified.
