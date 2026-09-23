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
the continuation key plus the maximum key observed in the first index scan.
The upper bound does not depend on the wall clock; preexisting future-dated
snapshots remain visible after clock corrections. A malformed, unsupported,
oversized, or cross-scope cursor is rejected. Entries newer than the first-page
upper bound are excluded. Inserts at or below that bound and deletions can still
change later pages, so this is a monotonic traversal rather than a transactional
snapshot.

Requests are capped at 10,000 entries. Each scan retains only the smallest
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
contents. Each page still scans the full query scope: N snapshots over P pages
require O(N * P) index visits, plus candidate maintenance and serialization.
Reducing allocations does not remove this repeated scan; an ordered index would
be needed to seek directly to a continuation key. No fixed latency is guaranteed. Regression coverage
uses a 100,000-entry synthetic index and covers frame overflow, near-16-MiB
metadata, malformed/scope-mismatched cursors, concurrent inserts/deletes,
summary propagation, and later-page aggregate failure.

Page telemetry uses `ops_name: "list_page"`, `list_time: 0`, and `ops_time: 1`
for every request, including failures and continuation pages. Legacy `List` / `ListOrphans`
keep `ops_name: "list"` and `list_time: 1`. These are daemon request results;
there is no query-wide correlation or final CLI outcome event, so page logs
must not be used to calculate whole-query counts or success rates.
