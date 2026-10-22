# CSV/TSV compression

[中文版](tabular-compression_zh.md)

## Contract

`TabularCompressor` consumes rectangular, header-first CSV or TSV directly.
The first record and at least two data records must identify a unique delimiter.
All cells remain strings; duplicate or empty headers, empty cells, row order,
and column order survive full compaction. No JSON compaction, numeric conversion,
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

## Candidate selection

1. Render all records with necessary quoting and LF record separators, without
   appending a final record separator. Removing an existing final separator can
   be the only change; it is adopted only when the estimated token count also drops.
2. Compare the complete candidate against the original input. Apply it only if
   both the character count and estimated token count decrease; otherwise return
   the original unchanged. There is no percentage savings gate.
3. Retain every data row, including duplicate rows and ordinary rows without
   diagnostic keywords. Table size and recovery availability do not enable
   sampling, omission, constant-column extraction or dictionary encoding.

## Runtime and compatibility

Runtime uses `TabularCompressor::compress`. The compatibility method
`compress_with_recovery` delegates to that same full-table path and ignores its
storage and recovery arguments. The outcome is always `Lossless`, with equal
original/retained row counts for parsed tables, no Stash writes and zero storage
errors. Cell equivalence does not imply byte-identical source formatting or
unchanged model reading accuracy.

Runtime owns final savings arbitration, dry-run and timeout rejection. Dry-run
and timeout return the original text. Tabular dispatch requires a successful
result and both output-replacement and arbitrary-text capabilities. Existing
file-origin, RTK and Retrieve bypasses continue to apply; JSON-only replacement
slots do not accept table views.

Applied full-table compaction reports content type `tabular` and operation
`tabular_compaction`. The Rust operation variant, protocol wire value
`tabular_row_reduction` and Python SDK enum remain available for compatibility
with historical records, but current table compression never emits them.
Previously stored originals remain retrievable under the existing authorization
and expiry rules. No configuration, command or storage-schema change is required.

## Validation

Compressor tests independently parse outputs and compare every cell and its
position. They cover quoting, embedded line endings, BOM handling, duplicate
headers, empty cells, malformed input, large tables and small savings.
Recovery-capability variations and a store that rejects every access verify the
compatibility entry point has no storage dependency. Runtime tests cover final
arbitration, capabilities, dry-run, timeout and zero Stash writes. Goldens retain
the former row-reduction inputs as complete-table regressions. Hook parity,
installed-wheel tests and installed-package checks verify complete CSV/TSV output
without recovery instructions. Reference fixtures establish behavior, not a
production savings guarantee.
