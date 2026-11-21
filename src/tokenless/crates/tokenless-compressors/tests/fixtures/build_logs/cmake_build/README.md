# CMake and Ninja build regression corpus

[中文版](README_zh.md)

These fixtures validate CMake Unix Makefiles and Ninja BuildLog changes based on ANOLISA `6fc582a4ecd4244707e2e14597d9665b5e09db9c` (Tokenless `0.9.6`). They are source-change evidence, not a release or live-Agent validation. `provenance.json` records pinned project commits, the immutable Docker image ID (`tokenless-cpp-capture:ubuntu24.04`, `sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`), CMake/Ninja/Make/GCC versions, sanitized container commands, exit status, and SHA-256 values. All eight fixtures are complete unmodified `observed.txt` streams.

## Reproduce captures

From `src/tokenless`, use Docker with a prepared `tokenless-cpp-capture:ubuntu24.04` toolchain image (Ubuntu 24.04 with `cmake` 3.28.3, `ninja` 1.11.1, `GNU Make` 4.3, and GCC 13.3.0) and Python 3.11 or later. Each run creates an unmodified pinned source checkout and captures `ninja_cold`, `ninja_incremental`, `make_cold` (`-j 2`), `make_incremental` (`-j 2`), and `failure_unknown_target` under `--network none --cpus 2`, `TZ=UTC`, and `LANG=C.UTF-8` without modifying any CMake definition. Pass `--sources-dir <dir>` to reuse offline git clones at `<dir>/<project>`; the script fails closed if a project clone, tag, or pinned commit is missing or mismatched.

```bash
python3 tests/capture_ctest.py --suite build --project brotli --output /tmp/tokenless-cpp-captures/brotli
python3 tests/capture_ctest.py --suite build --project catch2 --output /tmp/tokenless-cpp-captures/catch2
python3 tests/capture_ctest.py --suite build --project fmt --output /tmp/tokenless-cpp-captures/fmt
```

`brotli_unknown_target_failure.txt` retains a complete failure stream with exit code `1` (`ninja: error: unknown target 'tokenless_nonexistent_target'`), and `fmt_ninja_cold.txt` / `fmt_make_cold.txt` retain all five lines of the real GCC `fmt-c.h` ABI `note:` diagnostic verbatim in place while compacting progress lines before and after it. No failed result is presented as a passing run. The script automatically sanitizes JSON metadata (`src=/work-host/work`, omitting host paths and proxy endpoints).

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test cmake_build
cargo test --locked -p tokenless-compressors --lib build_log::cmake_build -- --nocapture
CPP_CAPTURE_ROOT=/tmp/tokenless-cpp-captures cargo test --locked -p tokenless-compressors --test cmake_build -- --nocapture
```

The optional `CPP_CAPTURE_ROOT` sweep scans `stdout.txt`, `stderr.txt`, and `observed.txt` across `ninja_cold`, `ninja_incremental`, `make_cold`, `make_incremental`, and `failure_unknown_target` for `brotli`, `catch2`, and `fmt` (45 streams total). Both committed fixtures and all 45 captured streams pass independent [literal_oracle.rs](../../../common/literal_oracle.rs) reconstruction (removing only independently specified ANSI SGR escapes for comparison and keeping Markdown-fenced regions verbatim), idempotence, zero-Stash, and strict character + heuristic-token reduction assertions (`stderr.txt` streams and single-line/failure runs stay untouched). Unit tests verify each edit independently.

Two reviewed golden outputs freeze the compact representation (`brotli_ninja_cold.expected.txt` and `brotli_make_cold.expected.txt`). Regenerate only after an intentional output-format change, then inspect the diff:

```bash
cargo test --locked -p tokenless-compressors --test cmake_build regenerate_cmake_build_goldens -- --ignored --exact
```

Committed fixture estimates (`heuristic-v1`):

| Fixture | Input tokens | SGR-cleaned | Candidate tokens | Saving vs input |
| --- | ---: | ---: | ---: | ---: |
| `brotli_ninja_cold.txt` | 769 | 769 | 332 | 56.83% |
| `brotli_make_cold.txt` | 735 | 735 | 317 | 56.87% |
| `catch2_ninja_cold.txt` | 4,270 | 4,270 | 1,154 | 72.97% |
| `catch2_make_cold.txt` | 4,189 | 4,189 | 1,195 | 71.47% |
| `fmt_ninja_cold.txt` | 1,369 | 1,369 | 838 | 38.79% |
| `fmt_make_cold.txt` | 1,571 | 1,571 | 903 | 42.52% |
| `fmt_make_incremental.txt` | 207 | 207 | 145 | 29.95% |
| `brotli_unknown_target_failure.txt` | 15 | 15 | 15 | 0.00% |

Failed-command behavior inside the compressor preserves failure diagnostics verbatim; Runtime continues passing `Tool Error` responses through unchanged. Handwritten `Makefile` logs continue using the existing `Make` route, and `Make VERBOSE=1` compiler invocations stay outside the layout pass; custom `[k/N]` / `[PPP%]` counter steps and non-SGR control lines stay verbatim on `CmakeBuild`, while mixed logs retain retrievable omission (scored formats such as `Make` or `Cargo` and command/exit-anchored `Generic` progress) outside compacted CMake/Ninja blocks, custom counter steps, Markdown fences, and non-SGR control lines when Stash and recovery are enabled (and combined configure-and-build logs whose head declines `CmakeBuild` detection likewise remain eligible for anchored `Generic` fallback). Runtime, CLI, and common Hook tests verify routing, metadata preservation, and host guards. No live Agent invocation was performed.
