# anolisa CLI

The `anolisa` CLI is the unified lifecycle entry point for ANOLISA components.
It resolves component sources, keeps scoped installation records, delegates RPM
transactions to the native package manager, and diagnoses or repairs drift.

---

## Installation

### Option A: Install script (recommended)

```bash
curl -fsSL https://get.agentic-os.sh | bash
```

### Option B: YUM (Alinux)

```bash
sudo yum install anolisa
```

Verify installation:

```bash
anolisa --version
```

---

## Scope And Visibility

`--install-mode user` writes under the current user's roots, while
`--install-mode system` writes system state and normally requires root. When
the option is omitted, root defaults to system mode and a regular user defaults
to user mode.

Read-only commands use a user-plus-system view. A regular user can therefore
see and diagnose a system installation, and adapter discovery can use its
published contract. Mutating commands still write only the explicitly selected
scope. In particular, a user installation may coexist with a system
installation of the same component.

---

## Commands

### install

Install one component through the configured raw or RPM backend, or plan every
component in the index:

```bash
anolisa --install-mode user install <component>
sudo anolisa --install-mode system install <component>
anolisa install --all
```

An installation in the other scope does not make the selected scope
"already installed." Reinstalling or changing an existing record is handled
by lifecycle planning rather than silently overwriting it.

RPM installs, including `--dry-run` and merged `--all` plans, ask DNF to
resolve dependencies and package conflicts before creating recovery journals.
The check uses the same repositories and version pins as the install. It may
refresh repository metadata but does not download packages or change rpmdb.
DNF 4 requires root for this check:

```bash
sudo anolisa --install-mode system --dry-run install cosh --backend rpm
```

A conflict fails the command without leaving a pending operation. Resolve the
reported conflict before retrying. A successful check does not guarantee that
a later download, scriptlet, or transaction will succeed.

### uninstall

Remove one installation from the selected scope:

```bash
anolisa uninstall <component>
anolisa uninstall <component> --purge
sudo anolisa --install-mode system uninstall <component> --remove-system-package
```

ANOLISA-owned files and managed RPM packages are removed by their owning
backend. Adopted or observed system RPMs are left installed by default; use
`--remove-system-package` only when native package removal is intended.

### update

Update one component, every recorded component, the CLI binary, or run the
read-only RPM update report:

```bash
anolisa update <component>
anolisa update all
anolisa update self
anolisa update --check
```

`update all` does not update the CLI binary. Delegated members are merged into
one native transaction where possible; each component keeps its own recovery
journal and record.

### list and status

Inspect the effective user-plus-system view:

```bash
anolisa list
anolisa list --installed
anolisa status
anolisa status <component>
```

In a user view with records in both scopes, the user record is active and the
system record remains visible as shadowed state. A system-mode view reads only
the system root; it does not enumerate other users' state.

Untracked RPM observations use the same package resolver as installation.
An index mapping takes precedence over historical package aliases and
capabilities: an old cosh-ng RPM providing `anolisa-component(cosh)` is shown
under cosh-ng, while cosh still resolves to copilot-shell. `action=install`
identifies a lifecycle action; it is not a native dependency check. Use the
RPM install dry-run to check whether the packages can coexist.

### doctor

Run read-only health, dependency, service, state, and recovery-journal checks:

```bash
anolisa doctor
anolisa doctor <component>
anolisa --dry-run doctor <component>
```

`doctor` scans every root in the current visibility view: user mode includes
the user root and a readable system root, while system mode includes only the
system root. It qualifies system repair suggestions with
`sudo anolisa --install-mode system` when the current invocation cannot mutate
that root. `--fix` is reserved in this release; follow the reported `fix_plan`
explicitly.

For raw installations, `status` and `doctor` allow content edits to files
declared as `type = "config"`. Missing files, unsafe paths, unexpected symlinks,
and permission or capability drift still fail checks; ordinary data and
executable files still undergo SHA-256 verification. Older installation records
recover unambiguous config declarations from their saved component manifest in
memory. If overlapping directory declarations mix config and immutable kinds,
legacy files retain digest checking because their source mapping is unknown.
Back up edited configs and reinstall the component to record accurate kinds.
Diagnostics do not rewrite state.

### restart

Restart services recorded for an installation in the selected scope:

```bash
anolisa --dry-run --install-mode user restart <component>
anolisa --install-mode user restart <component>
anolisa --dry-run --install-mode system restart <component>
sudo anolisa --install-mode system restart <component>
```

`--dry-run` lists the units that would restart and does not run
`systemctl daemon-reload` or `systemctl restart`. System-mode preview
reads recorded state without taking the exclusive install lock, so it
does not need write access to the state root.

### upgrade

Plan or apply the system/RPM image upgrade. Raw-managed components are reported
as skipped rather than migrated to another backend:

