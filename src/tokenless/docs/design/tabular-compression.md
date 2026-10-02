# CSV/TSV compression

[中文版](tabular-compression_zh.md)

## Contract

`TabularCompressor` consumes rectangular, header-first CSV or TSV directly.
The first record and at least two data records must identify a unique delimiter.
All cells remain strings; duplicate or empty headers, empty cells, row order,
and column order survive full compaction. No JSON cleanup, numeric conversion,
TOON encoding, or constant-column extraction participates.

Unnecessary quoting and record terminators can be normalized. Embedded field
line endings and whitespace remain data. Blank physical lines outside quoted
fields are layout, not records; delimiter-only records contain empty cells and
are retained. A full view is cell-equivalent, not a byte-exact copy of its source.

Malformed quoting, unequal record widths, ambiguous delimiters, single-column
text, Markdown and fixed-width tables are not compressed. Detection scans at
most 64 KiB and ten logical records. The selected compressor validates the full
input before transforming it; the CSV library's permissive quote parsing is
preceded by strict lexical validation at this boundary.

Row reduction additionally requires column-label evidence: every nonempty header
starts with a Unicode letter or `_`, followed by letters, numbers, `_`, `-` or
`.`; at least one label is nonempty. Duplicate and empty labels are allowed.
Headers with spaces, expressions or sentence punctuation keep all rows. This
conservative gate excludes repetitive function calls and comma-separated prose
from reduction, at the cost of skipping some genuine tables. It is a heuristic,
not proof that arbitrary delimiter-separated text has a semantic header.

## Candidate selection

1. Render all records with necessary quoting and LF record separators.
2. Compare against the original input, including all model-visible overhead.
   Full compaction wins immediately when it saves at least 15% of estimated
   tokens and reduces characters. Smaller full-data savings remain a candidate.
3. For more than 32 data rows with column-label evidence and working recovery, retain the first
   and last four rows plus diagnostic rows. Fill the remaining base budget of
   32 with evenly spaced ordinary rows. Protected rows can exceed that budget;
   retained rows are emitted in source order.
4. Diagnostic signals are the English whole words `error`, `failed`, `failure`,
   `fatal`, `panic`, `exception`, `warn`, `warning` (case-insensitive), or the
   Chinese substrings `错误`, `失败`, `警告`. These heuristics do not guarantee
   that all task-relevant rows are retained.
5. A reduced view must beat both the original and the full-data candidate in
   estimated tokens and characters. Its external notice reports retained/total
   data rows, one-based source row ranges excluding the header, and recovery.
   If the exact range list exceeds 1 KiB, discard the reduced candidate before
   rendering or storing it; keep the full-data candidate or original instead.
   Diagnostic rows and source ranges are never partially reported.
   Complete enumeration and calculations require the original table.

## Recovery and Runtime

Every omission stores the complete original CSV/TSV text in the existing Stash.
Recovery returns its original bytes, including BOM, quoting and record endings,
while that entry remains available and authorized. Storage failure or missing
recovery permits only full-data compaction or the original input.

Runtime owns final arbitration and the Stash ledger. Dry-run uses temporary
storage and returns the original; timeout rejection rolls back tentative writes.
Tabular dispatch requires a successful tool result and both output-replacement
and arbitrary-text capabilities. Existing file-origin, RTK and Retrieve bypasses
continue to apply. JSON-only replacement slots do not accept table views.

The protocol reports content type `tabular` and operations `tabular_compaction`
or `tabular_row_reduction`; the Python SDK exposes matching enum values.
Row reduction includes the same format normalization as compaction, so it reports
only `tabular_row_reduction`. Full views use the existing `lossless` state for cell equivalence; reduced views use
`retrievable`. No configuration, command or storage-schema change is required.

## Validation

Compressor tests parse rendered views independently and compare cell sequences,
exercise quote/delimiter boundaries, and verify byte-exact original retrieval.
Runtime tests cover arbitration, capabilities, dry-run and timeout rollback.
Hook parity and installed-wheel tests exercise the production entry points;
installed-package regression includes full, CSV-reduced and TSV-reduced views.
Reference fixtures establish behavior, not a production savings guarantee.
