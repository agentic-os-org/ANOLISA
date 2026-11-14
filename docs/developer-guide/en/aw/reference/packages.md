# Package installation and recovery

[中文版](../../../zh/aw/reference/packages.md)

Follow the [installation guide](../../../../user-guide/en/aw/installation.md) for first use. This reference covers packaging, deployment and automation.

## Install local packages

`aw install` accepts an unpacked local Provider extension containing `package.json` and `payload/`.
The description must declare `provider.protocol: aw-provider/v1alpha1`; `manifest.json`, AW core and ordinary backend packages are rejected. `--bundle` must be absolute.
Set `AW_PREFIX` to your absolute installation path, then preview and execute. This example uses user scope.

```bash
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user --dry-run
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user
aw list --installed --prefix "$AW_PREFIX" --json
aw doctor --prefix "$AW_PREFIX" --json
aw uninstall --prefix "$AW_PREFIX" --scope user --yes
```

Root-managed installations use `sudo` and explicit `--scope system`. Keep the same scope and prefix for Provider installation and removal.
All declared paths, including `bin/` and `share/`, are relative to this single prefix. Exact dependencies must already exist in the same prefix.
Provider IDs and versions are independent of AW core. Upgrade an existing Provider into a new prefix; the installer rejects in-place replacement.

Build AW, Providers and backends through their Makefiles and distribution scripts; RPM installation and ownership stay with the native package manager.
These installations need no AW package record: configure Providers through explicit `transport.argv`. See the [build instructions](../../../../../contribute/en/aw/building.md).

Plans list files, dependencies and blockers; blocked plans exit 1. Human output sends blockers to stderr with empty stdout; JSON retains the complete plan in its existing failure envelope. Execution rechecks inputs. Limits are 1 GiB per file, 4 GiB and 32767 owned files per prefix, plus bounded recovery records.
The filesystem must support `O_TMPFILE`, hard links, atomic rename and durable directory synchronization.

## Database and ownership

| Scope | Database |
| --- | --- |
| user | `$XDG_STATE_HOME/aw/packages`, defaulting to `$HOME/.local/state/aw/packages` |
| system | `/var/lib/aw/packages` |

`sudo` does not select system scope. Queries with `--prefix` use the database location recorded by that installation.
`installed.json` records Provider packages and file ownership; `transactions/` holds unfinished operations.
`<prefix>/.local-packages/installation.json` records the database location and authority; `<prefix>/.local-install.lock` protects files in that prefix.
These records are separate from AW runtime `state_dir`. AW accepts current Provider records without importing historical receipts, ANOLISA registrations or native RPM ownership.

## Recover failures

For `needs_recovery`, pause package writers and recover with the original scope.

```bash
aw recover --json
sudo aw recover --scope system --json
```

Recovery finishes cleanup after database commit, or restores prior files and records before commit. Conflicting files and backups are retained with reported paths; keep them until resolved.
`doctor` checks owned files and lists failures in JSON `result.report.issues`; it does not repair them.
Default uninstall rejects modified, missing, reverse-dependent or AW-held files. Explicit `--keep-changed` removes selected package records and verified unchanged files while leaving changed or missing payload paths untouched. Content, mode, type and missing-path differences can be retained; I/O access errors, verification limits, unsafe prefix/database paths and remaining dependencies still block removal. Unselected package files remain strictly verified.

Preview with `aw uninstall --prefix "$AW_PREFIX" --scope user --keep-changed --dry-run`, then replace `--dry-run` with `--yes` to execute. JSON lists retained relative paths and observed reasons in `result.plan.preserved` for previews and `result.preserved` after success; empty lists are omitted. Human output names these paths as `KEEP PATH` and `KEPT PATH`. These paths never enter deletion or rollback actions, even if their contents change again after planning.

Empty registration, locator and lock remain after the final package is removed. An absent prefix is recreated with these metadata files. Preserved payloads become user-owned and may conflict with reinstall; move them aside or use a new prefix. `aw recover` handles interrupted transactions; use `--keep-changed` for payload drift. See [operations](../../../../user-guide/en/aw/operations.md#upgrade-and-uninstall) for the recovery steps.

See [CLI automation](cli.md) for reports, [package design](../../../../../src/aw/docs/design/en/packaging/package-management.md) for transactions and lock ordering, and [contributing](../../../../../src/aw/CONTRIBUTING.md) for builds and release acceptance.
