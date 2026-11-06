# AW ptrace observation experiment

[中文版](README_zh.md)

This independent Sidecar records bounded TLS write metadata from explicitly authorized Linux x86-64 test processes. It supports dynamic OpenSSL `SSL_write` and `SSL_write_ex` in advise mode. A separate fixture importer correlates live process identities with real AW instances and task scopes. Maintainer: kongche-jbw; source and license attribution: [SOURCE.md](SOURCE.md).

**Default builds fail closed:** `observe` returns `observation_disabled` before loading the permit, creating output or attaching. Explicit `--features unsafe-lab` builds retain observation solely for disposable synthetic processes. Killing that tracer can terminate its target with SIGTRAP, including after attachment but before the first write. P0 crash isolation remains unresolved and payload rewriting is unavailable. See [measured results](POC_REPORT.md) and [Go/No-Go](GO_NO_GO.md).

## Read-only uProbe isolation comparison

A separate fixture-only BPF counter tests an alternative collection mechanism.
It uses a PID-filtered uProbe link, retains one counter, and never reads TLS
payloads. The normal ptrace binary remains disabled. This comparison does not
implement the transport event/digest contract or replace the AW correlation demo.

On the authorized Linux x86-64 lab, with Linux headers supporting
`BPF_TRACE_UPROBE_MULTI`, GCC, Python 3.11 and the existing non-interactive sudo
permission for the test owner:

```bash
python3 providers/observation/scripts/uprobe_cases.py \
  --output "$PWD/providers/observation/target/uprobe" --repeat 3
python3 providers/observation/scripts/uprobe_cases.py \
  --output "$PWD/providers/observation/target/uprobe-performance" --performance --repeat 3
python3 providers/observation/scripts/uprobe_coexist.py \
  --output "$PWD/providers/observation/target/uprobe-coexist"
```

The scripts create their own authorized TLS processes and compile the small BPF
loader. A short-lived helper validates the permit, creates a PID-filtered link
and a read-only userspace map handle, transfers the descriptors over an anonymous
Unix socket, and exits before collection starts. Collection runs as the test
owner with no effective/permitted/inheritable/ambient capabilities and with
`no_new_privs`. No persistent privileged service or security-setting change is
used. The helper is a lab harness executed with existing sudo authorization; it
is not a hardened production privilege broker.

