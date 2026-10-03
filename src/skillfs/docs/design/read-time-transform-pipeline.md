# Read-Time Transform Pipeline

Design notes for the `SKILL.md` read-time transform pipeline: the ordering
contract, the optional stage model, immutability guarantees, and the external
rule-provider contract. This document describes component-internal design; the
user-facing configuration reference lives in the SkillFS README and
`docs/user-guide/{en,zh}/runtime/skillfs.md`.

## 1. Activation before transformation

A `SKILL.md` read is served in a strict order:

1. Parse the Agent-visible path.
2. Resolve activation to `Current`, `Snapshot`, or `Hidden`.
3. `Hidden` returns `ENOENT` immediately — it never enters the pipeline.
4. Read the bytes of the *selected* target only: the live source for `Current`,
   the trusted snapshot for `Snapshot`.
5. Run the configured transform stages over those bytes.
6. Serve the transformed bytes and report their exact length from `getattr`.

Transformation is therefore always downstream of the security decision. A
`Snapshot` read transforms the snapshot bytes and never falls back to the live
source, even if a stage errors; a `Hidden` skill is never read or transformed.

### Pinned open handles

A non-mutating read-only open of a transformed flat or Hermes `SKILL.md`
captures the complete transformed UTF-8 bytes after selecting activation once.
Reads slice that immutable result until close, including after Current edits,
atomic replacement, activation changes, or removal of the selected snapshot.
A new open reads the newly selected source; a missing snapshot fails without
live fallback. Empty pipelines, writable or truncating opens, staging, pending
installs, passthrough files, and skill-discover retain their existing behavior.

Captured handles use FUSE direct I/O so the inode's shared page cache cannot
mix different open-time versions. This bypasses kernel readahead and does not
enable shared mmap for these handles. `getattr` remains an inode operation:
it reports the currently selected target's transformed length, not necessarily
an old handle's length. Reads and EOF on a captured handle use its own bytes.
Linux may omit the handle even for `fstat`; inode-only queries must not choose
an arbitrary open handle as the size authority.

## 2. Optional stages, fixed order

The pipeline holds each stage in a dedicated typed slot:

```text
directive: Option<DirectiveStage>
os_adapter: Option<OsAdapterStage>
```

`run` applies `directive` (if present) then `os_adapter` (if present) and
returns the input unchanged when both are absent. This makes three properties
structural rather than enforced by convention:

- **Fixed order** — `directive` always precedes `os_adapter`.
- **No duplicates** — a slot holds at most one stage; a stage cannot be added
  twice.
- **Optional** — either or both slots may be empty (directive-only, adapter-
  only, or a fully empty raw-passthrough pipeline).

The set of stages is decided once, at mount startup. `run` performs only
in-memory work; it never parses YAML, reads `/etc/os-release`, spawns a process,
touches the network, or calls an LLM.

### Why directive stays enabled by default

The directive/compiler stage predates this pipeline and is the historical
compile-on-read behavior. Keeping it enabled by default (when
`[transforms.directive]` is absent) preserves byte-for-byte output for existing
mounts. Making it *optional* — rather than a hardcoded first stage — lets it be
disabled for adapter-only or raw operation and leaves room to remove or replace
it later without reworking the pipeline shape.

## 3. Source and snapshot immutability

Transform stages are pure functions over the read bytes. They never write to
the source tree, snapshots, activation metadata, or the rule artifact. The only
observable effect is the bytes an Agent reads; the physical `SKILL.md` and any
trusted snapshot on disk are unchanged by a read.

## 4. Excluded paths

Only `SKILL.md` reads flow through the pipeline. The following never enter it:

- `.skill-meta/**` and lifecycle-reserved roots;
- activation JSON and xattrs;
- control-socket payloads;
- `skill-discover` virtual content;
- every other file type (other Markdown, shell, Python, YAML, JSON, TOML).

## 5. Transformed size and read semantics

The same transformed byte string backs `getattr` size, complete reads, and
offset/partial reads for a given resolved target, so a tool that stats then
reads (or reads in chunks) sees a consistent view. Content that grows or shrinks
under the OS adapter is reflected in the reported size.

## 6. OS-adapter rule catalog and compatibility

