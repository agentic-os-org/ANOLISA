# CTest regression corpus

[中文版](README_zh.md)

These fixtures validate CTest BuildLog changes based on ANOLISA `5768780b0e4887949765d53314070d7935012351` (Tokenless 0.9.5). They are source-change evidence, not a release or live-Agent validation. `provenance.json` records pinned project commits, the immutable Docker image ID (`tokenless-cpp-capture:ubuntu24.04`, `sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`), CMake/CTest/Ninja/GCC versions, sanitized container commands, exit status, and SHA-256 values. All six fixtures are complete unmodified `observed.txt` streams.

## Reproduce captures

From `src/tokenless`, use Docker with a prepared `tokenless-cpp-capture:ubuntu24.04` toolchain image (Ubuntu 24.04 with `cmake` 3.28.3, `ninja` 1.11.1, and GCC 13.3.0) and Python 3.11 or later. Each run creates an unmodified pinned source checkout and builds with CMake + Ninja inside `/work`, then executes `ctest_serial` (`ctest --test-dir /work/build-ninja --output-on-failure`) and `ctest_parallel` (`ctest --test-dir /work/build-ninja -j 2 --output-on-failure`) under `--network none --cpus 2`, `TZ=UTC`, and `LANG=C.UTF-8` without modifying any CMake definition. Pass `--sources-dir <dir>` to reuse offline git clones at `<dir>/<project>`; the script fails closed if a project clone, tag, or pinned commit is missing or mismatched.

```bash
python3 tests/capture_ctest.py --project brotli --output /tmp/tokenless-ctest-captures/brotli
python3 tests/capture_ctest.py --project catch2 --output /tmp/tokenless-ctest-captures/catch2
python3 tests/capture_ctest.py --project fmt --output /tmp/tokenless-ctest-captures/fmt
python3 tests/capture_ctest.py --project libgit2 --output /tmp/tokenless-ctest-captures/libgit2
```

`libgit2_offline_failure.txt` retains a complete failure stream with exit code `8` (four failing offline `online::customcert` checks plus `The following tests FAILED:` summary diagnostics), while still compacting its eight trailing passing test suites losslessly without Stash. No failed result is presented as a passing run. The script automatically sanitizes JSON metadata (`src=/work-host/work`, omitting host paths and proxy endpoints).

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test ctest
cargo test --locked -p tokenless-compressors --lib build_log::ctest -- --nocapture
CTEST_CAPTURE_ROOT=/tmp/tokenless-ctest-captures cargo test --locked -p tokenless-compressors --test ctest -- --nocapture
```

The optional `CTEST_CAPTURE_ROOT` sweep scans `stdout.txt`, `stderr.txt`, and `observed.txt` across `ctest_serial` and `ctest_parallel` for `brotli`, `catch2`, `fmt`, and `libgit2` (24 streams total). Both committed fixtures and all 24 captured streams pass independent [ctest_oracle.rs](../../../common/ctest_oracle.rs) reconstruction (removing only independently specified ANSI SGR escapes for comparison and keeping Markdown-fenced regions verbatim), idempotence, zero-Stash, and strict character + heuristic-token reduction assertions (`stderr.txt` streams stay untouched). Unit tests verify each edit independently.

Boundary mutations insert `***Failed`, `***Timeout`, `***Exception: SegFault`, `***Not Run`, assertion failures, `\r` overwrites, `\x1b[0K`, `\t`-indented `The following tests FAILED:` lines, quoted Markdown examples, and unclassified stdout lines at every line boundary of synthetic blocks and a real `brotli_parallel.txt` excerpt (330 synthetic mutations in unit tests plus 216 excerpt mutations in integration tests). Additional tests cover single-line/prose rejection, CRLF-only rejection, Markdown fences across head/tail sample gaps and inside otherwise valid runs, `UpdateCTestConfiguration` / `Site:` / `BuildName:` preambles, `4 KiB` (`4096`-byte) per-line local scope close versus `64 KiB` whole-result suppression, `256`-row / `64 KiB` block splitting, `256`-edit capping, `> 4 KiB` generated-header idempotence, anchored `Generic` fallback for uneditable logs, and keeping CTest `Start`/result lines out of `Generic` progress rules.

Two reviewed golden outputs freeze the compact representation (`brotli_serial.expected.txt` and `brotli_parallel.expected.txt`). Regenerate only after an intentional output-format change, then inspect the diff:

```bash
cargo test --locked -p tokenless-compressors --test ctest regenerate_ctest_goldens -- --ignored --exact
```

Committed fixture estimates (heuristic-v1):

| Fixture | Input tokens | SGR-cleaned | Candidate tokens | Saving vs input |
| --- | ---: | ---: | ---: | ---: |
| `brotli_serial.txt` | 2,984 | 2,984 | 680 | 77.21% |
| `brotli_parallel.txt` | 2,984 | 2,984 | 773 | 74.10% |
| `catch2_serial.txt` | 3,164 | 3,164 | 916 | 71.05% |
| `catch2_parallel.txt` | 3,164 | 3,164 | 942 | 70.23% |
| `fmt_serial.txt` | 572 | 572 | 217 | 62.06% |
| `libgit2_offline_failure.txt` | 5,344 | 5,344 | 5,245 | 1.85% |

Failed-command behavior inside the compressor preserves failure diagnostics, test status lines, and summary footers verbatim; Runtime continues passing `Tool Error` responses through unchanged. Runtime, CLI, and common Hook tests verify routing, metadata preservation, and host guards. No live Agent invocation was performed.
