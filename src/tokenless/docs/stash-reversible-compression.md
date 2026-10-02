# Reversible Compression (Stash)

When a compressor makes a recoverable omission, the payload is stashed under a
BLAKE3-derived key and the output includes an optional recovery instruction.
The model can retrieve that payload while it remains in Stash. This is not a
promise that every removed field is saved, that the model will request recovery,
or that retrieval reduces whole-session token use.

This mirrors Headroom's CCR (Compress-Cache-Retrieve); the mechanism here is
called **stash** to avoid the proprietary abbreviation.

## How it works

1. **Compress**: `JsonCompressor` first prefers a complete-data representation
   when it saves at least 15% of estimated tokens. Otherwise, a record array
   (at least 33 JSON objects) may be reduced to a 32-record base budget while
   preserving boundary, error, structural-anomaly, and numeric-outlier
   records. The complete pre-transform array is serialized once and
   `stash.stash(payload)` stores it. Other oversized arrays retain the first
   32 and last 8 items and stash the dropped middle window. A successful write returns a
   24-hex BLAKE3 key plus a store-wide, monotonically increasing ownership
   token used if the write must later be rolled back. Tokens are never reused
   after expiry, deletion, or eviction.
2. **Mark**: the output contains an instruction such as
   `32 of 64 records omitted. If needed, run in shell: tokenless retrieve HASH`.
   AgentScope instead sees `If needed, call tool NAME with hash_or_marker=HASH`,
   using its actual registered static Tool name.
3. **Retrieve**: `tokenless retrieve <KEY>` reads the original payload through
   the trusted same-user CLI boundary. Claude Code 2.1.121 or newer, Qoder CLI,
   OpenCode, Cosh-NG, Hermes, and DSH let the Marker direct the model to this
   existing shell command. Their adapters recognize only a successful,
   standalone retrieve command with a valid Hash or Marker and classify its
   output as an already recovered result, so it bypasses compression. AgentScope
   instead keeps its static Retrieve Tool and authorizes the key against the
   Marker set currently visible to the model before reading Stash.

DSH removes inherited `TOKENLESS_*` variables from model shell commands. Its
adapter therefore gives Core and the managed shell the same state directory,
defaulting to `.tokenless` beneath the current session workspace. An absolute
`TOKENLESS_DATA_DIR` set before DSH starts overrides that default when the DSH
shell sandbox can access it.

When no stash store is attached (`Option<Arc<dyn StashStore>>` = `None`),
recognized record arrays are not reduced and do not fall back to positional
array truncation. Other bounded transformations retain their existing lossy,
non-retrievable behavior where the caller permits it.

## No-savings rollback

`JsonCompressor` returns every tentative `StashWrite` in `JsonOutcome`.
`PostToolPipeline` records them in one per-invocation ledger, performs the
single final character/token arbitration, then commits only keys referenced by
the accepted output or rolls the ledger back. Markers that never reach the LLM
therefore do not leave orphan stash rows.

The ledger tracks each content-addressed key and generation. A key created in
this invocation is owned by the ledger; a later refresh updates that ownership
only when the store reports an unbroken chain (`previous_generation` equals
the generation last recorded by this invocation). A refresh of a key the
invocation never created stays off the rollback list.

That chain check is required because content-addressed keys are shared across
processes. If compressor A creates P, compressor B refreshes P and emits a
marker, then A stashes P again, re-adopting B's generation would make A's
no-savings rollback delete the row B's marker still needs. A mismatch drops
the key from the pending list instead; rollback of the stale create-time
token is a CAS no-op.

Runtime artifact metrics count live writes after the final commit or rollback,
not mutable compressor-side state.

Session scope differs by compressor:

- PostTool JSON compression uses a fresh Runtime ledger for every invocation.
- `SchemaCompressor` accumulates across `compress()` calls until rollback or
  `clear_stash_session()`. That matches `compress-schema --batch` (compress
  every item, then one all-or-nothing rollback). Call rollback only after
  every emit/discard decision for the session. Programmatic callers that
  emit some results and later discard others on the same instance must call
  `clear_stash_session()` after keeping output; otherwise a later rollback
  deletes those emitted markers.

