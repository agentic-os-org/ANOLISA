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

RPM installs support Yum 3 and DNF 4. ANOLISA prefers yum and uses dnf when
yum is absent. Use `sudo` to check dependencies and conflicts before installing:

```bash
sudo anolisa --install-mode system --dry-run install cosh --backend rpm
```

This check does not install or change packages. Resolve any reported conflicts
before retrying; a successful check does not guarantee the later installation
will succeed. If a requested version conflicts with system restrictions such
as a version lock, the command fails without substituting another version.

If ANOLISA cannot identify the system's package manager, follow the reported
instructions to install missing dependencies manually, then retry.

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

For OpenCode, manage an installed Tokenless plugin with:

```bash
anolisa adapter enable tokenless opencode
anolisa adapter status tokenless
anolisa adapter disable tokenless opencode
```

The driver finds `opencode` on PATH, or uses `OPENCODE_BIN`. It registers the
manifest's `.js` or `.ts` entry as `plugins/<plugin_id>.<extension>` under
`OPENCODE_CONFIG_DIR`, otherwise `XDG_CONFIG_HOME/opencode`, otherwise
`~/.config/opencode`. Custom directory values must be absolute. Keep the same
directory configuration for later status and disable commands; if it changes,
restore the original environment before retrying cleanup. Project-local plugin
installation and npm plugin management are outside this driver's scope.

Enable adopts an existing symlink to the same plugin source, including one
created by Tokenless's standalone installer. Relative links must resolve lexically to the
recorded source path; directory aliases are rejected so cleanup remains possible after
package removal. After adoption, disable removes
that link. A conflicting file, directory, or different symlink is preserved;
failed cleanup keeps the receipt so it can be retried after resolving the conflict.
Same-path upgrades replace the entry atomically. If enable or disable is interrupted,
retry the command with the same configuration to recover pending changes. If an error
reports a preserved entry, keep its recovery directory, resolve the conflicting public
path, and retry; recovery also restores displaced directories. Complete ANOLISA disable and
recovery before switching back to a standalone installer, which does not process these journals.
The configuration directory's filesystem must support atomic exchange and non-overwriting
rename for replacement and cleanup. If it does not, the operation fails; resolving a pathname
conflict alone will not add the missing filesystem support.
If the old installer used `TOKENLESS_OPENCODE_CONFIG_DIR`, set
`OPENCODE_CONFIG_DIR` to that same directory before enabling through ANOLISA.

Restart OpenCode after enabling or disabling. Status verifies the link and
package source and preserves `cleanup_failed` when enable was interrupted or cleanup still
needs a retry, even if the active link matches. Retry enable or complete disable to resolve it. Runtime loading is reported as `unknown`; an existing link
does not prove a running OpenCode process loaded it. `--dry-run` previews the
operation without modifying plugin files or receipts.

### logs and bug reports

Inspect component logs or generate a diagnostic bundle:

```bash
anolisa logs <component>
anolisa logs <component> --limit 50
anolisa logs <component> --severity warn
anolisa bug
```

`--level` is an alias for `--severity`.

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

For RPM repositories that require a proxy or a private CA, set `proxy`,
`proxy_username`, `proxy_password`, or `sslcacert` directly in `[main]` of
`/etc/yum.conf` (or `/etc/dnf/dnf.conf` when the former is absent).
Without explicit settings, ANOLISA uses environment proxies (`http_proxy`,
`https_proxy`, `all_proxy`, `no_proxy`) and the system's trusted certificates.

CLI flags override the operation being run; there is no `[install] mode`
setting.

---

## See Also

- [Installation Guide](../installation.md)
- [Troubleshooting](../troubleshooting.md)
