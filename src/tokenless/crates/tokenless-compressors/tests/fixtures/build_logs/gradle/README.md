# Gradle regression corpus

[中文版](README_zh.md)

These fixtures validate Gradle BuildLog changes based on ANOLISA `81e8476d4ee2a5370583fd65d48dc6a7f1f00db0` (Tokenless 0.9.5).
They are source-change evidence, not a release or live-Agent validation.
`provenance.json` records pinned project commits, immutable Docker image IDs,
Gradle/JDK versions, sanitized container commands, exit status, and SHA-256 values.
Complete fixtures are unmodified captured streams; `mockito_cold_excerpt.txt` is an
unmodified contiguous excerpt of `mockito` cold `stdout` (lines `1`–`789`, omitting
only trailing lines `790`–`793` containing the Develocity Build Scan publication URL).

## Reproduce captures

From `src/tokenless`, use Docker and Python 3.11 or later. Each run creates a new project directory at `--output`; unmodified checkouts and isolated Gradle homes live only there. With `--failure-probe`, the script executes `cold`, `incremental`, `clean_rebuild`, and `failure_unknown_task` (`buid`) with `--cpus 2`, `TZ=UTC`, and `LANG=C.UTF-8` without modifying build scripts or passing `-q` / `--console=plain` / `-x test`. Caffeine is scoped to `:simulator:build` so multi-hour concurrency model checks (`lincheckTest` / `frayTest`) are excluded while keeping plugin builds, compilation, static checks, configuration cache, and toolchain resolution representative.

```bash
docker pull eclipse-temurin:21-jdk
python3 tests/capture_gradle.py --project mockito --failure-probe --output /tmp/tokenless-gradle-captures/mockito
python3 tests/capture_gradle.py --project caffeine --failure-probe --output /tmp/tokenless-gradle-captures/caffeine
python3 tests/capture_gradle.py --project resilience4j --failure-probe --output /tmp/tokenless-gradle-captures/resilience4j
```

`caffeine_failure.txt` retains a complete failure stream with exit code `1`. No failed result is presented as a successful build. The script automatically sanitizes JSON metadata (`src=/work-host/work`, omitting host `uname` and proxy endpoints).

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test gradle
cargo test --locked -p tokenless-compressors build_log::gradle -- --nocapture
GRADLE_CAPTURE_ROOT=/tmp/tokenless-gradle-captures cargo test --locked -p tokenless-compressors --test gradle -- --nocapture
```

The optional `GRADLE_CAPTURE_ROOT` sweep scans `stdout.txt`, `stderr.txt`, and `observed.txt` across `cold`, `incremental`, `clean_rebuild`, and `failure_unknown_task` for `mockito`, `caffeine`, and `resilience4j` (36 streams total). Both committed fixtures and the full 36-stream corpus pass independent `tests/common/gradle_oracle.rs` reconstruction (removing only independently specified ANSI SGR escapes for comparison and keeping Markdown-fenced regions verbatim), idempotence, zero-Stash, and strict character + heuristic-token reduction assertions. Unit tests verify each edit independently.

Boundary mutations insert warnings, Kotlin `w:` notes, Java exception stack traces, `> Task :... FAILED`, `\r`, `\x1b[0K`, `\t`, and unclassified lines at every line boundary of a real `mockito_cold_excerpt.txt` block and synthetic blocks (310 synthetic mutations in unit tests plus 232 excerpt mutations in integration tests). Additional tests cover CRLF-only rejection, Markdown fences across head/tail sample gaps and inside otherwise valid build logs (preserving literal quoted tasks verbatim), `exit code:` / `exit status:` and `> Configure project :` head anchoring with custom configuration output, `4 KiB` (`4096`-byte) per-line local scope close versus `64 KiB` whole-result suppression, `256`-row / `64 KiB` block splitting, `256`-edit capping, `> 4 KiB` generated-header idempotence, anchored `Generic` fallback for uneditable logs, and keeping `> Task :...` lines out of `Generic` progress rules.

Two reviewed golden outputs freeze the compact representation. Regenerate only after an intentional output-format change, then inspect the diff:

```bash
cargo test --locked -p tokenless-compressors --test gradle regenerate_gradle_goldens -- --ignored --exact
```

Committed fixture estimates (heuristic-v1):

| Fixture | Input tokens | SGR-cleaned | Candidate tokens | Saving vs input |
| --- | ---: | ---: | ---: | ---: |
| `caffeine_cold.txt` | 953 | 953 | 899 | 5.67% |
| `caffeine_incremental.txt` | 795 | 795 | 632 | 20.50% |
| `caffeine_failure.txt` | 511 | 511 | 483 | 5.48% |
| `resilience4j_incremental.txt` | 5,266 | 5,266 | 1,963 | 62.72% |
| `resilience4j_clean_rebuild.txt` | 5,331 | 5,331 | 2,096 | 60.68% |
| `mockito_cold_excerpt.txt` | 10,243 | 9,852 | 7,865 | 23.22% |

Failed-command behavior inside the compressor preserves failure tasks and exception text verbatim; Runtime continues passing `Tool Error` responses through unchanged. Runtime, CLI, and common Hook tests verify routing, metadata preservation, and host guards. No live Agent invocation was performed.