## Recovery instructions and historical markers

```
12 passing-test lines omitted. If needed, run in shell: tokenless retrieve HASH
12 passing-test lines omitted. If needed, call tool tokenless_retrieve with hash_or_marker=HASH
```

- `HASH` is the first 24 hex characters (12 bytes / 96 bits) of a BLAKE3 hash
  of the stashed payload. 96 bits makes a collision astronomically unlikely
  (2⁴⁸ birthday bound), so a key is treated as a unique handle.
- `HASH` in these examples stands for the complete 24-hex value from the output.
- `capabilities.recovery` is required: `none`, `shell`, or `tool` with a validated
  static Tool name. Names allow 1–64 ASCII letters, digits, underscores, or hyphens.
  The old `retrieval_available` boolean is rejected; callers and Core must migrate together.
- CCR formats instructions before candidate sizing and arbitration. The ledger retains
  only complete output references with valid hashes and explicit boundaries. Bare hashes
  are not references, and tool instructions must match the currently declared name.
- BeforeModel collects complete shell instructions, matching tool instructions, and historical
  `<<tokenless:HASH>>` markers from actual visible strings. It returns sorted, deduplicated
  lowercase hashes. Only `tool` enables recoverable schema truncation; `shell` does not.
- New output never generates angle-bracket markers. `parse_marker` and CLI retrieval keep
  historical read support; `extract_hash` also recognizes complete shell instructions.

## Backends

| Backend | Feature | Persistence | Use when |
|---|---|---|---|
| `InMemoryStore` | default | process memory | tests, single-process CLI runs |
| `SqliteStore` | `sqlite` (on by default) | SQLite file (WAL) | **production hook path** |

The tokenless hooks fork+exec a fresh process per call, so an in-memory store
loses its contents between calls. `SqliteStore` is therefore the recommended
production backend: it persists to `~/.tokenless/stash.db` so a `retrieve` in
one process can read what a `compress` in another process wrote.

Both backends enforce:

- **TTL**: entries expire after a fixed lifetime (InMemory 5 min; SQLite 1 h).
  An hour comfortably covers a typical agent session's compress→retrieve
  round trip. Expiry is enforced **on read** — `retrieve()` filters out
  expired rows (SQLite `WHERE expires_at >= now`) and `len()` counts only
  live entries, so expired data is never returned. The rows themselves
  remain on disk until either capacity-based FIFO eviction (triggered by
  `stash()`) or an explicit `evict_expired()` call (available for bulk
  cleanup but not called automatically), so the SQLite file can grow
  beyond the capacity before a `stash()` triggers a trim.
- **Capacity** (FIFO): once the live entry count exceeds the limit (InMemory
  1000; SQLite 10 000), the oldest entries are evicted. This prevents
  unbounded growth from runaway compression.

SQLite allocates ownership tokens and performs the live-row check, `created`
decision, upsert, and capacity enforcement in one `BEGIN IMMEDIATE`
transaction. A singleton `stash_metadata` row persists the generation
high-water mark across row deletion, expiry, lazy purge, and eviction; opening
older databases migrates the generation column and repairs that high-water
mark from the existing rows. InMemory keeps the equivalent high-water counter
under its store lock. Both backends fail without changing stash state when the
signed SQLite generation limit is exhausted.

## CLI

```bash
# Compress with stash on by default — dropped array items become retrievable.
python3 -c 'import json; print(json.dumps(list(range(200))))' \
  | tokenless compress-response --truncate-arrays-at 5

# Retrieve the original dropped items (same stash db, separate process).
tokenless retrieve c30ccf5ed1125e0ed871ba8e

# Historical marker syntax is still accepted.
tokenless retrieve '<<tokenless:c30ccf5ed1125e0ed871ba8e>>'

# Opt out of stash (lossy truncation, the pre-stash behavior).
python3 -c 'import json; print(json.dumps(list(range(200))))' \
  | tokenless compress-response --no-stash

# Override the stash db path under the home or selected data directory.
tokenless retrieve c30ccf5ed1125e0ed871ba8e --stash-db ~/.tokenless/alt-stash.db
```

