# Tracing conformance inputs

`v1-trace-context-normalization.json` was captured by executing the stdlib-only functions
`parse_trace_context_payload` and `trace_context_to_payload` from
`agent-sec-cli/src/agent_sec_cli/correlation_context.py`. It freezes the six input
fields, aliases, Python whitespace and 256-code-point truncation behavior.
The Rust test reads these goldens without Python in the product runtime.

`metadata.json` contains 46 shared inputs, each evaluated against the original
Pydantic `ObservabilityMetadata`, `ModelCallMetadata` and `ToolCallMetadata`
schemas (138 schema checks). Each `expected` entry is keyed by `agent`, `model`
or `tool`: an object is the normalized output; `null` means validation must fail.
Regenerate from the component directory with
`agent-sec-cli/.venv/bin/python v2/fixtures/tracing/capture_metadata.py`.
It covers required fields, omitted/null optionals, invalid types, alias precedence,
ignored extras, empty/whitespace strings and Unicode truncation. Rust tests bind
every case against an already populated parent to detect accidental inheritance.
The generator is development-only; Rust conformance reads the frozen JSON.

Native wire schema and byte-budget cases live in
`asc-daemon-protocol/tests/tracing.rs`; real process inputs and local correlation
log assertions live in `tests/v2/e2e/test_otel_e2e.py`. Existing PAP and CLI goldens
remain the business compatibility oracle; tracing carriers are validated separately
before comparing the captured business envelope.

See `docs/design/V2_OTEL_ACCEPTANCE_zh.md` for commands and coverage boundaries.
