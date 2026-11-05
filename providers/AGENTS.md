# Provider collaboration

Read the repository AGENTS.md and [AW development contract](../src/aw/AGENTS.md).

Concrete integrations belong in repository-root `providers/<provider>/`, with
source, private schemas, examples, tests and paired READMEs owned together.
First-party and community implementations use the same contribution boundary;
external repositories and non-Rust implementations remain supported through stdio.

Each Rust Provider owns its Cargo workspace, lockfile and toolchain. Do not add
concrete Providers to the AW workspace or framework dependencies, including test
dependencies. Shared protocol and execution code remains in `src/aw/crates/`.
Preserve existing package/binary names and `transport.argv` behavior when moving
source. Do not copy shared library sources or embed backend code in the framework.

Provider owners own implementation, dependencies, tests and release artifacts.
The OS distributor selects versions, verifies origin and compatibility, and owns
composition, installation and upgrades. Target out-of-tree builds and releases
against pinned public AW libraries; do not couple Providers to service internals,
other Provider sources or the full OS checkout. Third parties can use the public
process protocol and YAML without Rust libraries or repository membership.

The bundled Providers currently use repository-local SDK paths; Tokenless tests
also use the sec-core bridge. Integration builds test selected framework,
Provider and backend versions together. AW admits only protocol-declared
Provider `package.json` bundles, with independent IDs and versions; core and
backend software are delivered outside that API. Native installations retain
native ownership and use explicit `transport.argv` without AW package records.

Run `python3 src/aw/scripts/check.py` from the repository root. The gate audits
all three workspaces and their combined runnable regression matrix. Keep Provider
paths in AW CI scope and nightly package validation, and retain negative tests for
missing workspaces, omitted suites and invalid source layout. See the
[contribution guide](README.md) for protocol and distribution entrypoints.
