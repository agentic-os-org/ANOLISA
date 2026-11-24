# C/C++ compiler invocation regression corpus

[中文版](README_zh.md)

These fixtures validate C/C++ compiler invocation (`cc_compile`) Stage 1 BuildLog changes based on ANOLISA `0172542d25442d94f01c00e72c82a79aec2eab46` (Tokenless `0.9.7`). They are source-change evidence, not a release or live-Agent validation. `provenance.json` records pinned project commits, the immutable Docker image ID (`tokenless-cpp-capture:ubuntu24.04`, `sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`), CMake/Ninja/Make/GCC/Clang versions, sanitized container commands, exit status, and SHA-256 digests. All eight fixtures are complete unmodified `observed.txt` streams.

## Reproduce captures

From `src/tokenless`, use Docker with a prepared `tokenless-cpp-capture:ubuntu24.04` toolchain image (Ubuntu 24.04 with `cmake` 3.28.3, `ninja` 1.11.1, `GNU Make` 4.3, GCC 13.3.0, and Clang 18.1.3) and Python 3.11 or later. Each run creates an unmodified pinned source checkout under `--network none --cpus 2`, `TZ=UTC`, and `LANG=C.UTF-8` without modifying any build definition. Pass `--sources-dir <dir>` to reuse offline git clones at `<dir>/<project>`; the script fails closed if a project clone, tag, or pinned commit is missing or mismatched.

```bash
python3 tests/capture_ctest.py --suite verbose --project brotli --output /tmp/tokenless-cpp-captures/brotli
python3 tests/capture_ctest.py --suite verbose --project catch2 --output /tmp/tokenless-cpp-captures/catch2
python3 tests/capture_ctest.py --suite verbose --project fmt --output /tmp/tokenless-cpp-captures/fmt
python3 tests/capture_ctest.py --suite verbose --project zstd --output /tmp/tokenless-cpp-captures/zstd
```

`brotli_clang_ninja_verbose_failure.txt` retains a complete `-Weverything -Werror` compilation failure stream with exit code `1` verbatim in place, and `fmt_ninja_verbose.txt` retains all five lines of the real GCC `fmt-c.h` ABI `note:` diagnostic verbatim in place while compacting compile invocations around it. No failed result is presented as a passing run. The script automatically sanitizes JSON metadata (`src=/work-host/work`, omitting host paths and proxy endpoints).

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test cc_compile
cargo test --locked -p tokenless-compressors --lib build_log::cc_compile -- --nocapture
CPP_VERBOSE_CAPTURE_ROOT=/tmp/tokenless-cpp-captures cargo test --locked -p tokenless-compressors --test cc_compile -- --nocapture
```

The optional `CPP_VERBOSE_CAPTURE_ROOT` sweep scans `stdout.txt`, `stderr.txt`, and `observed.txt` across 16 captured verbose CMake (`VERBOSE=1`, `ninja -v`) and native GNU Make (`V=1`) GCC/Clang runs under `captures/` and `slice_c_verbose/` (48 streams total). Both committed fixtures and all 48 captured streams pass independent [literal_oracle.rs](../../../common/literal_oracle.rs) reconstruction (removing only independently specified ANSI SGR escapes for comparison and keeping Markdown-fenced regions verbatim), idempotence, zero-Stash, and strict character + heuristic-token reduction assertions (`stderr.txt` and uncompacted failure streams stay untouched). Unit tests verify each edit and safety boundary independently.

Two reviewed golden outputs freeze the compact representation (`brotli_make_verbose.expected.txt` and `brotli_ninja_verbose.expected.txt`). Regenerate only after an intentional output-format change, then inspect the diff:

```bash
cargo test --locked -p tokenless-compressors --test cc_compile regenerate_cc_compile_goldens -- --ignored --exact
```

Committed fixture estimates (`heuristic-v1`):

| Fixture | Input tokens | SGR-cleaned | Candidate tokens | Saving vs input |
| --- | ---: | ---: | ---: | ---: |
| `brotli_make_verbose.txt` | 4,950 | 4,950 | 2,604 | 47.39% |
| `brotli_ninja_verbose.txt` | 3,585 | 3,585 | 1,610 | 55.09% |
| `brotli_clang_make_verbose.txt` | 5,041 | 5,041 | 2,747 | 45.51% |
| `brotli_clang_ninja_verbose_failure.txt` | 732 | 732 | 732 | 0.00% |
| `catch2_make_verbose.txt` | 46,894 | 46,894 | 11,148 | 76.23% |
| `fmt_ninja_verbose.txt` | 6,109 | 6,109 | 3,457 | 43.41% |
| `zstd_make_lib_v1_gcc.txt` | 5,432 | 5,432 | 1,531 | 71.82% |
| `zstd_make_lib_v1_clang.txt` | 5,619 | 5,619 | 1,542 | 72.56% |

Failed-command behavior inside the compressor preserves failure diagnostics verbatim; Runtime continues passing `Tool Error` responses through unchanged. Handwritten short `Makefile` logs continue using the existing `Make` route and non-verbose CMake/Ninja progress logs remain on `cmake_build`, while mixed logs containing both progress and verbose `-c` invocations compose both Stage 1 lossless passes on non-overlapping line spans; custom counter steps, shell metacharacters, CRLF-only logs, and non-SGR control lines stay verbatim on `cc_compile`, while mixed logs retain retrievable omission (scored formats such as `Make` or `Cargo` and command/exit-anchored `Generic` progress) outside compacted compile blocks, custom steps, Markdown fences, and non-SGR control lines when Stash and recovery are enabled. Runtime, CLI, and common Hook tests verify routing, metadata preservation, and host guards. No live Agent invocation was performed.
