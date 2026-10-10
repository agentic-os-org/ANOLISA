# V1 PII compatibility fixtures

`v1.json` contains synthetic inputs and expected responses recorded while the
existing Python scanner regression suite passes, plus explicit token and email
boundary probes and JWT JSON/base64 compatibility cases (234 synthetic cases). It records the source commit,
SHA-256 of each Python PII source file, and Python version. Regeneration is an
explicit review operation; Rust tests never regenerate their expected values.

The generator also freezes Python 3.11.6 / Unicode 14.0.0 word and decimal ranges
in `src/python_unicode.rs`. An exhaustive Rust check compares regex classes,
manual word boundaries and decimal values for every Unicode scalar against the
Python classification hashes. This keeps newer Unicode assignments and Python's
dotted/dotless-I case folding from changing builtin detection boundaries.

From the repository root, with Python 3.11.6 and the component test dependencies:

```sh
python src/agent-sec-core/tests/v2/fixtures/generate_pii_v1.py
```

The corpus excludes inputs larger than 64 KiB; dedicated Rust tests cover large
inputs and byte limits. Comparisons ignore elapsed time, additive V2 evidence metadata and the explicitly
versioned engine name (`regex_v1` to `regex_v2`). Version 2.0.0 detection improvements
have separate positive/negative cases in `../detection_quality.rs`; this V1 oracle
is not regenerated to hide intentional differences. Custom rule behavior is validated separately using isolated rule sets.

The email sentence-boundary repair adds 92 cases from the maintained V1 scanner;
all 142 previously frozen cases retain their expected responses. These additions
cover punctuation, invalid suffixes, Unicode spans, redaction, and confidence.
Rust also checks these boundaries independently in `../detection_quality.rs`.

The source hashes remain the reproducible oracle identity after implementation-branch
rebases; `source_commit` records the original capture checkout, not the final PR tip.

`credit_cards.json` is a separate, explicit card-structure corpus shared by the retained
Python detector and Rust 2.0.1. It includes synthetic network/format boundaries and
the original 214-byte OpenClaw and 840-byte Cosh-NG benign tool outputs, with input
SHA-256 identities. Its expected findings and redactions describe the new contract;
adding or changing these cases does not regenerate the historical `v1.json` oracle.
