# Snapshot list pagination

[中文版](list-pagination_zh.md)

`Request::ListPage { orphans_only: bool, workspace: Option<String>, limit: u32, cursor:
Option<String> }` and `Response::ListPageOk { snapshots, next_cursor }` are
appended to the bincode enums. Existing discriminants and the exact layouts of
legacy `List`, `ListOrphans`, and `ListOk` remain unchanged. `ListOrphans`
keeps request discriminant 28; the new `ListPage` is appended at 29. A current CLI and daemon are
required for cursor paging; legacy unpaged protocol messages remain decodable.

The daemon sorts by `(created_at, workspace_id, snapshot_id)` ascending. The
opaque, versioned cursor is bound to the resolved workspace scope and orphan
filter (`list --orphans`), and carries
the continuation key plus the maximum key observed on the first page.
The upper bound does not depend on the wall clock; preexisting future-dated
snapshots remain visible after clock corrections. A malformed, unsupported,
oversized, or cross-scope cursor is rejected. Entries newer than the first-page
upper bound are excluded. Inserts at or below that bound and deletions can still
change later pages, so this is a monotonic traversal rather than a transactional
snapshot.

Requests are capped at 10,000 entries. Each query retains only the smallest
`limit + 1` candidate keys after the cursor, then reads and clones the selected
entries. Filtering and comparison borrow index fields; only admitted candidate
keys and the observed upper bound are copied. This avoids cloning and sorting
the full metadata set. Pages target
about 1 MiB and are checked against the 16 MiB IPC payload limit before they are
sent. Skipped deleted/recreated candidates cannot change a cursor already
budgeted with an accepted item. Empty pages may advance past skipped keys;
the final item/cursor combination is checked again before returning.

If one entry's optional fields would exceed the hard frame limit, the daemon
returns a summary item preserving `id`, workspace, `created_at`, `pinned`, and
`missing`. CLI JSON keeps these three fields under `meta`, exactly as in full
entries, and marks the outer object with `detail: "summary"` and `omitted_fields`.
Omitted optional fields are absent from `meta`, not synthesized as null values.
The snapshot remains addressable by ID. This fallback is explicit and does not
silently truncate a full entry.

Without `--limit` or `--cursor`, the CLI follows all continuation cursors and
preserves the historical output contract: JSON is one array and table output is
one complete listing. It buffers aggregate JSON and emits nothing if a later
page fails. Explicit-page JSON is an object containing `snapshots` and
`next_cursor`; table output labels summaries and prints the next cursor.

The daemon reads the in-memory snapshot index and does not scan snapshot file
contents. Each workspace maintains a runtime ordered set of identity keys, so
pages seek directly past the continuation key and stop once later keys cannot
improve the bounded candidate set. Registration and recovery rebuild the order;
ordinary inserts/deletes update it incrementally, while whole-index replacement
compares old and new membership and updates changed keys. This adds O(N) resident
identity storage but does not change the persisted index or cursor format.
Sparse orphan-only queries can still inspect non-orphan keys in their remaining
range, including when finding the first page's upper bound. No fixed latency is
guaranteed. Regression coverage
uses a 100,000-entry synthetic index and covers frame overflow, near-16-MiB
metadata, malformed/scope-mismatched cursors, concurrent inserts/deletes,
summary propagation, and later-page aggregate failure. Ordered traversal tests
also compare complete results with an independently sorted index across tied
timestamps, workspace scopes, orphan filters and live index replacement.

Page telemetry uses `ops_name: "list_page"`, `list_time: 0`, and `ops_time: 1`
for every request, including failures and continuation pages. Legacy `List` / `ListOrphans`
keep `ops_name: "list"` and `list_time: 1`. These are daemon request results;
there is no query-wide correlation or final CLI outcome event, so page logs
must not be used to calculate whole-query counts or success rates.

## Hermes tool pagination

The existing `ws-ckpt-list` tool opts into one-page JSON output when either
`limit` or `cursor` is present. The handler distinguishes missing keys from null,
rejects invalid limit types/ranges and empty/non-string cursors before invoking
the CLI, and passes opaque cursors unchanged. Cursor-only requests omit the CLI
limit flag, retaining its default of 1000. Without either key, the original table
command and `{success, output}` response remain unchanged.

A page must be an object with a `snapshots` array of objects and an explicit
`next_cursor` that is null or a non-empty string. The handler emits a JSON string
with `success`, `snapshots`, and `next_cursor`; extra envelope fields are ignored.
Snapshot objects retain all nested fields, including summary markers and future
extensions. Only a null cursor ends traversal; the handler never infers completion
from page length, follows cursors automatically, or retries failures as a full
listing. It does not modify configuration or checkpoint/rollback state.

The tool description recommends a bounded first page and on-demand continuation.
Validation covers the registered schema, exact CLI arguments and one-call
behavior, invalid arguments, malformed responses, short/empty continuation pages,
summary preservation, and subprocess failures. OpenClaw and the Rust protocol
remain unchanged; pagination still requires a capable CLI and matching daemon.


### Installed Hermes acceptance

`tests/hermes_pagination_smoke.py` runs through the installed Hermes plugin
loader and tool registry, using the normal plugin installer in a temporary
Hermes profile. Run it with the Python environment belonging to that Hermes
installation. A dedicated daemon must already be running, with two registered
Btrfs workspaces and at least five real snapshots in the first workspace.
From the repository root:

```bash
python3 src/ws-ckpt/tests/hermes_pagination_smoke.py \
  --socket /tmp/ws-ckpt-pagination.sock \
  --workspace /tmp/ws-ckpt-pagination-a \
  --other-workspace /tmp/ws-ckpt-pagination-b
```

The script compares native first/continuation pages with the full CLI result,
checks default table output and cursor-only continuation, and requires explicit
errors for invalid arguments, cross-workspace cursors and an unavailable socket.
It checks that the snapshot listing is unchanged afterward. It creates no
workspaces or snapshots and makes no model calls. This is a manual acceptance
harness, not evidence that a Btrfs or interactive Hermes run has passed.