`TOKENLESS_DATA_DIR` relocates both SQLite databases, producing
`$TOKENLESS_DATA_DIR/stash.db` and `$TOKENLESS_DATA_DIR/stats.db`.
`TOKENLESS_STASH_DB` mirrors `TOKENLESS_STATS_DB` as a higher-priority
single-file override.

## Security model

`TOKENLESS_DATA_DIR` is an explicit directory-level trust decision and may
point outside the real home, including to a managed service directory. It must
be absolute, cannot be filesystem root or contain parent traversal, and its
nearest existing ancestor is canonicalized before use. An invalid explicit
directory disables SQLite state for that operation instead of silently moving
it back under home.

File-level overrides (`--stash-db`, `TOKENLESS_STASH_DB`, and
`TOKENLESS_STATS_DB`) remain confined to the canonical real home — derived
from `getpwuid_r(getuid())`, never `$HOME` — or the selected data directory.
Existing database files must be regular files rather than symlinks. The CLI
and bundled RTK writer use the same path policy.

`retrieve` queries are parameterized SQL. Invalid input fails before a Stash
read. AgentScope retrieval also rejects an unauthorized hash before the read
and does not record that attempt as a Hit or Miss. Marker-command recovery does
not add a second authorization layer: it deliberately uses the same trusted,
same-user CLI boundary as a direct local invocation. The 96-bit content hash is
the unguessable handle, and the adapter's strict command classification exists
to prevent recovered output from being compressed again.

## Fail-open policy

- **Direct compression commands**: `compress-response --no-stash` may emit an
  explicitly unrecoverable truncation. The low-level API retains the same
  opt-in behavior.
- **Protocol v2 lifecycle path**: no truncation is applied unless every removed
  payload is recoverable. Missing state produces
  `recoverability_unavailable`; an actual Stash write error fails the
  operation instead of emitting an invalid recovery promise.
- **Retrieve path**: retrieve is user-initiated, so failures surface as
  errors (exit 1) rather than being swallowed.

## What is not stashed

`JsonCompressor` stashes complete record arrays, complete long strings, dropped
non-record array windows, and depth-truncated subtrees when a trusted recovery
path is available. Structural
cleanup—blacklisted diagnostic fields, `null`, and empty values—is classified
as lossless and does not emit recovery markers.

The former stateless MCP Retrieve server was removed. It accepted hashes
without verified model-visibility context, so it cannot serve as an authorized
Retrieve Tool. AgentScope uses the Marker-authorized lifecycle path. Supported
local CLI adapters expose the existing `tokenless retrieve` command indirectly
through the Marker and their ordinary shell tool; this does not change the
CLI's same-user trust boundary.

Schema description truncation **is** stashed when a store is attached (CLI
default): `SchemaCompressor::truncate_description` writes the verbatim
original and appends the declared recovery instruction. It stays lossy only when
stash is off or the stash write fails.

## Mapping to Headroom CCR

| Headroom | Tokenless | Notes |
|---|---|---|
| CCR Store | stash store (`StashStore` trait) | InMemory / SQLite(WAL) / Redis* |
| Recovery reference | Optional shell or static Tool instruction | 24-hex BLAKE3; historical Tokenless markers remain readable |
| `headroom_retrieve` (MCP) | AgentScope static Retrieve Tool or Marker-directed `tokenless retrieve` | AgentScope requires visible Markers; supported CLI agents use the same-user local command |
| DashMap `remove_if` TOCTOU fix | `BEGIN IMMEDIATE` ownership transaction | SQLite path |
| default TTL 5 min / cap 1000 | InMemory 5 min / 1000; SQLite 1 h / 10 000 | tuned for hook process model |

\* Redis backend is not yet implemented; it is tracked for the
multi-worker case (no `cfg`-gated scaffolding exists yet).
