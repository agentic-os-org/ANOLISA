# Read-only statistics queries

[中文版](stats-query_zh.md)

`tokenless_stats::reader::StatsReader` opens an existing `stats.db` without
creating tables, migrating the schema, or changing producer behavior. It is
a query entry point for integrations that need the original recorded data.

## Contract

- `list(RecordQuery)` returns metadata ordered by descending record ID. The
  default limit is 50, with a maximum of 100. Continue a page using
  `next_before_id` and the same session/tool-call filters.
- Record IDs belong to the current database. Page bounds do not provide
  recovery after clearing or replacing that database.
- `get(id, include_content)` returns one record or `None`. Content is excluded
  unless requested; `content_included` distinguishes omission from null stored
  content. Requested text and output are returned without truncation.
- Original RFC3339 timestamps, operation strings, mode strings, and optional
  attribution are preserved. Missing historical fields are null. Unknown modes
  are not converted to Active. Invalid timestamps produce errors, not current time.
- Required columns are the ID, timestamp, operation, agent ID, and before/after
  character and token counts. Optional fields missing from older layouts,
  including the bundled RTK layout, are null. Filtering on an absent attribution
  column produces an explicit unsupported-layout error.
- `applied_operations` preserves the stored JSON text. Counts remain estimates;
  `tokenizer_id` identifies the counter when the producer stored it.
- Missing databases, unsupported layouts, corrupt rows, and SQLite read failures
  are errors. No row is silently skipped to return a successful partial page.

The reader uses SQLite read-only access with a 500 ms busy timeout. A live WAL
database still requires access to its SQLite sidecar files. HTTP and credentials
belong to the optional `query-server` service layer.

`list_for_tools` and authenticated `POST /v1/stats/records/query` match any of up to
128 exact tool IDs using a bound JSON array. The POST JSON includes `tool_use_ids`,
`limit` (1–100) and optional `before_id`; its body is limited to 1 MiB. IDs are nonempty
and at most 1024 bytes each. Pages are metadata only, ordered by source ID, and use
the same exclusive cursor as GET queries. Duplicate IDs do not duplicate rows.

## Query cost and source upgrades

Filters are assembled only when present, with all values bound. Session queries
use the producer's session indexes, ID cursors use the primary key, and tool
queries use `idx_tool_use_id`. The native recorder adds that index when it next
opens an existing database; new databases include it from initialization.
The reader never creates an index or migrates a source. Older or RTK-only sources
without this index remain readable but tool matching can scan the table until
the upgraded native recorder opens that database. Keep the source and AgentSight
query budgets in mind during this transition; no latency bound is promised.
The HTTP service warns once at startup if `idx_tool_use_id` is absent, without
creating the index or preventing startup.
This change does not introduce automatic source retention or delete source data.

## Invalid-record diagnostics

A corrupt matching row still fails the whole page. HTTP 500 `invalid_record`
includes `record_id` (null if the ID itself is not an integer) and `field`, never
the invalid value or stored content. Inspect and repair the identified source
record using the producer's operational procedure, then retry the same query.
For manual diagnosis, `before_id=<record_id>` reads older records without that
row; this is a deliberately narrower query, not a complete result or automatic
recovery. AgentSight keeps failing closed rather than calculating partial totals.

The HTTP service authenticates before reading a batch request body, which remains
limited to 1 MiB. After authentication, unknown paths (including trailing slashes)
return 404 before method checks. Both GET routes also support HEAD with the same
query validation and status, but no response body. Method errors return 405 with
`Allow: GET, HEAD` or `Allow: POST`. Startup warns on non-loopback
plaintext listeners and Unix credential files with group/other permissions; these
warnings preserve existing startup behavior. Use mode 0600 for credential files
and a private network or TLS proxy for remote access.
