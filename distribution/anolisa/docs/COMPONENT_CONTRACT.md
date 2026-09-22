# ANOLISA Component Contract

This document defines where a packaged component exposes its ANOLISA
component contract and how ANOLISA should consume that contract across RPM and
raw backends.

## Package Location

RPM packages that expose ANOLISA component metadata MUST install the component
contract at:

```text
/usr/share/anolisa/components/<component>/component.toml
```

In RPM spec files, use the datadir macro rather than hard-coding
`/usr/share`:

```spec
%global anolisa_component sec-core

install -d -m 0755 %{buildroot}%{_datadir}/anolisa/components/%{anolisa_component}
install -m 0644 .anolisa/component.toml \
  %{buildroot}%{_datadir}/anolisa/components/%{anolisa_component}/component.toml

%files
%dir %{_datadir}/anolisa/components
%dir %{_datadir}/anolisa/components/%{anolisa_component}
%{_datadir}/anolisa/components/%{anolisa_component}/component.toml
```

Examples:

```text
/usr/share/anolisa/components/sec-core/component.toml
/usr/share/anolisa/components/tokenless/component.toml
/usr/share/anolisa/components/os-skills/component.toml
```

## Rationale

`component.toml` is static, package-owned, architecture-independent metadata.
Under the Filesystem Hierarchy Standard, that makes `/usr/share` the right
system location.

Do not install the component contract under:

- `/etc`: reserved for administrator-editable configuration.
- `/var/lib`: reserved for runtime state.
- `/usr/libexec`: reserved for helper executables.
- `/opt`: reserved for private package trees, not ANOLISA discovery contracts.
- `/usr/share/anolisa/adapters/<component>`: reserved for adapter payloads.

The adapter payload tree remains separate:

```text
/usr/share/anolisa/adapters/<component>/<framework>/...
```

The component contract is component-level metadata. It may describe adapters,
services, health checks, files, backend compatibility, and future lifecycle
behavior, so it should not live inside the adapter namespace.

## User And Raw Installs

For user-mode or raw installs, the same logical datadir layout applies.
ANOLISA follows the user roots described by `file-hierarchy(7)`: the default
data root is `~/.local/share`, and `XDG_DATA_HOME` may override that data root.

The default location is:

```text
~/.local/share/anolisa/components/<component>/component.toml
```

When `XDG_DATA_HOME` is set, use the overridden data root:

```text
$XDG_DATA_HOME/anolisa/components/<component>/component.toml
```

Raw archives may also carry the source contract at:

```text
.anolisa/component.toml
```

ANOLISA should normalize that source contract into the installed datadir layout
or directly into the installed-state snapshot described below.

## Installed-State Snapshot

ANOLISA should keep package-owned contract files separate from its runtime
state. After install or adopt, ANOLISA may copy the resolved contract into its
state directory:

```text
{state_dir}/component-manifests/<component>/component.toml
```

Typical paths:

```text
/var/lib/anolisa/component-manifests/<component>/component.toml
~/.local/state/anolisa/component-manifests/<component>/component.toml
```

The package-owned contract is the source provided by RPM or raw artifacts. The
state snapshot is ANOLISA's runtime record and may be used by commands such as
`anolisa adapter enable <component> <framework>` after the component has been
installed or adopted.

## Discovery Order

For an installed component, ANOLISA should resolve the component contract in
this order:

1. Existing installed-state snapshot:
   `{state_dir}/component-manifests/<component>/component.toml`.
2. Package datadir contract:
   `{datadir}/components/<component>/component.toml`.
3. Raw archive source contract during install:
   `.anolisa/component.toml`.

If an RPM-installed component has no package datadir contract, commands should
treat adapter declarations as unavailable and report that the RPM does not
publish an ANOLISA component contract.

## Adapter Operation Notices

An adapter contract may declare static, display-only operator notices that
ANOLISA shows after `adapter enable` or `adapter disable` succeeds. Notices
are declared with `[[adapters.notices]]` on a generic adapter entry, or with
`[[adapters.openclaw.notices]]` / `[[adapters.hermes.notices]]` in a
framework-specific section (which takes precedence over the generic entry):

```toml
[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "tokenless"

[[adapters.notices]]
when = "post_enable"
level = "info"
text = "Restart the framework to load the plugin."
command = "openclaw restart"

[[adapters.notices]]
when = "post_disable"
level = "warning"
text = "Cached tokens remain until the framework restarts."
```

Each notice has:

- `when` (required): `post_enable` or `post_disable`.
- `level` (optional): `info` (default) or `warning`.
- `text` (required): the notice body.
- `command` (optional): a display-only command hint.

Notices are inert text. `text` and `command` are never shell-expanded,
template-substituted, or executed. Human-readable output escapes control
characters to protect terminal state, while structured JSON preserves the
declared values. Required framework configuration is not a notice — it stays
a structured `[[adapters.config]]` entry.

