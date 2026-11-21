# Transport observation validation

[中文版](POC_REPORT_zh.md)

The read-only BPF Source captures synthetic OpenSSL writes and correlates real
AW fixture identities. Paced fixtures pass, supporting further low-rate pilot
validation. SDK experiments now cover tool-dispatch protection and local model
transport without native Hooks. Pilot scope and remaining integration work are in
[Hook integration](ARCHITECTURE_NOTES.md).

## Tested scope

Tests ran on 2026-11-07 on Linux x86-64, kernel 6.6.102, OpenSSL 3.0.12,
Python 3.11.6, Clang 17, GCC 12.3 and libbpf 1.2.2. AW was built from the baseline
in [SOURCE.md](SOURCE.md). The fixture sends synthetic bytes to a loopback TLS
receiver and verifies its byte count and SHA-256. No model account or real Agent
traffic was involved in that OpenSSL matrix. Existing Yama=0, perf_event_paranoid=2 and
unprivileged_bpf_disabled=2 remained unchanged.

| Coverage | Result |
| --- | --- |
| Bounded event matrix | 69/69 pass: two SSL APIs, 1/4/8/16 threads, new threads, four creation waves, size boundaries, count/rate caps and backpressure |
| Lifecycle | 21/21 pass: same-PID executable change and 20 repeated target exits |
| Strengthened kill/exit regression | 12/12 pass |
| Unit checks | 35/35 pass on native x86-64 and local AArch64, including SDK classification, auth relay and artifact checks |
| Real AW registration | 200 uniquely associated events; 200 events retain same-PID multi-instance ambiguity; owner exit clears bindings |
| Collector death | All eight main-matrix SIGKILL targets complete with receiver-confirmed bytes; final loss summary is absent |
| Buffer handling | Per-thread alternating reuse and shared immutable buffers pass; concurrent mutation remains non-atomic |
| Comparison backend | Earlier count-only matrix: 112 functional, 24 performance and four coexistence cases pass; shared-broker regressions remain passing |

The AW fixture uses the existing empty-route Qoder SDK lifecycle profile and
same-PID exec. It establishes registration/correlation behavior, without real SDK
Hooks or child-Worker adoption. Kernel payload-read failure has unit projection
coverage only. Live AgentSight coexistence and actual PID reuse remain untested.

## SDK boundaries without native Hooks

Pinned versions: Qoder SDK 1.0.56 / Worker 1.1.78, Claude Agent SDK 0.2.88,
LangChain Core 1.7.6 / OpenAI integration 1.0.3, Python MCP 1.26.0,
JavaScript MCP 1.32.2 and Node 22.22.0.

Qoder used a preinstalled SDK tree whose Worker metadata identifies linux-arm64.
The x86-64 experiment executed its JavaScript authentication, catalog and model
selection paths; native platform assets and a fresh x86-64 Worker installation
were not validated. The account gate below prevents a model-transport conclusion.

| Experiment | Actual result | Supported conclusion |
| --- | --- | --- |
| OS launcher + three SDK tool dispatchers | 30/30 cases pass, including allow/deny and five side-effect types | Seccomp rejects subsequent exec and write-opening files with EPERM; tool bodies have already entered |
| LangChain startup injection | 20/20 sync/async cases pass | Patching `BaseTool.run/arun` blocks before the tool body; allowed calls retain their effects |
| Direct LangChain function call | 1/1 counterexample passes | Calling `.func()` bypasses that runtime entry guard |
| Launcher parent-death cleanup | 3/3 focused cases pass after the parent-identity race fix | Baseline exec, denied exec and supervisor SIGKILL cleanup verified |
| LangChain local model call | Completed; 2 SSL_write_ex events, 644 bytes, 2 digests, zero drops | One digest matches the entire 101-byte request body received by the local server |
| Claude dynamic-only control | Model call completes; actual sender remains `UNSUPPORTED` | No supported dynamic libssl; Python frontend has zero hits |
| Claude pinned static profile | Three local model calls complete; each has 2 events / 2109 bytes, zero drops and one receiver wire-digest match | Opt-in profile supports only the tested Claude Code 2.1.161 Linux x86-64 binary |
| Claude Code + Token Plan | One interactive turn completes; 2 TLS writes / 5835 bytes, zero drops | The cloud returns an answer; no cloud-side byte-digest confirmation |
| Qoder authenticated local direct model | Authentication and catalog succeed; account custom-provider capability is disabled, so the case is `BLOCKED` | Model policy runs once, but no local TLS connection or collection starts; transport coverage remains unverified |

The 51 tool cases use real SDK dispatch APIs with synthetic calls and no model
roundtrip. File/exec denial is a fixed syscall policy. Memory-only tools and
writes through inherited descriptors still succeed; a mixed tool mutates memory
before its file write is denied. This policy is not a complete sandbox or a
semantic `tool.before` event. The LangChain guard is a version-pinned startup
adapter with a preset decision, without an AW policy call or formal action receipt.
The final 51-case rerun includes the launcher cleanup fix and stricter artifact
checks: denied files must be absent; blocked inherited descriptors stay empty.
Five regression tests reject empty or partially written artifacts as proof of denial.

