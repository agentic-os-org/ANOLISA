# Benchmark Result Reporting

[中文版](README_zh.md)

Generate two result reports from any non-empty subset of L1–L4 measurements.
This is an offline reader: it never launches compressors, Docker, model calls or
reference processes. Missing layers and unknown metrics remain unmeasured, not
zero or perfect. Results keep their original measurement identity; this tool
does not certify that old results describe a newer source revision.

## Requirements and entrypoint

Python 3.12 or later, standard library only. Run from this directory:

```bash
python3 -m reportgen --l1 <component-result.json> --out <report-directory>
python3 -m reportgen --l2 <measurements.json> --out <report-directory>
python3 -m reportgen --l3 <scenario-report.json> --out <report-directory>
python3 -m reportgen --l4 <collected-run-directory> --out <report-directory>
```

Combine flags for any subset and repeat a flag for additional rounds or inputs:

```bash
python3 -m reportgen --l1 <component-result.json> --l2 <measurements.json> --out <report-directory>
python3 -m reportgen --l2 <round-one.json> --l2 <round-two.json> --out <report-directory>
python3 -m reportgen --l3 <scenario-report.json> --llm l3=<model-results-directory> --out <report-directory>
```

Outputs are `tokenless-report_zh.md` (Tokenless and its necessary baseline) and
`comparison-report_zh.md` (both products when measured). Each includes measured
rows, strengths/limitations supported by those rows, plain-language definitions
and unavailable metrics. Reports are generated artifacts, not committed data.
Do not place the output directory inside an input run tree.

## Accepted input contracts

| Layer | Input | Boundaries |
|---|---|---|
| L1 | Current `compression_rate --json` canonical aggregates; optional row-based `component_cases`, `component_domains` or Headroom component-worker JSON | Legacy byte estimates are displayed only in a separate source-aggregate table, never reconstructed as measured rows, independent N or provider billing. Row-based candidates are not production acceptance. |
| L2 | Current `report.json` category aggregates, or row-based `measurements.json` schema 1/2 (`text_store` references in schema 2) | Aggregate-only input is not reconstructed into samples. Row inputs combine repeated timings per exact processing condition; distinct RTK commands stay separate. |
| L3 | `report.json` with scenarios and optional sibling `lifecycle.json` | Lifecycle supplies actual input and output identity and mode. Without it, model scores cannot be bound. Shared reference timing is labelled, not claimed separately for each pipeline. |
| L4 | Root containing `out/repeat-N/arm-X`, optional `eval/`; or collected host directories with that layout | Selection, current prediction digest and matching completion/report determine official solved counts. Missing scoring stays unscored; completed zero-prediction tasks stay in the denominator. |

The reporting PR works independently against the current aggregate producers;
row-based formats are optional and require the version of the producer that
emits them. No unpublished branch or external converter is required for
aggregate input. Missing row-level information stays unmeasured. L3 rates above
100% are refused; negative savings represent legitimate output expansion.

Statistical summaries count identical input bytes once per product, mode and
processing condition even when multiple sample aliases refer to them. Aliases
remain visible for provenance; different outputs are excluded from stable
summaries. PureStage facts and latency remain unmeasured when only Router
measurements exist. Timing descriptions retain their actual measurement bases.

Optional model-result directories contain `plan.json` and `stats.json` generated
by the evaluator. Only matching layer, row, product, mode and both original-input
and processed-output hashes are attached. A score for the same name but different input is not reused.
Incomplete answers do not become full scores. Reading these files is not a new
model run and needs no provider credentials.

L4 input/output/cache-read/request fields must be non-negative integers; null,
strings, booleans and known aggregate mismatches are not valid billing observations.
Official repair tests retain their FAIL_TO_PASS/PASS_TO_PASS meaning. A passed
patch does not prove compression was active, and the reader does not invent L4
semantic retention, compression savings, cloud prices or total billing.

## Privacy and verification

The reader never includes original text, raw model answers, host directories,
auth headers or arbitrary metadata. Samples, datasets and displayed fingerprints
use HMAC pseudonyms with a per-invocation random key; raw dataset names do not
appear in summaries. For stable labels across invocations, explicitly supply
`REPORTGEN_SALT` as a private environment value and retain it securely; it is
never logged or written to the reports. Pseudonyms are not a complete anonymity
guarantee. Mode/content/tokenizer descriptions use a controlled vocabulary.
Before any output is
written, the whole generated text is checked for addresses, private directories,
email and credential material. `--forbid <identifier>` adds operator-specific
names or identifiers to the output denylist; these values are not logged.
Pattern checks cannot recognize every possible person's name: the conservative
field selection and anonymized labels are required, not replaced by regexes.
Raw input files stay private and retain their original bytes for reproducibility.
`--verbose` sends tracebacks to private stderr for debugging; they may contain
input paths and must not be shared publicly. Normal validation errors identify
the layer and offending row/field without exposing source content. Unknown
restriction notes are visibly marked as an unregistered note, not silently lost.
L4 usage sums only selected instances and displays excluded-instance counts,
scoring status and its limitations.

```bash
python3 -m pytest -q tests
```

The offline suite covers all 15 non-empty layer combinations, current scoring
bindings, incomplete usage, repeated inputs, privacy refusal before output and
model-score input identity. No model or Docker services are required. The
component target `make test-benchmark-reporting REPORT_TEST_PYTHON=<venv-python>`
runs the same suite from the Tokenless directory. It requires pytest only for
testing, not for report generation.