Display behavior:

- `adapter enable` shows `post_enable` notices after a successful enable;
  `adapter disable` shows `post_disable` notices after a successful disable.
  `post_disable` notices are taken from the enable-time receipt, so they are
  shown even if the component manifest is no longer present.
- Human-readable output prints the notices; `--quiet` suppresses them.
- `--json` returns the notices in a stable `data.notices` array.
- `--dry-run` previews the notices that a real operation would show, labeled
  as a preview; nothing is executed.

## Installation Hook Integrity

On a fresh raw install, `post_install` and `post_enable` hooks may rewrite
regular files placed by the component layout. The installed digest records
the content after those hooks, so later content changes remain detectable.
Declared permissions, capabilities, and symlink referents still apply.
A removed regular file or one replaced by a symlink or special file
prevents the digest from being recorded and rolls back the install.
Files created by hooks outside the layout are not added to the ownership record.

Raw repair and update restore the package payload (including declared content
rendering); they do not replay installation hooks. Use `render` for layout-path
substitution that must also be reproduced by repair and update.

## Editable Files (`type = "config"`)

The raw backend records config layout entries, including files expanded from
directory sources, as `kind = "config"` in owned-file state. Their install-time
digest remains recorded, but integrity probes skip content hashing after checking
path boundaries, file type, permissions, and applied capabilities. Other regular
files remain immutable. This does not change update or uninstall behavior.

The runner carries the kind and mode from the actual source mapping through
directory expansion and placement. Distinct sources can share or overlap a
destination without changing each other's file contracts.

Legacy records recover config kinds from exact file declarations or when every
matching directory declaration is config. Mixed directory kinds are ambiguous
without source provenance, so those legacy files retain digest checking.
Back up edited configs and reinstall to record their kinds accurately; directory
permissions are inferred only when all matching declarations agree.
Read-only diagnostics do not rewrite state. Older CLI binaries cannot deserialize
the new `config` kind; before downgrading, back up state and change owned-file
`kind = "config"` entries to `kind = "file"`, restoring the old digest-check behavior.

## Content Rendering (`render`)

A `[[component.layout.files]]` entry may request that ANOLISA render the
file *content* against the final filesystem layout before placing it:

```toml
[[component.layout.files]]
source = "share/anolisa/sec-core/agent-sec-core.service.in"
target = "{userunitdir}/agent-sec-core.service"
mode = "0644"
render = "anolisa-paths-v1"
```

`anolisa-paths-v1` substitutes the same placeholder vocabulary used for
destination paths — `{bindir}`, `{libdir}`, `{libexecdir}`, `{datadir}`,
`{etcdir}`, `{statedir}`, `{logdir}`, `{cachedir}`, `{unitdir}`,
`{userunitdir}` (and underscore aliases), plus `{component}` — inside the
file content. This makes shipped templates such as systemd units
relocatable: the rendered `ExecStart` and `ReadWritePaths` follow the
install mode and any custom `--prefix` instead of hard-coding
`/usr/local`.

Rules:

- `render` applies to a single regular file. Directory sources and
  symlink entries must not declare it.
- The content must be valid UTF-8; an unknown `{...}` token in the
  content fails the install. Rendering is opt-in per entry, so files that
  legitimately contain braces are unaffected unless they declare
  `render`.
- A brace token immediately preceded by `$` is left verbatim. `${VAR}` is
  a shell or systemd environment reference that the **consumer** resolves
  at runtime — a unit fed by `EnvironmentFile=`, for example — so ANOLISA
  neither expands nor rejects it. The exemption is purely lexical: write
  `{bindir}` to get layout expansion, never `${bindir}`, which ships
  verbatim and is then resolved at runtime as an unset variable. A layout
  placeholder nested inside such a token still renders, so
  `${SKILL_ROOT:-{datadir}/skills}` keeps the environment reference and
  resolves `{datadir}`, and an unknown nested name still fails the
  install.
- The `$` exemption applies to file content only. `target` and every other
  destination path is written by ANOLISA itself, with no shell or service
  manager in the loop, so `${VAR}` there is rejected as an unknown
  placeholder.
- The value is versioned. An installer that does not implement the
  declared value refuses the install rather than copying the template
  verbatim; pair new `render` values with a matching
  `min_anolisa_version`.
- The recorded file digest covers the rendered bytes, so integrity
  verification and repair operate on what is actually on disk.

The RPM backend does not render: RPM packages expand paths in their spec
at build time. `render` exists so the same source template serves both
packagings.

## Minimum CLI Version Gate

`[component.contract].min_anolisa_version` declares the oldest ANOLISA
CLI that can install the contract:

```toml
[component.contract]
schema_version = "1.0"
min_anolisa_version = "0.2.17"
```