SkillFS ships a built-in Ubuntu/Alinux rule catalog and embeds it in the binary
from the repository asset `crates/skillfs-core/assets/ubuntu-alinux.yaml` via
`include_bytes!`, so the default adapter works in source builds, RPMs, and
containers without a separate on-disk file. An operator may override the default
with an external read-only artifact by setting a non-empty `rules_path`. Either
way the artifact is a top-level YAML sequence, loaded and validated once at
mount startup; `OsAdapterStage::load_default` compiles the built-in bytes and
`OsAdapterStage::load` compiles an external file. There is no second in-code
mapping table — the catalog lives only in the asset.

### Built-in catalog composition

The bundled catalog carries **311 rules** covering package-manager verbs,
`-dev`/`-devel` package names, service unit names, and filesystem paths. Each
rule's eligibility is normalized to an explicit `auto_apply`: 257 rules are
`auto_apply: always` and 54 are `auto_apply: never` (51 medium-confidence and 3
low-confidence). The high-confidence `cron`/`cronie` mapping is eligible with
`match: token`, replacing the duplicated install-command entries without
matching host words such as `crontab` or `cronutils`. Protected rules are
documented in the catalog but never substituted — SkillFS performs no
verification, Repology lookup, network call, subprocess, or LLM review to
promote them. After normalization the catalog produces **223** non-identity
active substitutions for target Alinux and **192** for target Ubuntu, with no
duplicate or ambiguous active mapping for either target.

### Explicit rule contract (built-in and external)

- Each rule declares `ubuntu`, `alinux`, `direction`, and a **required**
  `auto_apply` (`always` | `never`). Eligibility is governed solely by
  `auto_apply`; only `always` rules are applied, and only in a direction the
  resolved target permits.
- `match` is optional and defaults to `literal`, retaining the historical
  substring semantics for existing artifacts. `token` requires
  ASCII-alphanumeric boundaries at alphanumeric source edges; the same mode is
  applied after source/target reversal.
- `confidence` and `notes` are accepted but inert — SkillFS attaches no behavior
  to them.
- An artifact that omits `auto_apply` on any rule is rejected with an indexed
  error, rather than defaulting to applied. This applies to external override
  artifacts too: they must carry explicit `auto_apply`.
- Duplicate and ambiguous active mappings are rejected. A many-to-one forward
  mapping must resolve reverse ambiguity explicitly: exactly one pair is
  `bidirectional` (the canonical reverse) and the alternates are direction-
  scoped (`ubuntu_to_alinux_only` / `alinux_to_ubuntu_only`). The built-in
  catalog applies this to the apt shorthand verbs (`apt update`, `apt upgrade`,
  …), which are `ubuntu_to_alinux_only` so the reverse target uses the canonical
  `apt-get`/`apt-cache` spelling without collision.

### Non-cascading substitution

`apply` runs a single left-to-right pass over the *original* read bytes. At each
position it selects the **longest** matching source pattern (most specific
wins), emits that rule's declared target, and advances past the consumed span.
Neither the replacement text nor already-scanned input is rescanned, so:

- overlapping patterns never chain — `apache2` does not rewrite the inside of
  `apache2-utils`, and `cron` does not re-hit the `crond` a more specific rule
  produced;
- each rule is a 1:1 map to its declared target, independent of file order (two
  distinct sources of equal length cannot both match at one position, so the
  longest match is unambiguous).

Before a `token` candidate enters longest-match selection, the scanner rejects
it when an ASCII letter or digit touches an alphanumeric source edge. Thus a
bounded `cron` maps before EOF, whitespace, newlines, or punctuation, while
`micron`, `crontab`, `cronutils`, and `cron2` do not match. Literal rules retain
their previous prefix behavior.

A naive per-rule sequential `replace` would corrupt these cases; the single-pass
scan is the correctness fix.

**Protection matches.** Ineligible patterns — `auto_apply: never`, identity
(`from == to`), and direction-disallowed for the resolved target — also take
part in the scan, as *protection* matches: when one is the longest match at a
position it is emitted verbatim and skipped. Without this, dropping ineligible
rules from the compiled table let a shorter eligible rule rewrite inside a span
an ineligible rule claims (e.g. the `never` path `/etc/init.d/apache2` becoming
`/etc/init.d/httpd`, or the identity `postgresql-contrib` becoming
`postgresql-client-contrib` on reverse), silently bypassing eligibility. An
Protection is deduplicated by `(source, match)`. An eligible substitution
removes protection only for the same source and match mode; different modes for
one source remain in the compiled scanner. When both candidates match, the
substitution wins their equal-length tie. If its mode rejects the current input,
the other-mode protection can still preserve the full source span and prevent a
shorter rule from penetrating it. Thus a direction-disallowed alternate never
suppresses a matching canonical reverse mapping, while token substitution and
literal protection can intentionally coexist.

