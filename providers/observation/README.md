# AW transport observation experiment

[中文版](README_zh.md)

This independent package collects bounded TLS write metadata from authorized
Linux x86-64 processes. The read-only BPF Source records timestamps, lengths,
SHA-256 and capture loss; a separate importer correlates real AW fixture
identities. The next integration target is small, restricted Qoder/Qoder SDK
observation. [Hook integration](ARCHITECTURE_NOTES.md) explains capabilities,
native Hook replacement limits and the proposed pilot.

The package is an Observation Source experiment, with no default installation
or registered Provider protocol. Maintainer: kongche-jbw. Source and license
attribution: [SOURCE.md](SOURCE.md). Measured results: [POC_REPORT.md](POC_REPORT.md).

## Run the BPF fixture

Use an explicitly authorized Linux x86-64 lab with kernel/UAPI support for
`BPF_TRACE_UPROBE_MULTI`, Python 3.11+, GCC, Clang with the BPF target, libbpf
headers/runtime, OpenSSL development files and `openssl`. The fixture uses the
operator's existing non-interactive sudo permission for a short-lived setup
helper; it never changes global security settings. Collection runs as the ordinary
user with empty capabilities. Tested library/tool versions are in the report.

From the repository root, build the real AW CLI and run the matrix:

```bash
(cd src/aw && cargo build --locked -p aw-cli)
AW_FIXTURE="$PWD/src/aw/target/debug/aw"
python3 providers/observation/scripts/transport_cases.py \
  --output "$PWD/providers/observation/target/transport" --aw "$AW_FIXTURE"
python3 providers/observation/scripts/transport_lifecycle.py \
  --output "$PWD/providers/observation/target/transport-lifecycle"
```

Choose new output directories with absolute paths short enough for Unix sockets.
The matrix builds the BPF object, loader and TLS client, creates a loopback server
and private AW daemon, checks receiver bytes/digests, and reaps its processes.
Expected: 69 passing matrix cases, including 200 uniquely correlated events and
200 ambiguous same-PID events; the lifecycle runner has 21 cases. Output includes
private JSONL, generated certificates and `results.json`. Remove only that output
directory after checking cleanup. Without `--aw`, 67 cases run with no instance
correlation test; `--smoke` selects one basic case.

## Source output and limits

The direct entrypoint `scripts/uprobe_transport.py` requires `--permit`,
`--library`, `--object` and `--output`. The fixture generates the private process
permit, `loader.so` and `transport.bpf.o`. A permit pins PID/start ticks, UID, PID
namespace, boot identity, executable identity and hash. The collector retains a
pidfd and rechecks identity before event projection. This operator-controlled
lab loader requires fixed, reviewed assets before persistent deployment.

| Option or resource | Default / limit |
| --- | --- |
| `--function` | `SSL_write`; also `SSL_write_ex` |
| `--duration` | 10 / 30 active seconds |
| `--max-events` | 4096 / 65536 |
| `--rate` | 10000 / 100000 events per second |
| `--privileged-fixture-setup` | Opt-in short-lived sudo helper |
| Kernel ring / entry | 1 MiB / 4144 bytes |
| Captured bytes | At most 4096, temporarily in memory; only SHA-256 persisted |
| JSONL output | Exclusive 0600, 8192 bytes/record, 64 MiB data plus two control records |

Oversize writes have null digests. Saturation drops records and increments
counters while the target continues. Output caps do not limit the target's write
rate. Missing final summaries, including collector SIGKILL, leave interval loss
unknown. Stopped collectors or blocked filesystems can exceed active duration;
the fixture owns wall-clock deadlines. Shared-memory reads are non-atomic and
entry hashes do not prove successful send adoption.

The private `aw.transport-observation-poc/v0` schema retains transport-only
coverage. Raw events have null AW IDs; the independent importer verifies fixture
ownership before association. Unknown processes stay Observed, and multiple
instances on one PID stay ambiguous. Real Qoder/SDK TLS paths and child-Worker
mapping still need verification; see the [restricted pilot](ARCHITECTURE_NOTES.md#restricted-pilot).

## Development checks and comparison backends

```bash
python3 -B -m unittest discover -s providers/observation/tests -v
(cd providers/observation && cargo fmt --all -- --check)
(cd providers/observation && cargo test --locked)
(cd providers/observation && cargo clippy --locked --all-targets -- -D warnings)
```

`uprobe_cases.py` and `uprobe_coexist.py` retain the count-only baseline. The Rust
ptrace backend rejects `observe` by default. Only explicit `cargo build --release
--locked --features unsafe-lab` in this package enables its disposable fixtures;
tracer SIGKILL can terminate the target with SIGTRAP, including before the first
write. `acceptance.py`, `lifecycle.py`, `races.py` and `demo.py` retain reproduction
coverage; `--aw` supplies the private AW fixture where required. Failing crash
cases return nonzero. Payload rewrite is unavailable.
