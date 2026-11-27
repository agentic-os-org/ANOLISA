# Agent Plugins 1.0 package mounts

**Experimental, opt-in feature.** This design proposes `[[packages]]` in
`mount.toml` and `skillfs validate --package` as explicit entry points for
Agent Plugins 1.0.0. Without these inputs, existing `sources`, layouts,
and CLI invocations behave as before. The package CLI and configuration
may change until the feature graduates.

SkillFS gives consumers a read-only package root with portable Skills,
MCP configuration, and resources at their expected relative paths.
Operators can also project selected package Skills into the flat `/skills`
view. Marketplace operations, automatic dependency installation, client
extension execution, unified plugin-level trust decisions, and specification
versions after 1.0.0 are outside the scope of this design. The package's
own `version` is independent of the specification version.
[Issue #8735](https://github.com/agentic-os-org/ANOLISA/issues/8735)

The implementation baseline is `agentic-os-org/ANOLISA` `main` at
`87e0d988f92757b09bf71f2834e79f47a35d1206`. The earlier Linux mount,
activation, consumer, and package corpus experiments used SkillFS at
`60c4d4eea`.

Wave 1 covers this design, physical-directory resolution for categorized
Skills in flat mounts with discovery filtering, and the core validator
with `skillfs validate --package`.
[Design #10865](https://github.com/agentic-os-org/ANOLISA/pull/10865) ·
[Activation and discovery #10866](https://github.com/agentic-os-org/ANOLISA/pull/10866) ·
[Static validation #10868](https://github.com/agentic-os-org/ANOLISA/pull/10868)

The follow-up stages add the read-only `/plugins/<id>/` mount with optional
projections and inbox isolation (P2b), external activation and trusted
snapshot state (P3/G3), Hermes Agent 0.21.0 end-to-end acceptance (P4),
and corpus CI (P5).

## 1. Package inputs, views, and refresh

At the implementation baseline, `skillfs-core` discovers flat and
categorized Skill directories and indexes entries by leaf name. A package
root passed as a legacy source can yield Skills through the categorized
scan, without establishing package identity or preserving the portable
root layout. The `skillfs-cli` mount-file loader selects the first source
that provides a given name and uses that complete Skill with its resources.
[Skill loader](../../crates/skillfs-core/src/store.rs) ·
[Mount-file loader](../../crates/skillfs-cli/src/mount_file.rs)

### Package configuration and projection (D1)

Each `[[packages]]` entry sets a package `root` and may set `id` and
`project`. `root` names an absolute package directory. `id` is a safe
single path segment, defaults to the validated manifest `name`, and
must be unique within the mount. `project` lists exact names of
discovered Skills to expose under `/skills`. Omit it to expose those
Skills only in the package view. A mount can use packages on their own
or alongside ordered legacy sources.

```toml
mountpoint = "/mnt/agent-skills"
sources = ["/srv/skills"]

[[packages]]
root = "/srv/plugins/reports"
id = "reports"
project = ["summarize"]

[[packages]]
root = "/srv/plugins/research"
```

Both packages can provide `summarize` under their respective
`/plugins/<id>/skills/` directories. The reports entry also claims
`/skills/summarize`. A collision with a legacy source, another projection,
or the reserved virtual `/skills/skill-discover` entry is fatal at mount
startup. The configuration diagnostic names the Skill and both claimants.
`project = ["skill-discover"]` triggers this rejection. A valid
`/plugins/<id>/skills/skill-discover` remains available in the package view.
Projections use the exact Skill name and have no alias option. Legacy
sources retain ordered shadowing among themselves.

A projection owns its `/skills/<name>` slot until remount. Host changes
and store refreshes may discover a same-name legacy Skill, but the
projection keeps the slot. Mutations targeting that name through `/skills`
or the inbox return `EROFS` after the existing path and access checks.
Remount reevaluates ownership using the startup collision rules.

At P2b startup, canonicalize every configured package `root`, legacy
`sources` entry, and `mountpoint`. For a new mountpoint, reuse mount-file
resolution of its existing ancestors and check the resulting path before
creating the directory. Reject startup with a fatal configuration error
naming both claimants and paths if any resolved package root equals,
contains, or lies inside another resolved package root, legacy source,
or mountpoint.

P2b also checks inode isolation. At mount startup, walk each package tree
from its verified root descriptor without following symlinks. Reject the
mount if any regular file has `st_nlink > 1`, reporting the package root
and relative path. Check the link count again on the opened descriptor
before using a package-backed regular file at load time or serving it on
a new open. Deny a multiply linked runtime replacement. The check covers
hard links within the package, in legacy sources, and elsewhere on the
host. Operators must keep the roots free of cross-root bind mount aliases
and prevent host-side creation of cross-root inode aliases while mounted.
Direct host writes remain outside the mount's read-only policy.

Mutations of visible package paths and projected package Skills return
`EROFS` after the existing path and access checks, including `Hidden`
(`ENOENT`) and reserved `.skill-meta` (`EACCES` with audit). With the root
and inode checks above, legacy passthrough, ancestor directory renames,
and `/.skillfs-inbox/` operate on separate backing trees. The inbox serves
only eligible non-package legacy sources and is absent on package-only mounts.

The package view preserves relative depth. A link at
`skills/summarize/server` with target `../../bin/server` reaches the
package's `bin/server` under `/plugins/<id>/`. Flattening the Skill would
make the same link resolve above `/skills`. Consumers that rely on
package-relative resources use the package view. Containment and Skill
activation still govern access.

### Manifest and MCP snapshots (C1)

At mount validation, retain the original bytes of `plugin.json` and
top-level-valid `mcp.json` in memory, up to 1 MiB per document. This
matches the existing `DEFAULT_MAX_SKILL_SIZE` limit. Present
`/plugins/<id>/plugin.json` and `/plugins/<id>/mcp.json` as regular files.
Reads and `stat` sizes use the retained bytes even if the source entry
was a contained symlink or is edited or replaced after mount. Exceeding the
limit rejects the package for `plugin.json` or disables MCP for
`mcp.json`. When MCP is disabled, hide `mcp.json` in the package view and
report the reason.

A valid MCP envelope stays visible byte for byte even when individual
servers were skipped. SkillFS reports the skipped entries, and the
consumer independently validates the original entries before use.

### Top-level visibility (C2)

C2 exempts `plugin.json`, a valid `mcp.json`, and a discovered `skills/`
directory from `extensions` filtering. These fixed portable locations
follow C1 and Agent Plugins 1.0.0 §6.1–6.2 and §8.1.
[Fixed component locations](https://agent-plugins.org/specification#61-fixed-locations) ·
[Manifest extension data](https://agent-plugins.org/specification#81-manifest-extension-data)

Present `/plugins/<id>/skills` as a directory containing only the loader's
validated Skill subdirectories, subject to Skill activation. Present
`/plugins/<id>/skills/<s>/SKILL.md` as a transformed regular file under D2.
Like the C1 JSON files, these entries keep their mounted types when the
validated source entries are contained symlinks into a C2-hidden directory
such as `.payload/skills/`.

Apply these filters to directory listings and direct lookups at the
root of `/plugins/<id>/`.

1. Hide all entries with a leading `.`, including files and directories
   such as `.mcp.json` and `.claude-plugin/`.
2. Hide directories matching the reverse-domain pattern
   `^[a-z0-9-]+(\.[a-z0-9-]+)+$` and non-fixed directories matching a key
   of an object-valued `extensions` field. Apply the directory rules to
   contained symlinks resolving to directories.

This covers directory-only namespaces, including prefixes such as `org.`,
`io.`, and `dev.`. The filter uses the package's top-level entry names.
Other path exclusions, including containment and failed component
validation, apply independently.

P2b uses these same visibility rules to validate package-relative MCP
paths. A server whose command or working directory traverses a C2-hidden
entry receives `skip-server`. A top-level `runtime.v1/` directory matches
the reverse-domain pattern, even without an `extensions` entry. See
[Containment and diagnostics](#containment-and-diagnostics) for the
mount and offline validation rules.

Across 248 candidate packages, the static filter hid 284 top-level entries
and retained 1,035. The hidden set comprised 205 dot-prefixed entries,
33 directories matching declared extension keys, and 46 other namespace
directories. All 631 fixed or ordinary entry checks and nine
package-relative command and working directory checks for valid MCP
entries survived C2.
The scan also found client-specific layouts outside the rule. For example,
Agentlas-OS has plain `codex/` and `hermes/` adapter directories, which
C2 retains as opaque resources. The Google Maps `.well-known/ai-catalog.json`
index is hidden by the dot rule and belongs to a separate discovery
format. These results cover top-level entries and declared MCP paths.
[Agent Plugins extension convention](https://agent-plugins.org/specification#82-extension-directories) ·
[Agentlas-OS adapter](https://github.com/agentlas-ai/Agentlas-OS/blob/914bc10a24909cef8ccf5097e4860855c218dddd/codex/README.md) ·
[Google Maps catalog](https://github.com/googlemaps/agent-skills/blob/e7538f369afc18acbd9bde2cdcb9b7a7be6d7978/.well-known/ai-catalog.json)

### Skill transforms and refresh (D2)

`/plugins/<id>/skills/<s>/SKILL.md` and `/skills/<name>/SKILL.md` both
select the activation target before applying the existing
`TransformPipeline`. On each open, resolve the selected document relative
to the verified package root descriptor or, in P3, the trusted snapshot
root descriptor for `Snapshot`. Enforce containment and target policy
during descriptor-relative resolution, including through contained
symlinks. Verify that the opened object is a regular file, apply D1 to
package-backed files, and feed the transform bytes read from that same
descriptor. With the same target and profile both views serve the same
transformed bytes. The transform leaves the bytes on disk intact.
[Pipeline implementation](../../crates/skillfs-core/src/transform.rs) ·
[Read-time transform pipeline](read-time-transform-pipeline.md#1-activation-before-transformation)

With a fixed Linux profile advertising `uv` and `pnpm` and an empty
environment map, `skillfs-core::compiler::compile` changed 105 of 628
validated Skill documents (16.72%) at both `60c4d4eea` and `87e0d988f`.
The earlier revision predates the heredoc fix in `fae61bdd7`. Both
runs changed 109 of all 687 physical `SKILL.md` files (15.87%).
Byte differences in this corpus came from the default heuristic route.
[Compiler](../../crates/skillfs-core/src/compiler.rs)

Package discovery, projected names, validation diagnostics, and pinned
JSON documents are mount-time snapshots. Remount or restart a managed
mount to rescan them. For an already indexed Skill, reopening
`SKILL.md` under `Current` reads the latest source bytes. The same live
read applies with security disabled. An open transformed handle keeps
its open-time bytes through close, including after source replacement or
activation change. With `Snapshot`, a new open reads the selected approved
snapshot, and an unavailable snapshot fails the read. `Hidden` returns
`ENOENT` before transformation.
[Pinned open handles](read-time-transform-pipeline.md#pinned-open-handles)

## 2. Validation and discovery

The `skillfs-core` package loader validates root `plugin.json` first.
Each document selects a locally supported validator through its exact
`$schema` value, without a network fetch. Agent Plugins 1.0.0 uses:

- `https://agent-plugins.org/schemas/1.0.0/plugin.schema.json`
- `https://agent-plugins.org/schemas/1.0.0/mcp.schema.json`

The specification text governs parsing, type checks, and recovery.
[Agent Plugins 1.0.0 specification](https://agent-plugins.org/specification)

### Manifest and fixed locations

`plugin.json` must resolve to a regular file containing a closed JSON object.
`$schema` and `name` are required strings. The name is 1–64 characters
from lowercase ASCII letters, digits, hyphens, and periods. It begins
and ends with an alphanumeric character and contains neither `--` nor
`..`. Optional `version`, `description`, `homepage`, `repository`, and
`license` are strings. `keywords` is an array of strings. `author` is a
closed object with optional string `name`, `email`, and `url`. Check metadata
types. Suggested formats such as SemVer and SPDX are advisory.

Report and ignore each unknown top-level field. Report and ignore a
non-object `extensions`. When `extensions` is an object, its keys
participate in C2 and values for unimplemented namespaces are ignored
without validation. Other manifest violations reject the package before
component discovery.

Discover only root `skills/` and `mcp.json`. Manifest data cannot override
either location or declare components inline. Absence is valid. Under
§6.2, a present location that fails containment or fails to resolve to
its expected kind (directory for `skills/`, regular file for `mcp.json`)
disables only that component type. A dangling or looping link at a
fixed location counts as present and invalid.

### Strict Agent Skills 1.0 frontmatter (C3)

Inspect only immediate child directories of `skills/`. A candidate's
exact `SKILL.md` path must resolve to a regular file inside the package.
Cap each `SKILL.md` document at 1 MiB (`DEFAULT_MAX_SKILL_SIZE`). An
oversized document produces a `skip-skill` diagnostic that reports the
1 MiB limit. Omit that Skill from both views while keeping the package accepted.

Require YAML frontmatter followed by Markdown. The mapping is closed:

| Field | Rule |
| --- | --- |
| `name` | Required string. After Unicode NFKC normalization it is 1–64 characters, made of lowercase alphanumerics and single hyphens, with alphanumeric ends. Compare with the NFKC-normalized parent directory name. |
| `description` | Required nonblank string of at most 1024 characters. |
| `license` | Optional string. |
| `compatibility` | Optional string of 1–500 characters. |
| `metadata` | Optional mapping with scalar keys and values read as strings. |
| `allowed-tools` | Optional string. |

String fields and `metadata` keys and values require YAML scalars. Reject
sequences, mappings in scalar positions, unknown fields, and duplicate YAML
keys. The experimental validator reads scalars as strings through `serde_yaml`,
preserving unquoted spellings such as `true`, `1.0`, and `null`. It also
reads explicitly tagged scalar values such as `!!int 42` as literal strings.
Skip invalid Skills individually and omit them from both views, including
the package `skills/` resource tree.

Use a dedicated validator for packages. At the implementation baseline,
`skillfs-core` `parser.rs` accepts conventional fields such as `version`,
`tags`, and `requires` and retains some incomplete metadata as `Degraded`.
Legacy inputs keep that parser and `SkillStore::load_from_directory`.
Only valid, explicitly selected package Skills enter the flat store as
projections.
[Legacy parser](../../crates/skillfs-core/src/parser.rs) ·
[Agent Skills reference validator](https://github.com/agentskills/agentskills/blob/83021e2be457a9b6679148719c1f6074f0ad975e/skills-ref/src/skills_ref/validator.py)

### MCP envelope and server entries

First validate `mcp.json` as a closed JSON object containing exactly
`$schema` and `mcpServers`. Require the same supported specification version
as `plugin.json` and an object-valued `mcpServers`. An empty object is valid.
An invalid envelope disables MCP for the package.

Then validate each server independently against the closed variant
selected by `type`. An unknown type or field, field from another variant,
invalid value, or containment failure skips only that server.

| `type` | Server configuration |
| --- | --- |
| `stdio` | Requires a single executable token in `command`, either a bare platform executable name or a contained `./` path. A `./` path may contain spaces and remains one token. Optional `args` is a string array and optional `env` a string map. `env` cannot set `PLUGIN_ROOT` or `PLUGIN_DATA`. `cwd` defaults to the plugin root. A configured `cwd` is a contained `./` path, exactly `${PLUGIN_ROOT}` or `${PLUGIN_DATA}`, or a path starting with one of these placeholders followed by `/`. |
| `streamable-http` | Requires an absolute HTTP(S) `url` with neither userinfo nor a fragment. HTTPS is required except for `localhost` and loopback IP literals. Optional `headers` must be valid HTTP string fields with case-insensitively unique names. |
| `sse` | Same URL and header rules, using legacy HTTP+SSE. |

Expand exact occurrences of `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` once
in `args` elements, `env` values, and `cwd`. Replacement text is never
rescanned. Other placeholder-like text stays literal, and commands,
environment keys, URLs, headers, and discovery paths take no expansion.
Resolve `cwd` after expansion and enforce containment under the
corresponding resolved root. SkillFS checks the static form of
`${PLUGIN_DATA}` paths. The consumer checks their final containment
using its own data directory. Arguments and environment values are
opaque strings for path validation.

### Containment and diagnostics

Resolve package-supplied paths against the filesystem-resolved package
root and enforce component-wise containment on the resolved target.
Contained symlinks are allowed. At the fixed JSON paths, C1 presents the
validated bytes and their size as regular files, including when a source
symlink points into a dot-hidden directory inside the package. C2 presents
the validated `skills/` directory and Skill subdirectories by their logical
names. D2 applies descriptor-anchored opens to each fixed `SKILL.md`,
including through a contained source symlink.

Access companion files under `/plugins/<id>/skills/<s>/` against the
Skill's validated physical directory. Check containment at discovery and
use descriptor-anchored containment for runtime access to mutable resources.
Symlinks among companion files and non-fixed package resources resolve through
the mounted view, applying C2, Skill activation, and pinned-document rules to
their targets. The consumer owns process isolation. Apply the narrowest
failure boundary from §4.1.

Wave 1 static MCP validation checks physical package containment. P2b adds
the same C2 visibility check to the static loader used by `validate --package`
and mount validation. For a `./` command, a `./` cwd, or a cwd under
`${PLUGIN_ROOT}`, walk the path as in the mounted namespace, including
symlink traversal. Apply C2 whenever that walk visits a package-root entry,
including a symlink entry or its target. Report `skip-server` on `command`
or `cwd` if the walk crosses a C2-hidden entry. Runtime lookup applies the
same rules. C1 still serves the original config bytes, and the consumer
checks both runtime paths in the mount before starting a server.

| Failing path | Result |
| --- | --- |
| Root `plugin.json` | `reject-plugin` |
| Fixed `skills/` or `mcp.json` location | `disable-component` |
| Discovered Skill directory or `SKILL.md` | `skip-skill` |
| MCP `./` command or package-root cwd that fails containment or C2 visibility | `skip-server` |
| Other package resource | Deny access to that path |

The experimental `skillfs validate --package <root> [--format text|json]`
uses the same static loader as mount validation and makes no mount or
MCP connection. `--package` and the legacy Skill `SOURCE` argument are
mutually exclusive. The default format is `text`. Package validation exits
`0` when the root and `plugin.json` are accepted, even with warnings,
disabled components, or skipped entries. It exits `1` when the package is
rejected and `2` on a CLI argument error.

Text output uses the caller-supplied path and reports the component,
offending path or field, and effect. The JSON report contains
`experimental: true`, `accepted`, `manifest` (`null` on rejection),
`skills`, `servers`, and `diagnostics`. Each diagnostic contains `severity`,
`component`, package-relative `path`, `json_pointer`, `spec_ref`, and
`message`. An empty pointer addresses the entire document. A pointer into
a Skill document addresses its frontmatter. P2b mount diagnostics and
`explain` add `package_id` and the resolved `root`.
[Baseline CLI](../../crates/skillfs-cli/src/main.rs) ·
[Baseline explain implementation](../../crates/skillfs-cli/src/explain.rs)

## 3. MCP ownership and the acceptance client

| Responsibility | SkillFS | Consumer |
| --- | --- | --- |
| Discovery and diagnostics | Statically validate documents and package paths, expose the read-only C1 configuration, and report disabled components and skipped entries. | Read the pinned config, isolate bad entries, and diagnose unavailable transports and runtime failures. |
| Runtime paths | Preserve the layout and backing execution bits for resources visible under C2. | Bind `PLUGIN_ROOT` to the resolved mounted package path. Create writable, persistent `PLUGIN_DATA` outside the mount, dedicated to the installed plugin instance and preserved across updates. Resolve package-relative commands and cwd in the mount, skip unavailable entries, and pass args separately. |
| Environment | Validate expansion syntax and reserved variable names. | Apply expansion, overlay configured `env` on the base environment, then set the two reserved environment variables. |
| Transport and authorization | Classify server configurations and report static validation errors. | Use the declared transport for the initial attempt. Own processes, connections, handshakes, credentials, authorization, and per-server failure isolation. Support at least one of `stdio` or `streamable-http`. `sse` is optional. Client-generated headers take precedence. Forwarding configured headers to another origin requires explicit user authorization. |

The Linux mount options in `skillfs-fuse` at `87e0d988f` omit `noexec`.
Keep that behavior so a consumer can run an executable `./` stdio
command through the package view. The consumer decides whether to
execute, connect, or enable client extension behavior. It manages
credentials independently of the visible package env and headers.
[Baseline mount options](../../crates/skillfs-fuse/src/mount.rs)

**Hermes Agent 0.21.0** is the acceptance consumer. Pin commit
`9514d354ca47267c4c2c08dc639dd8f9331abc5d` and use its `uv.lock`.
In the Linux aarch64 experiment, Hermes discovered the package through
a symlink and `hermes chat --oneshot` completed an actual stdio MCP tool
call against a local model stub. A read-only SquashFS control mount
demonstrated execution from a mounted package path.
[Hermes lockfile](https://github.com/NousResearch/hermes-agent/blob/9514d354ca47267c4c2c08dc639dd8f9331abc5d/uv.lock)

Repeat that test with the discovery symlink pointing to SkillFS
`/plugins/<id>/`. Verify Skill reads, MCP discovery and initialization,
and a local `tools/call` round trip. Assert that `PLUGIN_ROOT` and cwd
resolve to the mounted package, the server writes to the external
`PLUGIN_DATA`, and a malformed sibling server receives a diagnostic
while the valid server and Skill remain usable.

OpenClaw 2026.11.10 retained the source with `plugins install --link` and
made a stdio call through the pinned bundle runtime. Codex CLI 0.172.0
`plugin add` copied the package into a versioned cache. Subsequent reads
and execution used the cache, disconnecting them from mount reads and
activation. Codex serves as an install-copy compatibility check.
[OpenClaw link option](https://github.com/openclaw/openclaw/blob/v2026.11.10/src/cli/plugins-cli.ts#L150) ·
[Codex cache copy](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/core-plugins/src/store.rs#L736)

## 4. Skill activation and provenance

Apply the existing Skill-level policy to each Skill and its companion
files through both views. Approval applies to one `canonicalSkillDir`
and its selected content. It confers no approval on other Skills,
package-level resources, MCP servers, or extensions. The consumer owns
MCP and extension security. Any future plugin-wide trust decision
belongs to a separate sec-core design.

Both views map a Skill to the same stable `canonicalSkillDir`. Identical
Skill names from distinct packages have separate canonical identities.
Keep notify v2 with `schemaVersion: 2` and its four business fields
`canonicalSkillDir`, `skillId`, `eventKind`, and `paths`. `skillId` is the
complete path relative to `canonicalRoot`. `paths` are relative to
`canonicalSkillDir`. P2b records `package_id`, the resolved `root`, and
the declared version when present in mount diagnostics and `explain`.
[Notify v2](notify-v2.md) ·
[Canonical and live roots](control-socket-resolver.md#canonical-root-vs-live-root)

### Support matrix and prerequisites (D3)

| Mount combination | Package support |
| --- | --- |
| Legacy flat and categorized sources without packages | Existing mount and activation combinations follow their existing rules. |
| Package views with optional projections and legacy sources, security disabled | P2b, with the D1 root and inode checks and read-only package paths. |
| Out-of-place package mounts with Skill-level security | P3, gated on the external activation and snapshot contract and the runtime checks below. Reject the combination until P3 enables it. |
| In-place package mounts (`--security-mode`) | Unsupported. Original package trees stay separate from the mount and writable state. |

At the implementation baseline, `skillfs-cli` mount-file validation
rejects security options. Wave 1 resolves categorized Skills to their
physical directories for disk and decision-command activation in flat
mounts. The default and secondary `skill-discover/SKILL.md` views omit
the names, metadata, and paths of `Hidden` and unmapped Skills.

P2b preserves `/.skillfs-inbox/` for eligible non-package legacy sources
even with activation disabled. Package paths are ineligible for inbox
lookup and writes under D1, and package-only mounts have no inbox.
Before P3 enables package security, resolve package Skills to their physical
directories for activation bootstrap and reload, and align control lookup
and notify with the selected identity.

[Mount option gate](../../crates/skillfs-cli/src/mount_file.rs) ·
[Activation implementation](../../crates/skillfs-fuse/src/security/activation.rs) ·
[Discovery rendering](../../crates/skillfs-fuse/src/fs/discover.rs) ·
[Installation path resolution](../../crates/skillfs-fuse/src/fs/paths.rs)

A `Hidden` Skill is absent from listings, direct lookup, and discovery
metadata in both views. Link and alias access must enforce the target
Skill's policy, including when the alias lies in another Skill or in a
package-level resource. SkillFS enforces these rules on paths through
its mount. Operators restrict consumer access to the physical backing
tree with host permissions or deployment isolation.

### P3 external activation and trusted snapshots (C4)

The baseline `activation-mode file` reads a per-Skill xattr or
`.skill-meta/activation.json` and resolves snapshots underneath the live
Skill. Existing control writes also target the live directory. S1's
`canonicalRoot`/`liveRoot` mapping separates identity from live I/O
while the current sec-core ledger still stores per-Skill activation and
versions in the live tree. `--decision-command` shares the snapshot
coupling. Its `fallback` result requires `targetKind` equal to
`relative_to_skill_dir` and a target under
`.skill-meta/versions/<version>.snapshot`. An immutable package therefore
requires external state for both activation paths.
[File activation](../../crates/skillfs-fuse/src/security/activation.rs) ·
[Control write contract](control-socket-resolver.md#relationship-to-activation-writes) ·
[External decision fallback](../security/external-decision-protocol.md#fallback) ·
[sec-core ledger](../../../agent-sec-core/v2/crates/asc-capability-skill-sec/src/ledger.rs)

SkillFS and sec-core must agree on these interfaces before enabling P3:

- Identify the trusted writer of activation records and approved snapshots
  outside the package, the authenticated control operations that update
  them, and how reload observes a complete state update.
- Specify how startup and reload map `canonicalSkillDir` to the trusted
  record and snapshot for file mode and decision-command fallback.
  Define the state root, its ownership and validation, the reload
  trigger, and the external fallback target contract. Account for S1's
  layout constraints and the mapping for contained symlinked Skills.
- Define fail-closed handling. Missing, unreadable, invalid, or untrusted
  external state keeps the affected Skill `Hidden` and reports the reason.
  On reload failure, new lookups and opens cannot fall back to live
  content. Existing transformed handles follow the open-time contract.

P3 runtime acceptance uses a genuinely read-only package filesystem such
as SquashFS and the authenticated control socket in its required
owner-only directory. Exercise trusted activation publication and reload.
Check that control writes update only the external state and that
package contents and xattrs remain unchanged. Cover missing and invalid
state at startup and reload. Verify that cross-Skill symbolic links cannot
surface hidden content in either view, with a same-Skill symbolic link as
a positive control. D1 rejects hard-linked backing files. Include package
resources, discovery metadata, and inbox paths in the visibility checks.
For both views, verify that `Hidden` (`ENOENT`) and reserved `.skill-meta`
(`EACCES` with audit) take precedence over read-only refusal (`EROFS`).

## 5. Regression coverage

Cover these cases with local fixtures in ordinary PR checks.

- D1 name collisions and same-name coexistence. For each pair of a package
  root and a legacy source, another package root, or the mountpoint, reject
  equal paths and nesting in either direction after resolution, including
  symlink aliases. Check that both paths appear in the fatal diagnostic,
  no view is mounted, and the physical package tree stays intact.
- D1 inode isolation on mixed mounts. With disjoint roots, hard-link a
  package file into a writable legacy source and repeat with an alias
  outside the configured roots. Verify both configurations fail at startup
  and leave the backing file intact. With single-link files, verify legacy
  writes leave package files intact. Replace a package file from the host
  with a hard-linked file and verify a new open cannot serve it.
- D1 projection ownership across refresh. Publish a same-name legacy
  Skill from the host after mount and verify the store refresh keeps the
  original projection. Verify writes and renames into the slot return
  `EROFS` through `/skills` and the inbox. Remount with both claimants
  present and verify startup rejects the collision.
- P2b inbox lookup and writes for eligible legacy sources on mixed mounts
  with disjoint roots, and inbox absence on package-only mounts.
- C1 byte and size stability for contained symlinks and source replacement.
- C2 listings and lookup with `skills/` linked to `.payload/skills/` and
  `SKILL.md` linked to a contained file under `.payload/`. Verify the Skill
  directory, transformed document, and companion files are accessible at
  their logical paths. Verify that `.payload/` stays hidden and companion
  symlinks targeting it cannot be followed.
- C2 and MCP path parity. Use separate servers whose `./` command or cwd
  traverses `runtime.v1/`, `.payload/`, or a symlink into either directory.
  Verify P2b offline and mount validation report `skip-server` on the
  offending fields, lookup cannot reach the hidden paths, and `mcp.json`
  keeps its pinned bytes. On the mount, execute a valid sibling with
  `./bin/runtime.v1/server` and verify its `./bin/runtime.v1` cwd.
- C3 validation boundaries, including a document over 1 MiB that is
  skipped in both views while the package remains accepted.
- D2 reopen in both views. Replace `SKILL.md` and, separately, an ancestor
  with a symlink escaping the package after mount. Verify new opens fail
  and existing transformed handles keep their bytes. Verify a contained
  replacement is visible on reopen in both views.
- Source immutability, view parity, remount semantics, and the
  credential-free Hermes test.

Run the existing SkillFS regression suite and Linux FUSE checks for mount
changes.

Keep the 22-package corpus pinned by revision, archive digest, and tree
identity for scheduled or manual replay. It contains 20 public package
snapshots and two upstream conformance packages, covering 382 valid
Skills and 14 valid MCP declarations. Retain the paired
`github/awesome-copilot` source and materialized snapshots to verify
the published package layout. Local checks use controlled consumers
and run independently of live community services.