### Fail-closed OS detection

`target_os = "auto"` maps the exact `/etc/os-release` `ID`: `ubuntu`/`debian`
→ Ubuntu, `alinux`/`anolis` → Alinux. `ID_LIKE` is intentionally ignored, so
RHEL-family derivatives are not silently treated as Alinux and unrecognized
hosts reject the mount. Operators on other distributions must set `target_os`
explicitly. A present-but-blank `rules_path` is rejected as a misconfiguration
rather than silently falling back to the built-in catalog.

## 7. Content-free Open audit context

A successful non-mutating Open of a virtual flat or Hermes `SKILL.md` carries a
stable detail string when the OS adapter is enabled:

```text
transform=os_adapter target_os=<target> rule_digest=<sha256>
```

The pipeline exposes only a borrowed metadata view containing the resolved
target and rule-artifact digest; it never exposes or copies the rule table. The
event does not include source bytes, transformed bytes, a diff, or rule content.
Write, staging, pending, passthrough, adapter-disabled, and failed/hidden paths
are not labeled as adapter transforms. Successful per-syscall Read events remain
suppressed to avoid audit flooding.

## 8. Bounded cross-open reuse

Each mount owns an in-memory LRU of at most 128 entries and 8 MiB of transformed
UTF-8 payload. `lookup`, `getattr`, and eligible read-only opens reuse the same
`Arc<str>` when the complete key matches: Skill identity (including Hermes
category), physical path, pinned activation target (including snapshot path
and version), source device/inode/size/nanosecond mtime and ctime, a SHA-256
digest of the selected source bytes, and pipeline fingerprint. Activation is selected once for both metadata and transformed
size. Reads on captured handles never consult the LRU or the physical source.

The pipeline fingerprint covers the ordered enabled stages, the directive
stage's captured OS, sorted available commands and environment key/value pairs,
and the adapter's resolved target and rule-artifact digest. Length-prefixed
fields prevent ambiguous concatenation. It is computed when configuring stages,
not on reads; environment or rule-file edits still require a remount to take
effect. No environment values or Skill content appear in cache diagnostics.

Cache lookup opens and reads the selected physical file, then hashes those
bytes before checking the key. Timestamp precision does not guarantee a unique
version: same-size writes can share mtime and ctime, including when mtime is
restored. Every lookup/getattr/new open therefore pays for source reading and
hashing, while hits avoid pipeline execution and transformed-result allocation.
Population uses those same bytes and checks both the fd metadata and the
selected path after transformation. If an edit or replacement is detected, the
captured result is returned without insertion. This fixes the bytes for that
handle; it does not promise an atomic snapshot of a source being concurrently
written in place. Snapshot failures never trigger live-source fallback.

The cache lock covers LRU ordering and entry/byte accounting, not I/O or
transformation. Concurrent cold misses may perform duplicate work; insertion
deduplicates an exact key. Debug events report only hit, miss, invalidation,
eviction, entry count, and payload bytes. Tests count transformations through
the loader closure without production pipeline counters.

Oversize results are kept only by their open handles. Eviction drops the LRU's
reference without revoking open handles; the 8 MiB limit bounds cached payload,
not in-flight transformations. Captured handles have a separate mount-wide
budget of 64 MiB and 1,024 handles, charged at the full transformed length per
handle even when the payload is shared. Admission and release update both
counters under the handle-table lock. Exceeding either limit (including a
single result larger than 64 MiB) returns `ENOMEM` for the new open; existing
handles keep their bytes and closing them restores capacity. Counting empty
handles also bounds their bookkeeping. Thus retained transformed payload is
bounded by 64 MiB for handles plus the 8 MiB LRU; temporary load/transform
allocations are outside these retained-memory budgets. Closing handles and
dropping the mount release their references. Nothing is persisted to disk.
Empty/raw pipelines and mutable, staging, pending-install, passthrough, and generated
skill-discover reads do not use the transformed-content cache.

Additional text-file types, script transforms, LLM/network calls in the read
path, and rule hot-reload without a remount remain out of scope.
