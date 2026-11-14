# Maven regression corpus

[中文版](README_zh.md)

These fixtures validate Maven BuildLog changes based on ANOLISA `442d70487`
(Tokenless 0.9.5).
They are source-change evidence, not a release or live-Agent validation.
`provenance.json` records pinned project commits, immutable Docker image IDs,
Maven/JDK versions, commands, exit status and raw SHA-256 values. Each complete
fixture is the parent's observed stdout/stderr chunk stream; its ordering is not
a guarantee about JVM cross-stream chronology. `downloads_batch.txt` is an
unmodified contiguous stdout excerpt, with its source line range recorded.

## Reproduce captures

From `src/tokenless`, use Docker and Python 3.11 or later. Each output directory
must be new; projects and dedicated dependency caches are created only there.
The script runs each unmodified project's `clean package` twice, first with an
empty cache and then with the same cache. It neither edits dependency definitions
nor disables tests. Default and batch mode have separate caches and measurements.

```bash
docker pull maven:3.9.9-eclipse-temurin-21
docker pull maven:3.9.9-eclipse-temurin-17
python3 tests/capture_maven.py --project commons-cli --output /tmp/tokenless-maven-9966-commons
python3 tests/capture_maven.py --project slf4j --output /tmp/tokenless-maven-9966-slf4j
python3 tests/capture_maven.py --project gson --output /tmp/tokenless-maven-9966-gson
python3 tests/capture_maven.py --project jsoup --output /tmp/tokenless-maven-9966-jsoup-default
python3 tests/capture_maven.py --project jsoup --batch --output /tmp/tokenless-maven-9966-jsoup-batch
```

The current script explicitly limits both Docker and JVM processor counts to two.
The initial jsoup captures failed because Jetty's test proxy calculated a selector
thread requirement greater than its configured pool size on this host. Docker's
CPU quota alone did not correct the older JDK's processor detection; adding
`JAVA_TOOL_OPTIONS=-XX:ActiveProcessorCount=2` did. `jsoup_failure.txt` retains a
complete original failure, including its errors and stack traces. Earlier
successful Commons CLI, SLF4J and Gson runs did not require that JVM setting;
their exact commands are retained in provenance. No failed result is presented
as a successful build. Capture output includes the JVM's option announcement.

## Verify preservation and savings

```bash
cargo test --locked -p tokenless-compressors --test maven
cargo test --locked -p tokenless-compressors build_log::maven -- --nocapture
MAVEN_CAPTURE_ROOT=/tmp cargo test --locked -p tokenless-compressors --test maven -- --nocapture
```

The optional root scan consumes `cold` and `warm` stdout/stderr/observed files
under directories named `tokenless-maven-9966-*`. Thirty successful captured
streams and nine retained failed-capture streams passed independent complete
reconstruction, exact ordered CR-record retention, idempotence and zero-Stash
checks. The decoder in `tests/common/maven_oracle.rs` does not call the production
parser. It removes only independently specified SGR escapes for comparison;
no decorative layout normalization is needed. Unit tests also reconstruct every
edit and verify its strict character and heuristic-token savings. Disabling only
download edits provides the ablation below, without changing scope recognition.

Warnings, exceptions and unknown text are inserted at every boundary of both a
synthetic download block and the captured 40-record excerpt inside explicit
Maven phase context: 216 mutations in total. Other cases cover raw CR
continuations, batch prefixes, singletons, duplicates, malformed/truncated
input, control sequences, Unicode, scope reopening, and atomic 256-record/64-KiB
limits.

Routing regressions cover 1/2/30/400 downloads, isolated status markers, quoted
Maven phases in prose/Markdown/CSV/source code, structured launcher/JVM
preambles within the head budget, fenced blocks inside and outside logs,
CRLF-only logs, and head/tail sample gaps. Download-only fragments do not
trigger a build-log route; neither Cargo nor standalone Generic detection treats
Maven transfer labels or CR redraws as progress evidence. Command-wrapped logs
without Maven edits retain Generic Shell/Tool recovery, including scoped
verified transfers before any diagnostic or unknown boundary, with exact
reconstruction of stashed omissions. Edit counts around the 256-edit limit
retain the unedited suffix and remain idempotent.

Two reviewed golden outputs freeze the compact representation. The raw download
excerpt is checked unchanged on its own, then checked against its compact golden
with an explicitly synthetic Maven phase preamble. Full-stream fixture inputs
are unchanged; Runtime, CLI and Hook positives use the complete captured jsoup
batch log. Regenerate only after an intentional output-format change, then
inspect the diff:

```bash
cargo test --locked -p tokenless-compressors --test maven regenerate_maven_goldens -- --ignored --exact
```

Complete observed-stream estimates from these captures (heuristic-v1):

| Fixture | Input | Candidate | Saving | Download contribution |
| --- | ---: | ---: | ---: | ---: |
| Commons CLI, warm | 3,992 | 3,657 | 8.39% | 0 |
| SLF4J, warm | 6,040 | 5,817 | 3.69% | 0 |
| Gson, default cold | 121,636 | 121,281 | 0.29% | 314 |
| jsoup, default cold | 122,265 | 121,975 | 0.24% | 290 |
| jsoup, batch cold | 58,770 | 48,582 | 17.34% | 10,188 |

These compare each input with its own candidate, not default input with batch
input. Do not treat them as model billing or whole-task savings. Failed-command
fixtures exercise preservation inside the compressor; Runtime still passes Tool
Errors through unchanged. CLI and Common Hook tests verify local routing and
replacement envelopes, including unchanged metadata and unsupported-host gates.
No real Agent invocation was performed.

The five successful complete fixtures were also replayed through the actual
Protocol v2 CLI with recovery `none`: all selected their smaller BuildLog result
with no Stash keys. The failure fixture returned `tool_error` unchanged. On Linux
x86-64, workspace formatting, Clippy, tests and rustdoc passed, together with
`make test-integration`, `make test-adapters`, `make test-rtk-integration` and the
L1/L2 standalone workspace checks and release tests. Integration used Python
3.11.6, matching CI's Python 3.11 family. The pre-existing Headroom recursion test
fails on this host's Python 3.13/3.14; it passes on 3.11.6 and was not changed.