```bash
anolisa --install-mode system --dry-run upgrade
sudo anolisa --install-mode system upgrade
sudo anolisa --install-mode system upgrade --target <profile>
```

### adopt, repair, and forget

Manage state without confusing package ownership:

```bash
sudo anolisa --install-mode system adopt <component>
sudo anolisa --install-mode system repair <component>
anolisa --install-mode user forget <component>
sudo anolisa --install-mode system forget <component>
```

`adopt` records an existing system RPM as delegated-adopted without claiming
native removal authority. `repair` reconciles a scoped record with rpmdb or an
interrupted journal. `forget` removes only the record in the selected scope and
never performs package or owned-file removal; a user-scoped forget cannot
delete a visible system record.

If an operation is pending, `forget` directs you to `repair`, even when an
install failed before creating an installation record. Repair reconciles the
journal with rpmdb; forgetting must not discard evidence of a transaction
that may already have changed the system.

### adapter

Manage component adapters:

```bash
anolisa adapter scan
anolisa adapter enable <component> [framework]
anolisa adapter disable <component> [framework]
anolisa adapter status [component]
```

For OpenClaw plugins, executing `adapter enable` accepts the plugin's declared
capabilities. ANOLISA adds `--accept-capabilities` only when the installer's
help advertises it, including in the dry-run plan. Capability consent does
not authorize `--allow-unsafe-plugin-install`; a consent rejection is reported
separately from a plugin-safety rejection.

### logs and bug reports

Inspect component logs or generate a diagnostic bundle:

```bash
anolisa logs <component>
anolisa logs <component> --limit 50
anolisa logs <component> --severity warn
anolisa bug
```

`--level` is an alias for `--severity`.

With `--component cosh-ng`, `anolisa bug` also asks the installed
`cosh-shell` binary to export its sanitized diagnostic bundle to a fresh
private path (`0600`, never overwritten) and summarizes the bundle's health
finding IDs and manifest in the report. The binary is resolved from the
cosh-ng installation's private libexec locations (the raw contract's
directory and the RPM's `/usr/libexec` path), with `COSH_SHELL_BIN` as an
override and PATH as a development fallback; the printed manual and
reproduction commands use the resolved absolute path so they stay runnable.
The bundle is written below the calling user's own state root —
even when diagnosing a system-scope installation — and is never uploaded;
review it locally before attaching it to an issue. If no bundle can be
produced, the report says so explicitly and prints the manual
`cosh-shell diagnostics export` command instead.

---

## Recovery Behavior

Install, uninstall, update, adopt, and repair write recovery intent in the
selected state root before their lifecycle side effects. Native package
operations are forward-only: if dnf may have committed but the ANOLISA record
did not, the journal remains pending and `anolisa repair <component>`
re-observes rpmdb. Owned-file operations keep verified backups and compensate
in reverse order on failure. `forget` is an atomic record-only state update; it
does not perform package/file side effects or create a recovery journal.

`upgrade` remains a compatibility orchestrator rather than a planner/journal
consumer. It refuses existing pending recovery and re-observes rpmdb after a
transaction failure, but it does not create a per-component recovery journal.
After an interrupted `upgrade`, run `anolisa doctor` and reconcile any reported
component drift before starting another lifecycle mutation.

Do not delete a pending journal merely to unblock a command. Run `doctor` to
identify its scope and subject, then run the qualified `repair` command. A
malformed or ambiguous journal is intentionally left pending for manual
inspection.

---

## Global Options

| Option | Description |
|--------|-------------|
| `--install-mode user\|system` | Select the mutation scope |
| `--prefix <PATH>` | Override the selected scope's install prefix |
| `--dry-run` | Print the plan without executing it |
| `--json` | Emit machine-readable JSON |
| `-v, --verbose` | Increase verbosity |
| `-q, --quiet` | Suppress non-error output |
| `--no-color` | Disable colored output |
| `--version` | Show the CLI version |
| `--help` | Show command help |

---

## Example Workflow

```bash
curl -fsSL https://get.agentic-os.sh | bash
anolisa env
anolisa install cosh
anolisa install tokenless
anolisa adapter enable tokenless cosh
anolisa doctor
anolisa status
```

---

## Configuration

Backend selection and endpoints are read from `/etc/anolisa/repo.toml` in
system mode or `~/.config/anolisa/repo.toml` in user mode:

```toml
schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "https://repo.example.com/anolisa/v1/"
```

The raw backend re-fetches the distribution index on every run: there is no
index cache and no stale-index fallback, so an unreachable repository fails the
command instead of resolving against older data. `cache_ttl_secs` and
`offline_fallback` were never wired up; configs that still set them keep
parsing, but the values are ignored.

CLI flags override the operation being run; there is no `[install] mode`
setting.

---

## See Also

- [Installation Guide](../installation.md)
- [Troubleshooting](../troubleshooting.md)
