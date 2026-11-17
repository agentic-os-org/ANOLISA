# Gradle regression corpus

[中文版](README_zh.md)

These fixtures validate Gradle BuildLog changes based on ANOLISA `442d70487`
(Tokenless 0.9.5). They are source-change evidence, not a release or
live-Agent validation. `provenance.json` records the pinned project tag,
Gradle wrapper version, JDK, host platform, commands, exit status and raw
SHA-256 values. Each fixture is the captured stream's observed
stdout+stderr concatenation (`observed.txt`), byte-identical to the capture
receipt in `capture.json`.

## Capture environment

The committed fixtures come from mockito/mockito v5.24.0 built with its
pinned Gradle wrapper 8.14.2 and Eclipse Temurin JDK 21.0.13+9 on a Linux
x86-64 host. Captures were performed natively (no container) because
container registry blob redirects are blocked on the capture network;
dependency and wrapper downloads went through the site HTTP proxy configured
in `gradle.properties` (Java ignores the `http_proxy` environment variable).
Build scripts were unmodified and no `-q`, `--console=plain` or `-x` flags
were used.

The optional `GRADLE_CAPTURE_ROOT` scan additionally covers local captures
of caffeine 3.3.1 (Gradle wrapper 9.8.1) and resilience4j 2.4.0 (Gradle
wrapper 8.10); their wrapper-download progress lines, retry backoff notice
and `Welcome to Gradle` banner bullets shaped the recognition preambles. One
local caffeine capture failed its OSGi tests for environmental reasons (Pax
Exam bundle lock on the host); no failed result is presented as a successful
build.

## Reproduce captures

From a new output directory, with the same JDK and a `GRADLE_USER_HOME`
whose `gradle.properties` carries the proxy settings:

```bash
git clone --branch v5.24.0 https://github.com/mockito/mockito.git source
cd source
./gradlew build                          # cold
./gradlew build                          # incremental
./gradlew clean build                    # clean_rebuild
./gradlew build buid                     # failure_unknown_task (misspelled task)
```

`mockito_cold.txt`, `mockito_incremental.txt`, `mockito_clean_rebuild.txt`
and `mockito_unknown_task.txt` correspond to those four modes. Every mode
exited non-zero: the javadoc task fails under this JDK configuration
(`Javadoc generation failed`), which is retained verbatim — the failure
records and their diagnostics exercise exactly the adjacency preservation
the Gradle compactor must guarantee. No failed result is presented as a
successful build.

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test gradle
GRADLE_CAPTURE_ROOT=/tmp cargo test --locked -p tokenless-compressors --test gradle captured_streams -- --nocapture
```

The optional root scan consumes `cold`, `incremental`, `clean_rebuild` and
`failure_unknown_task` stdout/stderr/observed files. All committed fixtures
and captured streams passed independent complete reconstruction, idempotence
and zero-Stash checks. The decoder in `tests/common/gradle_oracle.rs` does
not call the production parser. Heuristic-v1 estimates for the observed
streams: cold 5,995 → 4,899 tokens (18.3%), incremental 8,165 → 6,237
(23.6%), clean rebuild 7,097 → 7,063, unknown-task failure 410 → 377.

Warnings, test lines, stack traces and SGR-only records never enter a
template block. The reviewed golden `mockito_incremental.expected.txt`
freezes the compact representation for the incremental capture; regenerate
only after an intentional output-format change:

```bash
cargo test --locked -p tokenless-compressors --test gradle regenerate_gradle_goldens -- --ignored --exact
```
