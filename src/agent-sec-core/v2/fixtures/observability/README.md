# V1 observability record oracle

`v1-records.json` was captured from the Python V1 checkout at `c7d3e5888`, using
`agent_sec_cli.observability.schema.validate_observability_record(input).to_record()`.
The six cases cover every hook, filtering unknown fields, hook-specific optional
metadata, non-ASCII/reserved characters, significant whitespace and timestamp formatting.
Dates are fixed in 2030 so shutdown retention does not delete the restart-test records.

The CLI Rust tests compare the params and decoded OTel baggage to this oracle.
`tests/v2/e2e/test_observability_record_e2e.py` compares real daemon JSONL and SQLite
rows to the same records and reopens them after restart. No V1 runtime is needed.
This is bounded differential evidence, not exhaustive Pydantic input coercion parity.

`v1-timestamps.json` additionally freezes 24 Pydantic timestamp acceptance and
normalization cases, including seconds/milliseconds, decimal strings, alternate
ISO formats, invalid ranges, and the distinct negative-float conversion path.
The domain parser and CLI tests consume this oracle.
