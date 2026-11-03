# Local Provider management

[中文版](../../zh/packaging/package-management.md)

`aw-package` admits local Provider extensions and generates or edits AW
configuration. File verification, inventories and durable transactions live in
its private `store` module. Framework and backend builds use Makefiles and
distribution scripts. AW's Provider API does not build sources or install core/backend software.

## Inputs and admission

Every AW bundle has one bounded `package.json` and a `payload/` tree. Descriptions
record independent IDs, SemVer versions, Linux architecture, exact dependencies,
file SHA-256 values and modes. Every package must declare
`aw-provider/v1alpha1`; `manifest.json`, `aw-core` and ordinary tools are rejected.
Provider IDs and versions need not match AW core. Backends and their services
remain separately installed components.

The public `aw_package::plan::install` entrypoint applies
that admission before returning an opaque `PackagePlan`. It serializes the plan's
read-only decisions and blockers. `aw_package::plan::execute` accepts only this
admitted type. Generic bundle inspection, descriptions and installation entrypoints
remain private implementation details of `aw-package`.

Planning never creates the target prefix, database, locks or journals. It verifies
regular files without following symlinks, exact same-prefix dependencies,
destination collisions and ownership. It does not download URLs, resolve product
names or perform in-place replacement. Destructive plans verify reverse
dependencies and the files selected for removal.

## Database ownership

User extension records live at `$XDG_STATE_HOME/aw/packages`, falling back to
`$HOME/.local/state/aw/packages`. Explicit system scope uses `/var/lib/aw/packages`
and requires root for writes; effective UID does not implicitly select scope.
System inventories and their read locks can be read by ordinary users, while
transaction journals remain private to the writer.

| Database path | Purpose |
| --- | --- |
| `installed.json` | Versioned inventory, commit generation, installation IDs, Provider packages, files and exact dependencies |
| `lock` | Shared query/configuration lock or exclusive transaction lock |
| `transactions/<transaction_id>/` | Durable journal and publication evidence for an interrupted operation |

A prefix locator binds the extension installation to its selected database and
manager without duplicating package facts. AW rejects ANOLISA-owned locators and
does not import native RPM ownership. Explicit-prefix queries and configuration
generation follow the verified locator, including readable system installations.
No-prefix queries use only the selected AW database. File damage is reported by
`doctor`; removing all extensions retains an empty registration for safe reuse.

The AW core binary installed by an ordinary Makefile has no extension record.
Providers installed by RPM or another owner can run through explicit
`transport.argv` without any AW database. Configuration with external paths does
not adopt their file ownership.

## Transactions and recovery

Queries hold existing database read locks. Configuration takes database then
prefix read locks through file verification and private output publication.
Mutation execution takes database then prefix write locks, rechecks the planned
snapshot and uses the private `store` transaction implementation. Private staging
and no-follow identity checks prevent a path replacement from redirecting writes.

Durable replacement of `installed.json` commits the operation. An interrupted
transaction blocks further mutation until explicit `aw recover` completes
committed cleanup or rolls back uncommitted writes. Recovery retains conflicting
bytes and reports blockers instead of overwriting them. The package engine's
file, byte, action and journal limits apply before mutation and again under locks.

## Runtime file references

`aw-exec::PackageLeases` holds existing prefix read locks for known executables
and literal absolute argv paths. The CLI, service and Host keep those leases
through their owned process lifetime and cleanup. This `aw-exec` helper reads
no central inventory and creates no package records. Ordinary or native
executables without a managed locator remain usable.

These references do not discover arbitrary external processes or shell-generated
paths. Package removal therefore checks only the evidence it can establish;
backend lifecycle and native package removal remain their owners' responsibility.
Native Hook registration uses `aw agent connect` and is separate from Provider
bundle installation.

## Verification

AW admission tests cover rejection of non-Provider and manifest inputs, independent
Provider IDs and versions, exact dependencies, stale plans, file ownership and
recovery. The `store` unit tests cover the underlying bounded file transactions.
Provider protocol and native Agent adoption tests remain separate; installing a
bundle does not prove that a framework adopted its effects.