Expected results and limits are in [the report](POC_REPORT.md#read-only-uprobe-comparison).
`aw.uprobe-counter-poc/v0` records only cumulative entry counts and metadata;
there is no payload digest, per-write length or timestamp, return value, or AW
instance attestation. Output is bounded to 512 records of at most 8192 bytes and
10 seconds of active collection; a stopped process or blocked output can delay
exit. The fixture harness owns deadlines and reaps its processes. Generated
certificates, binaries and logs belong only to the selected output directory.

## Build and demonstrate

Prerequisites: Linux x86-64, Rust 1.97.1, Python 3.11+, GCC, OpenSSL headers/libraries and `openssl`. The observer and target must share a UID and the existing ptrace policy must permit attachment. The tested lab had `ptrace_scope=0`. Under `ptrace_scope=1`, this sibling-process demo is expected to reject attachment; use a separately authorized isolated lab or a future parent-supervisor launcher. The scripts never alter that setting.

From the repository root, build the default refusal binary and explicitly opt into the hazardous lab binary for these disposable fixtures:

```bash
(cd providers/observation && cargo build --release --locked --target-dir target/safe)
(cd providers/observation && cargo build --release --locked --features unsafe-lab)
(cd src/aw && cargo build --locked -p aw-cli)
python3 providers/observation/scripts/demo.py \
  --aw "$PWD/src/aw/target/debug/aw" \
  --output "$PWD/providers/observation/target/demo"
```

Use a new output directory each time. The demo starts its own loopback TLS server, clients and private AW daemon. Expected: 400 registered writes, 100 unregistered writes, verified server byte digests, a unique fixture association, and zero AW bindings after owner exit. `registered/correlated.jsonl` contains instance/scope IDs; raw Sidecar events always carry null AW IDs. The demo uses empty native routes under the existing Qoder SDK registration profile and certifies only the lifecycle contract, not a real SDK.

All generated state stays in the selected directory. Tests reap their own processes and stop the private daemon. The TLS private key is ephemeral synthetic test material. After inspecting results and confirming the scripts have exited, remove only that chosen output directory. No daemon installation or default Provider registration is performed.

## Explicit CLI

For a process you created and authorized for this experiment:

```bash
OBSERVER="$PWD/providers/observation/target/release/aw-observation-ptrace-poc"
"$OBSERVER" authorize --pid "$FIXTURE_PID" --output "$PRIVATE_DIR/permit.json"
"$OBSERVER" observe --permit "$PRIVATE_DIR/permit.json" \
  --output "$PRIVATE_DIR/events.jsonl" --mode advise \
  --duration 10 --max-events 4096 --rate 10000
```

`authorize` pins PID, start ticks, UID, PID namespace, boot identity, executable device/inode/path and SHA-256. It asserts operator authorization; a same-UID attacker is outside this POC trust boundary. `observe` verifies the permit and opens a pidfd before attachment. SIGINT/SIGTERM request bounded detach. Existing output files are rejected; files are created with mode 0600. The Sidecar never receives an AW socket or lifecycle handle.

| Limit | Default / hard maximum |
| --- | --- |
| Runtime | 10 / 60 seconds |
| Retained payload | SHA-256 of at most 4096 bytes; no body output |
| Events | 4096 / 65,536 |
| Output rate | 10,000 / 100,000 events per second |
| Encoded record | 8192 bytes |
| Output data budget | 64 MiB threshold, plus one bounded record and lifecycle records |
| Traced threads / pause samples | 256 / 65,536 |
| Attach / detach | 3 seconds each; cleanup failure is reported |

Oversize writes have a null digest and `payload_over_limit`; dropped events are counted. `coverage.status` remains `unknown`: a transport entry does not prove success at the peer or identify a model request. `--offsets` is parsed and explicitly returns `unsupported_offsets`; static/stripped binaries and other TLS ABIs are unsupported. An existing entry `int3` returns `unsupported_live_uprobe_or_int3` without replacing it. Live uProbe coexistence has not been validated. `--mode enforce` is rejected.

## Verification

```bash
(cd providers/observation && cargo fmt --all -- --check)
(cd providers/observation && cargo test --locked)
(cd providers/observation && cargo test --locked --features unsafe-lab)
(cd providers/observation && cargo clippy --locked --all-targets -- -D warnings)
(cd providers/observation && cargo clippy --locked --all-targets --features unsafe-lab -- -D warnings)
python3 -B -m unittest discover -s providers/observation/tests -v
python3 providers/observation/scripts/lifecycle.py \
  --output "$PWD/providers/observation/target/lifecycle" --repeat 3
python3 providers/observation/scripts/races.py \
  --output "$PWD/providers/observation/target/races"
python3 providers/observation/scripts/acceptance.py \
  --aw "$PWD/src/aw/target/debug/aw" \
  --output "$PWD/providers/observation/target/acceptance"
```

Both lifecycle and acceptance commands return **1** when crash-safety cases fail. The lifecycle runner records deterministic checkpoints and injected-error cleanup separately from actual SIGKILL outcomes. `AW_PTRACE_TEST_PHASE` is honored only by unsafe-lab builds; its stdin rendezvous expires after five seconds. The race suite covers four waves of thread creation, busy attachment, SIGSTOP/SIGCONT and detach during a group stop. Unsupported architectures return **77** with an explicit SKIP record. Unit tests alone do not constitute native acceptance. The independent workspace has no AW dependency and does not join the framework layout gate or installer. The [architecture notes](ARCHITECTURE_NOTES.md) describe the future query bridge separately from this worker.