Raw install and update compare it (SemVer) against the running CLI and
refuse the operation when the CLI is older, pointing the operator at
`anolisa self-update`. A value that is not valid SemVer is also refused.
There is no override flag: manifest parsing is tolerant, so an older CLI
would otherwise silently drop fields such as `render` and install a
broken result. Set the field to the **first released ANOLISA version
that ships** whichever contract behavior the component depends on —
never an earlier release that merely parses the field. Content
rendering, this gate, and backend-specific adapter roots all ship in
0.2.17; a contract using any of them must declare at least that.

### Bootstrap: protecting CLIs older than the gate itself

The gate only works once a CLI that evaluates it is running — released
CLIs ≤ 0.2.16 parse tolerantly, so they would ignore both
`min_anolisa_version` and `render` and install the raw template as-is.
No contract-side hook can stop them (verified against a real 0.2.16
build — repo `index.toml` `schema_version`, `components.toml` schema,
and the manifest's own tolerant `schema_version` all do **not** stop an
old CLI), so the protection lives in the distribution layout instead:

- `v1/index.toml` — the generation-1 index. Must only ever contain
  entries whose contracts pre-0.2.17 CLIs install correctly. This is
  the only file those released binaries read, and they parse it
  atomically: one entry shape they cannot represent would take every
  unrelated component down with it.
- `v1/index-v2.toml` — the complete generation-2 index (a full world
  view, not a delta; publish both files from the same pipeline run).
  Entries whose contracts depend on ≥ 0.2.17 semantics are published
  **only** here. 0.2.17+ CLIs read this file when present and fall
  back to `index.toml` when it is not.

An old CLI therefore keeps installing unrelated components untouched
and resolves a gated component as not-found — fail closed, no silent
mis-install and no collateral breakage. From 0.2.17 the index is also
parsed entry-tolerantly: a row this build cannot represent is skipped
with a warning, and any request that row may have answered (same
component and target; any version for "latest", the exact version when
pinned) is refused with a `self-update` hint rather than answered from
older parsable rows — skipping must never turn into a silent downgrade.
This split is a one-time measure: future entry shapes will not need an
`index-v3.toml`.

### Downgrade and rollback with trust anchors

Enabling an adapter whose resources live at an external RPM root
records a trust anchor in `installed.toml` and bumps its schema to
v6. Anchors are recorded only when the enabled receipt actually
depends on external trust: it must contain a symlink resource whose
*target* lies outside the trusted layout and datadir roots (today only
the Codex driver records such a symlink). An RPM contract whose
`resource_root` resolves inside the datadir, or an adapter whose
receipt carries no symlink resources, needs no trust migration and
keeps state at v5. Released
0.2.16 binaries refuse a v6 state file read-only (by
design — they would otherwise silently drop the anchor), so a direct
binary downgrade while an anchored receipt exists leaves state
commands refusing until the newer CLI runs again. The supported
downgrade path is: `anolisa adapter disable` any adapters of
RPM-provenance components with a declared
`[adapters.backends.rpm].resource_root`, which removes their anchors
— an anchor-free state is written back at schema v5, byte-compatible
with 0.2.16 — then downgrade the binary. No data is lost either way;
the v6 refusal is deliberate fail-closed, not corruption.

## Backend-Specific Adapter Resource Roots

For a unified raw/RPM contract, the adapter `dest` describes where a
*raw* install lays the bundle — and, for raw installs, where
`adapter enable` later reads it. An RPM package may carry the same
bundle at a package-owned path instead. Declare that path per backend:

```toml
[[adapters]]
framework = "openclaw"
source = "adapters/sec-core/openclaw"
dest = "{datadir}/adapters/{component}/openclaw/"

[adapters.backends.rpm]
resource_root = "/opt/agent-sec/openclaw-plugin/"
```

Selection follows the component's installed provenance recorded in
state:

- RPM-installed (managed, adopted, or observed) with a declared
  `resource_root`: scan/status/enable read the bundle from that root. It
  is read-only — ANOLISA never writes under it — and must be an
  absolute path or a `{datadir}`-rooted template (plus `{component}`).
  Other layout placeholders (`{bindir}`, `{libexecdir}`, …) are
  rejected: they would expand against the *consuming* scope's layout
  rather than the contract's — a user-mode manager consuming a system
  RPM contract would resolve them under the user prefix. A template
  whose expansion is not an absolute path is likewise rejected before
  any filesystem probe: a relative root would resolve against the
  process working directory.
- RPM-installed without a declared `resource_root`: resolution falls
  back to `dest` and convention discovery, as before.
- Raw-installed: `dest` semantics are unchanged; the RPM root is
  ignored.

A declared root is authoritative for its backend. When the directory is
missing or holds no valid bundle, the operation reports the expected
path instead of silently falling back — a missing RPM payload is a
packaging defect that must surface, not be masked by a stale raw
leftover in `{datadir}`.

## Contract Template

Use a shipped component manifest such as
[`manifests/components/cosh/component.toml`](../manifests/components/cosh/component.toml)
as the example schema for new component contracts.
