# Package installation and recovery

[中文版](../../../zh/aw/reference/packages.md)

Follow the [installation guide](../../../../user-guide/en/aw/installation.md) for first use. This reference covers packaging, deployment and automation.

## Install local packages

`aw install` accepts unpacked Preview bundles or independent packages containing `package.json` and `payload/`. `--bundle` must be absolute.
Set `AW_PREFIX` to your absolute installation path, then preview and execute. This example uses user scope.

```bash
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user --dry-run
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user
aw list --installed --prefix "$AW_PREFIX" --json
aw doctor --prefix "$AW_PREFIX" --json
aw uninstall --prefix "$AW_PREFIX" --scope user --yes
```

Root-managed installations use `sudo` and explicit `--scope system`. Keep the same scope and prefix for core, extensions and uninstall.
Preview Providers require core from the same build; ordinary tools can be installed independently. Exact dependencies must exist in the same prefix or arrive in the same Preview composite bundle.
Upgrade existing components into a new prefix; the installer rejects in-place replacement.

Plans list files, dependencies and blockers; blocked plans exit 1. Execution rechecks inputs. Limits are 1 GiB per file, 4 GiB and 32767 owned files per prefix, plus bounded recovery records.
The filesystem must support `O_TMPFILE`, hard links, atomic rename and durable directory synchronization.

## Database and legacy installations

| Scope | Database |
| --- | --- |
| user | `$XDG_STATE_HOME/aw/packages`, defaulting to `$HOME/.local/state/aw/packages` |
| system | `/var/lib/aw/packages` |

`sudo` does not select system scope. Queries with `--prefix` use the database location recorded by that installation.
`installed.json` records packages and file ownership; `transactions/` holds unfinished operations. `<prefix>/.aw-packages/aw-core.json` records the database location.
These records are separate from AW runtime `state_dir`.

Legacy Preview component records reside in `<prefix>/.aw-packages/*.json`. Queries require explicit prefix; first-write migration archives the old JSON.
For `runtime_unknown`, retain the old installation and use a new prefix. Old installers reject the new database locator format.

## Recover failures

For `needs_recovery`, pause package writers and recover with the original scope.

```bash
aw recover --json
sudo aw recover --scope system --json
```

Recovery finishes cleanup after database commit, or restores prior files and records before commit. Conflicting files and backups are retained with reported paths; keep them until resolved.
`doctor` checks owned files and lists failures in JSON `result.report.issues`; it does not repair them.
Uninstall rejects modified, missing, reverse-dependent or AW-held files. Empty registration, locator and lock remain after the final package is removed.

See [CLI automation](cli.md) for reports, [package design](../../../../../src/aw/docs/design/en/packaging/package-management.md) for transactions and lock ordering, and [contributing](../../../../../src/aw/CONTRIBUTING.md) for builds and release acceptance.
