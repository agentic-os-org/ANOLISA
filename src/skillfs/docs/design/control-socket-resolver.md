# SkillFS Control Socket Resolver (S1)

Design record for the stable default control socket endpoint and the
read-only `skill.resolveLiveSource` query. This is SkillFS package **S1**.
It does not implement notify v2, `register`, `mountId`, `generation`,
`sourceId`, multi-source runtime aggregation, or deletion-state semantics
— those are deferred to S2 (see [Out of scope](#out-of-scope)).

## Goals

`skill-ledger` needs a stable, authenticated way to ask a running SkillFS
instance: "for this canonical Skill directory, where is the physical
live/backing source, and is it yours to manage?" S1 provides:

1. A stable default control socket endpoint per UID / security domain, so
   the ledger does not need to be told a socket path out of band.
2. A read-only `skill.resolveLiveSource` query answering the mapping above
   with a strict three-state contract.

Everything reuses the existing control socket and its `schemaVersion "1"`
business envelope. The host profile retains its `SO_PEERCRED` +
trusted-executable-identity + process-starttime authentication unchanged. An
explicit container profile may instead complete the HMAC preface defined in
[SkillFS Container Peer Authentication](container-peer-authentication.md).
Neither profile changes the resolver business protocol version.

## Endpoint

Each UID / security domain has one resolver endpoint:

```
/run/user/<uid>/skillfs/control.sock
```

The effective endpoint is resolved by priority:

1. CLI `--control-socket <PATH>`
2. `[control_socket].path` in the config file
3. the default per-user endpoint above

The default path never falls back to `/tmp` or `/var/tmp`. If
`/run/user/<uid>` does not exist or is not a directory, startup fails with
a clear, actionable error instructing the operator to pass
`--control-socket` explicitly; SkillFS never invents `/run/user/<uid>`.

The control plane stays **opt-in and authenticated**:

| trusted peer configured | socket path configured | result |
| --- | --- | --- |
| yes | no | use the default endpoint |
| no | yes | configuration error |
| no | no | control plane stays off |
| yes | yes | use the explicit path |

Authentication is selected as one mutually exclusive profile. The host
profile uses `SO_PEERCRED`, pins the peer's `/proc/<pid>/exe` `(dev, ino)` to
the configured executable, and brackets the lookup with
`/proc/<pid>/stat` starttime for PID-reuse defense. The container profile
requires an explicit endpoint and shared-key mutual authentication before the
same resolver request is read. It never falls back to host authentication or
plain NDJSON after an authentication failure.

A single SkillFS instance may in the future manage multiple canonical
roots behind this same endpoint, selecting the live source by
`canonicalSkillDir`. S1 implements only the current single-source case and
does not pre-build multi-source runtime aggregation.

## `skill.resolveLiveSource`

Request (business parameter is only `canonicalSkillDir`):

```json
{
  "schemaVersion": "1",
  "method": "skill.resolveLiveSource",
  "canonicalSkillDir": "/absolute/canonical/skill/path"
}
```

### managed = true

The path is inside this instance's canonical root and resolves to a valid
live Skill directory.

```json
{
  "schemaVersion": "1",
  "ok": true,
  "result": {
    "managed": true,
    "canonicalSkillDir": "/canonical/path/apple/apple-notes",
    "skillId": "apple/apple-notes",
    "relativeSkillDir": "apple/apple-notes",
    "liveSkillDir": "/physical/live/path/apple/apple-notes",
    "identity": { "device": 42, "inode": 1001 },
    "transport": "shared_path"
  }
}
```

- The physical live/backing source is returned — never a FUSE current,
  fallback, or hidden view.
- `identity` comes from the actual opened live Skill directory.
- The query is read-only: it triggers no scan, manifest build, policy
  decision, or activation write.

### managed = false

Used only when the request is well-formed, `canonicalSkillDir` is a valid
absolute path, and the path lies outside this instance's managed root.
This is a normal success; `skill-ledger` may fall back to managing that
directory directly.

```json
{
  "schemaVersion": "1",
  "ok": true,
  "result": {
    "managed": false,
    "canonicalSkillDir": "/some/other/path",
    "reason": "not_managed"
  }
}
```

### structured error

The following are structured errors, never disguised as `managed=false`:

| condition | error code |
| --- | --- |
| protocol / request format error | `invalid_request` |
| non-absolute path | `invalid_canonical_path` |
| repeated or trailing path separator | `invalid_canonical_path` |
| illegal `..` (or `.`) segment | `invalid_canonical_path` |
| symlink / path escape | `invalid_canonical_path` |
| management / reserved directory | `invalid_canonical_path` |
| Skill directory does not exist under the managed root | `skill_not_found` |
| invalid Skill layout / missing `SKILL.md` | `invalid_skill_layout` |
| live source cannot be safely accessed / identity unverifiable | `live_source_unavailable` |
| peer authentication failure | `permission_denied` |

The error codes reuse the existing control-protocol style. The error
system is not redesigned.

## Canonical root vs live root

The resolver context stores the two roots explicitly rather than relying
on a single ambiguous `source_root`:

- **canonical root** — the absolute, lexically normalized user-visible
  Skill identity the ledger addresses (`canonical_identity_root` in the
  CLI). It does not follow a source-root symlink. Incoming
  `canonicalSkillDir` paths are checked for lexical containment against it,
  and the relative skill id is derived from it.
- **live root** — the backing/daemon-facing root whose physical content
  stays accessible after the FUSE over-mount (`daemon_root` in the CLI:
  the ledger backing root when configured, otherwise the realpath-resolved
  physical source). The live Skill directory is opened under this root,
  and its `(dev, ino)` is reported.

The roots can use different path strings even without a backing root when
the configured source is a symlink. They still identify the same tree, but
are kept separate so a query never crosses the canonical / FUSE / live
boundary implicitly.

The control plane is a **daemon-facing operation**: in an in-place mount
the source path is a FUSE over-mount, so resolving against it would return
the current/fallback/hidden view instead of the physical live source.
SkillFS therefore requires `--ledger-backing-root` whenever an in-place
`--security --activation-mode file` mount enables the control plane (as it
already does for notify/activation), and startup fails closed otherwise.
The live root is always an absolute path (the backing root, or the
canonicalized source), so `liveSkillDir` is usable regardless of the CWD
the mount was launched from.

## Path resolution and escape safety

Resolution is O(path depth) and never scans the whole Skill root:

1. **Raw-string lexical syntax** — validate the raw request string before
   constructing a `Path` or doing containment: reject a NUL byte, a
   non-absolute path, repeated separators, a trailing separator, and any
   `.`/`..` segment. This runs on the raw bytes because
   `Path::components()` would silently normalize aliases such as
   `/root//skill`, `/root/skill/`, and `/root/./skill`; an illegal request
   must never fall through to `managed=false`.
2. **Lexical containment** — the user path is *not* canonicalized;
   containment against the canonical root is purely component-wise. A
   valid path outside the root becomes `managed=false`.
3. **Reserved directories** — reject any dot-prefixed component
   (`.skill-meta`, `.hub`, lifecycle roots, staging dirs, …) and the
   synthesized `skill-discover` view.
4. **Layout boundary** — enforce the same Skill boundary as the FUSE layer
   and the Hermes id enumeration, so a subdirectory is never reported as a
   phantom Skill. Flat: a Skill is exactly one directory level. Hermes: a
   Skill is a top-level directory, or a `<category>/<skill>` leaf whose
   category is not itself a top-level skill (has no own `SKILL.md`);
   anything deeper, or a subdirectory of a top-level skill, is rejected
   with `invalid_skill_layout`.
5. **Safe descent** — open the live Skill directory one component at a
   time from the live-root directory fd, each with `O_NOFOLLOW |
   O_DIRECTORY`, so a symlink at any level fails closed (escape →
   `invalid_canonical_path`) rather than following outside the managed
   root. `(dev, ino)` is read via `fstat` on the final opened fd.
6. **Leaf layout check** — a Skill directory must contain a `SKILL.md` that
   is a **no-follow regular file**. Presence and type are classified with
   `fstatat(AT_SYMLINK_NOFOLLOW)` on the already-opened directory fd (never
   `openat`, so an unreadable mode-`000` marker is still correctly seen as
   present). A `SKILL.md` that is a symlink (never followed), directory, or
   any other non-regular object is **not a valid marker and is treated as
   absent** — a queried leaf without a regular marker returns
   `invalid_skill_layout`, and a symlinked *top-level* marker means the
   directory is a category whose real nested Skills still resolve. Only a
   genuinely inconclusive `stat` (e.g. an I/O error) fails closed as
   `live_source_unavailable`. The fd-based resolver and mutation paths share
   one classifier. Store discovery and FUSE layout code apply the same
   no-follow regular-file rule through their path-based helper, so no layer
   disagrees about what a Skill is (see
   [Skill layout](#skill-layout-and-skill-id)).

The backing path is never produced by naively joining unvalidated user
input; each component is validated and opened without following symlinks.

## Skill layout and skill id

The full skill id is derived from the canonical relative path, never from
the basename:

- **Flat**: `<root>/my-skill/SKILL.md` → `my-skill`
- **Hermes nested**: `<root>/apple/apple-notes/SKILL.md` → `apple/apple-notes`

Top-level and Hermes nested skills may coexist under one root. Querying a
category directory that has no `SKILL.md` of its own (e.g. `apple`) returns
`invalid_skill_layout`. The resolver enforces the layout depth boundary
(see [Path resolution](#path-resolution-and-escape-safety) step 4), so a
subdirectory of a Skill — `my-skill/subdir` under Flat, `top/sub` under a
Hermes top-level skill, or any third-level path — is never reported as a
phantom Skill, keeping the resolver's identities consistent with the FUSE
layer and the notify id enumeration.

Whether a directory "has a `SKILL.md`" — used for the top-level-vs-category
decision and the leaf check — follows one semantic predicate: a **no-follow
regular file**. A `SKILL.md` that is a symlink or other non-regular object is
not a marker, so such a top-level directory is a category (its real nested
Skills resolve) and such a leaf is `invalid_skill_layout`. The fd-based
control-plane paths share one implementation; store discovery and FUSE layout
code retain their crate-local path helper but apply the same rule. A directory
is therefore a Skill in every layer or in none — the resolver cannot report a
Skill the store never loaded, or reject one it did.

### Relationship to activation writes

The read-only resolver and activation write methods (`meta.writeActivation`,
`meta.setActivationXattr`) use layout-relative Skill ids. Flat layout accepts
one component, while Hermes accepts either a top-level Skill or
`category/skill`. Each component is validated independently and opened with
`O_NOFOLLOW|O_DIRECTORY`; nested ids are never truncated to a basename, and
managed or reserved paths remain invalid write targets. Writes in both layouts
require a no-follow regular `SKILL.md` at the leaf. Hermes nested writes also
require the first component to be a category rather than a top-level Skill.

## Shared fd boundary and container follow-up

### Decision for S1

Read-side resolution and activation writes use one internal fd-anchored Skill
directory capability. That shared layer owns:

- opening the trusted live root and descending each component with
  `O_NOFOLLOW | O_DIRECTORY`;
- classifying symlink, missing, and non-directory components;
- enforcing Flat and Hermes depth and category boundaries;
- requiring a no-follow regular `SKILL.md` at the leaf; and
- retaining the verified leaf fd for identity reads or metadata mutations.

Request syntax, reserved-name checks, response construction, and protocol
error mapping stay with each caller. This is intentional: for example, the
resolver reports a non-directory component as `invalid_skill_layout`, while
the existing activation-write protocol reports it as `skill_not_found`.
Sharing the syscall and boundary layer must not silently change either public
contract.

The capability is rooted at a directory fd internally rather than at a joined
user-controlled path. S1 still opens that root from the configured shared path
and still returns `transport = shared_path`; it does not add an unused wire
protocol or expose a new public API.

### Container and security-integration plan

The shared fd boundary is a prerequisite, not the complete container design.
Follow-up container work is split into these independently testable steps:

1. **SkillFS sidecar only** — keep the current shared-volume FUSE topology.
   OpenClaw flat/staging paths and Hermes nested paths continue using their
   existing layout semantics; replacing the example workload with either real
   runtime does not itself require a new resolver protocol.
2. **Ledger colocated with SkillFS** — when security integration is required
   before a cross-container trust design is approved, run the trusted Ledger
   worker in the SkillFS container. Existing `SO_PEERCRED`, `/proc/<pid>/exe`,
   socket, and ledger-backing-root assumptions remain locally verifiable.
3. **Ledger in a separate container** — define a shared runtime volume for
   sockets and an explicit source capability transport. Evaluate a shared PID
   namespace against a container-aware authenticated identity; any identity or
   executable lookup ambiguity must fail closed.
4. **General multi-container/Kubernetes support** — add `dir-fd` /
   `SCM_RIGHTS` only with a versioned transport contract, namespace-aware
   identity tests, and lifecycle tests for sidecar restart and fd revocation.
   The same verified-directory primitive then consumes the received root fd,
   so Flat/Hermes Skill boundaries do not fork again.

The complex part of containerizing Skill Ledger is therefore not OpenClaw or
Hermes parsing. It is preserving four cross-container security invariants:
authenticated peer identity, visibility of the runtime socket, access to the
physical live/backing source rather than the FUSE view, and consistent fd/path
identity across PID and mount namespaces. Until those invariants have an
accepted threat model and black-box Kubernetes tests, the separate-Ledger
profile is not advertised as supported.

## Socket lifecycle hardening

Single-instance, fail-closed lifecycle:

- The socket parent directory must be a directory owned by the current
  uid with mode `0700`; the socket file is `0600`.
- A non-blocking `flock` lifecycle lock on `<socket>.lock` guards the
  endpoint. A second instance targeting a live endpoint fails fast (it
  never blocks unbounded and never unlinks the active socket).
- A pre-existing object is only reclaimed when it is confirmed stale: a
  socket the current uid owns whose non-blocking `connect` probe returns a
  definitive `ECONNREFUSED`. A successful connect (live listener), a
  backlog-full result, or any inconclusive error (`EACCES`, `EINTR`,
  resource exhaustion, …) fails closed rather than unlinking. Symlinks,
  regular files, directories, and sockets owned by another uid are never
  deleted.
- The bound socket's `(dev, ino)` is recorded. On shutdown the path is
  unlinked only if it still resolves to that exact identity, so an object
  that later replaced the path is never deleted.
- The accept loop is non-blocking and polls the shutdown flag, so shutdown
  is bounded even when the path has been replaced.

## Query load

`skill-ledger` issues high-frequency, saturating resolve queries over
candidate directories. S1 keeps the existing one-request-per-connection
model handled on a single accept thread — no thread-per-request, no
speculative global cache, and no artificial millisecond SLA. Each query is
O(path depth). Bounded concurrency is deferred until measurements justify
it.

## Out of scope

Deferred to S2 and not implemented here:

- notify v2 (implemented separately in
  [SkillFS Notify v2](notify-v2.md)); the existing notify protocol is
  unchanged in S1.
- `register` / `unregister`, `mountId`, `generation`, `sourceId`,
  `resolverSocket`, and `integrationProtocolVersion`.
- deletion-state semantics (tombstone / `exists`); S1 does not add these
  unsettled fields and does not interpret a deleted path as `not_managed`.
- multi-source runtime aggregation.
- `dir-fd` / `SCM_RIGHTS` transports (`transport` is always
  `shared_path`).
- any change to `agent-sec-core`.
