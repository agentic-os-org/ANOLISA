# Incremental public audit reads

[中文版](../../zh/runtime/audit-export.md)

`aw audit-export` exposes retained AW execution metadata to local consumers such
as observation integrations. `aw-service::audit_export::query` provides the same
bounded, read-only operation to Rust callers. Both reuse the existing FileJournal
and [public objects](public-objects.md); neither starts a service nor acquires
execution ownership. [CLI automation](../../../../../../docs/developer-guide/en/aw/reference/cli.md#public-audit-export)
contains invocation and request examples.

## Ownership and access

The CLI opens the selected service state's `journal/` directory. The directory
must belong to the effective UID and have mode 0700. The reader pins its file
descriptor, opens children without following symlinks, and requires same-UID,
mode-0600 regular files with one link. Consumers run as the source owner; there
is no cross-user delegation or access to the service control socket. This is a
local same-user trust boundary, with no protection from a trusted writer that
can rewrite all source files and retained consumer evidence.

`aw-core::journal::reader` owns storage validation, inventory and source-position
paging. `aw-service` validates the retained source identity and projects a field
whitelist through `public_projection`. Export envelopes use the separately
registered `audit-export.aw/v1alpha1` request and response schemas. Existing
private envelopes, public object schemas and control RPC versions retain their
bytes and meanings. Private plans, intermediate records, tool bodies and storage
error strings are excluded from responses.

The producer creates `.aw-audit-source.json` once, with a random `sourceId`.
This identity survives service restarts and is the public producer's `scopeId`.
Each event retains the writer's `generation`. Export never substitutes the
reader's lifetime, current time or directory pathname. Missing/invalid source
metadata fails the read; export does not create or repair it. Moving an intact
retained directory preserves source identity; replacing it with a new source
invalidates old cursors. Copying history preserves identity and therefore requires
consumers to detect conflicting bytes under the same public IDs.

## Inventory and paging

An inventory scan collects at most 10,000 directory entries, validates journal
key names and sorts the keys. Its digest covers the whole key set. The continuation
contains `source_id`, `inventory_digest`, the last returned `after` key and the
first page's highest key as `through`. Each page contains at most 64 keys. New or
removed keys set the response's `changed` flag while paging continues after the
last key; deleting that key does not reset progress. Keys above `through` wait
for the next scan. New keys below `after` also appear in the next scan. This keeps
existing later keys reachable under concurrent creation and deletion.

Directory enumeration observes a live directory rather than a transactional
snapshot; changes during a scan may appear on a later scan. Consumers periodically
start from the beginning, retain per-chain cursors, and bound their own page loop
and rescan schedule. The 10,000-entry inventory cap is a current storage-size
limit on each request, not a retention policy or an index for larger installations.

Enumeration includes legacy/private keys. Reading such a chain returns
`unsupported_history`; it never manufactures an ExecutionEvent. A changed key
set says nothing about a known chain's append position. Consumers retain separate
inventory and chain checkpoints and bound their own rescan scheduling.

A chain request carries `chain_id`, `limit`, and a nullable cursor. Its cursor
binds the source and chain to both `consumed` and `observed_tip`, each containing
`sequence` and `digest`. Every read verifies the entire captured chain from its
reservation before returning a page. Limits are 8 MiB and 4096 source envelopes
per chain, 64 source positions per page, 16 KiB per request and 1 MiB per response.
The exported limits are fixed for this protocol version. Limit failures require
operator review; the reader does not silently truncate or delete history.

Each successful page includes the immutable ExecutionEvent, projected AuditRecords,
the captured `tip`, a reusable cursor, `caught_up` and `terminal`. Private source
positions advance `consumed` even when `records` is empty. Public IDs, timestamps,
references and source digests remain identical across replay. Consumers deduplicate
by producer identity and UID, compare object digests, and atomically persist data
with their checkpoint.

`terminal` describes the captured chain tip, so it may already be true on an
early page. Finish a chain only when **both `caught_up` and `terminal` are true**.
If `caught_up` is true and `terminal` is false, retain the cursor and revisit it:
a later append must be read even if enumeration already passed this key. An open
chain from an ended generation remains nonterminal/incomplete history; no read
infers a successful outcome or recreates its execution handle.

## Integrity, gaps and cancellation

The reader captures the file's size when opening it and verifies that prefix.
Appends beyond that size wait for the next visit; a partial final line returns
`history_incomplete`. Reading fewer bytes after concurrent truncation returns
`history_changed`. Each complete envelope must satisfy canonical parsing,
sequence continuity and the linked digest. A record after a closed tip is corrupt.
A new visit checks retained consumed and observed-tip digests, exposing rewritten
or removed suffixes, including a suffix not yet delivered in a smaller page.
Without an independently retained tip, a syntactically valid shortened prefix
cannot prove that a suffix once existed.

| Error | Consumer action |
| --- | --- |
| `source_changed` | Keep old source checkpoints separately; discover the new source |
| `history_missing`, `history_changed` | Record a history gap and reconcile retained evidence |
| `history_corrupt`, `history_incomplete`, `projection_invalid` | Keep the checkpoint; report damage/incompleteness and retry only under an explicit recovery policy |
| `projection_unavailable` | Record a known producer metadata/observation-limit gap; do not infer a public event |
| `unsupported_history` | Record unsupported legacy/private history; do not label it complete coverage |
| `source_missing`, `source_invalid`, `access_denied` | Correct the source selection or authorized storage setup |
| `read_limit`, `response_limit` | Report the limit; do not skip records as if consumed |
| `cancelled`, `storage_unavailable` | Retain the previous checkpoint and retry a read within consumer policy |

Missing bytes are reported as missing or changed history. The exporter has no
retention writer and cannot attribute deletion to a retention policy. It never
automatically deletes Journal entries. Read failures do not poison the writer's
Journal or change critical synchronous audit acknowledgement. No export retry
replays an AW action. Cancellation and deadlines are checked between bounded
reads and projection steps; local filesystem calls remain cooperative rather
than hard real-time. The CLI exits after one request and owns no background resources.
