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
database still requires access to its SQLite sidecar files. This library does
not provide an HTTP listener or credentials; those belong to the service layer.