Transport tests use actual SDK model calls to a loopback synthetic TLS server,
private HOME directories and no native Hooks. Socket ownership identifies the
real sender before TLS proceeds. Model completion, probe support and capture
accounting are checked separately. Qoder uses an operator-authorized credential
only for allowlisted official authentication and read-only bootstrap metadata.
Inference egress stays blocked. Its public `settings.providers` direct path keeps
the real account capability check; the tested account did not meet it. Inline
`resolveModel` objects select server-side BYOK and cannot route that server to the
fixture loopback. Both offline authentication and authenticated capability blocks
return nonzero and are not counted as capture success. The final Qoder call ended
with `error_during_execution`; the refined BLOCKED classification was verified
by replaying its recorded capability/result evidence and unit tests, without
repeating authentication. SDK classification, relay
and artifact checks are included in the local/native Python test suite. These runs do not
establish Qoder CLI compatibility, arbitrary running-Agent attachment or AW
registration of real SDK Workers. SDK latency/CPU overhead and a production
pilot have not been measured.

## Pinned Claude static TLS entry

The Claude Agent SDK 0.2.88 wheel installed on x86-64 includes Claude Code
2.1.161. Its executable has no exported SSL_write symbol or mapped libssl.
Static BoringSSL discovery followed the referenced Bun research in [SOURCE.md](SOURCE.md).
A candidate entry was checked by disassembly and local TLS receiver digests.
The optional `claude-2.1.161-linux-x64` profile now pins the whole executable
SHA-256 and ELF executable file offset; it performs no runtime signature guessing.
Unknown fingerprints, architectures, APIs and invalid segments are rejected.
The default dynamic resolver keeps its prior UNSUPPORTED result for this Worker.

Three independent local runs verify actual sender ownership, model completion,
receiver digest matching and complete capture accounting. Five additional unit
tests cover profile admission and ELF bounds. Killing the static collector after
attachment, before sending, still allows the Worker to finish its local model
call; the missing final summary keeps loss unknown. Static-profile latency, sustained
load and arbitrary Claude versions remain unmeasured.

A separate user-authorized interactive Claude Code session used Alibaba Cloud
Token Plan's Anthropic-compatible endpoint with model `auto`, tools disabled and
a synthetic coding question. The private network namespace could reach only the
fixed model host through a TLS-passthrough tunnel; two unrelated connection
attempts were rejected. The model answered, and the 30-second collection window
captured two writes with two digests and no loss or read failure. Both CLI and
collector exited successfully. TLS writes may include auxiliary requests and do
not count model turns. The remote service supplied no byte-level receipt.
Temporary credentials and SDK HOME/TMPDIR were removed; no raw headers or request
bodies were retained. This run does not retest Qoder account capabilities or
establish model-driven tool blocking, latency targets or production readiness.

## Workload envelope

Paced cases write 50 times per thread with 5 ms sleeps, retaining exactly
50/200/400/800 events for 1/4/8/16 threads. These short synthetic runs do not set a
sustainable production rate. A stopped collector retains 252 records in the
1 MiB ring and accounts for subsequent loss while the target completes.

Unpaced load uses 1024 writes per thread, 128 bytes each, with one baseline and
one observed run per concurrency:

| Threads | Baseline p99 µs | Observed p99 / max µs | Emitted / hits | Ring loss |
| --- | --- | --- | --- | --- |
| 1 | 4.650 | 24.754 / 78.483 | 293 / 1024 | 71.4% |
| 4 | 9.013 | 23.030 / 53.402 | 300 / 4096 | 92.7% |
| 8 | 14.860 | 38.118 / 1222.998 | 321 / 8192 | 96.1% |
| 16 | 12.960 | 34.067 / 3958.296 | 380 / 16384 | 97.7% |

These are whole SSL_write latencies, not isolated probe pauses. Collector RSS
is 19,956–19,972 KiB and active CPU increments are 0.04–0.09 seconds, measured at
10 ms granularity. Payload-Source idle CPU was not measured. Even a single-thread
burst loses records, so a small pilot must measure its actual bursts as well as
concurrency. Current loss accounting reconciles hits, ring drops, emitted events,
output caps and identity discards; passing that check does not imply full capture.

## Retained failure and next validation

The ptrace comparison remains disabled by default: tracer SIGKILL can terminate
the target with SIGTRAP. The original native acceptance had five such failures.
This failure applies even to small deployments, and payload rewrite remains
unavailable. BPF collector-death results apply only to the read-only Source.

Next use an eligible account to continue Qoder direct-model validation and
validate the pinned Claude profile lifecycle and workload envelope, then validate
one managed Worker and one active Query at the chosen
task rate. Confirm process ownership,
collection lifecycle and fixed privilege assets before a restricted read-only
pilot. Extreme-concurrency optimization can follow separately.

Reproduction commands are in [README](README.md). Native C/BPF warning checks,
Black, Python tests and documentation/link checks pass; unchanged Rust sources
retain their prior fmt/test/clippy results. Detailed JSONL and measurement files
are generated in the selected private output directory. They are runtime evidence
and are not checked into this source package.
