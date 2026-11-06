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
instances on one PID stay ambiguous. Real SDK child-Worker mappings to AW still
need verification. SDK results are in the report; see the
[restricted pilot](ARCHITECTURE_NOTES.md#restricted-pilot).

## SDK boundary fixtures

The SDK fixtures exercise real dispatch APIs and local synthetic model endpoints.
They do not require native Hooks. The [report](POC_REPORT.md#sdk-boundaries-without-native-hooks)
separates tool-entry checks, syscall denial and transport coverage.

Prepare a private dependency environment on the authorized Linux x86-64 host.
Node 22, `libseccomp.so.2` and the BPF prerequisites above are required. Install
Python packages and SDK packages into a separate directory:

```bash
SDK_ROOT="$PWD/providers/observation/target/sdk-deps"
python3 -m venv "$SDK_ROOT/venv"
SDK_PYTHON="$SDK_ROOT/venv/bin/python"
SDK_NPM="$SDK_ROOT/npm"
"$SDK_PYTHON" -m pip install -r providers/observation/fixtures/sdk_transport_requirements.txt
npm install --ignore-scripts --prefix "$SDK_NPM" \
  @qoder-ai/qoder-agent-sdk@1.0.56 @modelcontextprotocol/sdk@1.32.2
```

The Qoder model fixture additionally needs Worker 1.1.78 before entering its
offline namespace. The npm package with `--ignore-scripts` does not contain it.
The official installer downloads and verifies the platform runtime:

```bash
node "$SDK_NPM/node_modules/@qoder-ai/qoder-agent-sdk/scripts/postinstall.cjs"
```

This fresh-install command was checked against installer source, but not run in
this experiment. The tested Worker JS came from a preinstalled SDK tree; its
native platform assets were not validated (see the report). Tool-dispatch cases
do not start that Worker. `SDK_NPM` contains `package.json` and `node_modules`.
Run the tool cases:

```bash
"$SDK_PYTHON" providers/observation/scripts/sdk_boundary_cases.py \
  --python "$SDK_PYTHON" --npm "$SDK_NPM" \
  --output "$PWD/providers/observation/target/sdk-boundary"
```

Expected: 51 passing cases. Allow/deny cases verify the actual side effects and
whether the tool body entered. The launcher allows the initial interpreter exec,
then denies subsequent exec and write-opening files. Runtime injection covers
LangChain's pinned sync/async tool entry; the direct-function bypass is an explicit
counterexample. These scripts are experimental launchers, without AW policy RPC.
The separate `sdk_boundary_requirements.txt` supports running only tool cases.

For model transport, `iproute2` and `util-linux` provide the disposable loopback
namespace. Existing sudo authorization creates it, then drops to the current user:

```bash
SDK_ENTRY="$SDK_NPM/node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"
sudo -n unshare --net -- sh -c '
  ip link set lo up
  exec runuser -u "$1" -- "$2" "$3" --output "$4" --qoder-sdk "$5" \
    --case langchain --case claude
' fixture "$(id -un)" "$SDK_PYTHON" \
  "$PWD/providers/observation/scripts/sdk_transport_cases.py" \
  "$PWD/providers/observation/target/sdk-transport" "$SDK_ENTRY"
```

Expected: LangChain `OBSERVED`, Claude `UNSUPPORTED` with a completed model call.
The runner refuses a host network or root. It starts the local TLS server, checks
the sender socket and identity, attaches both SSL probes and verifies server
receipt. Adding `--case qoder` without authentication support reports BLOCKED and
makes the suite exit nonzero. Output contains private results and process
diagnostics; remove only the chosen fixture output after cleanup. Packages and
generated evidence are not source assets.

Qoder's authenticated direct test requires an account with custom-provider access.
The test keeps that SDK capability check. Use an owner-only token file; the helper
accepts its path and passes the credential through an inherited FD, without
putting its value in arguments or environment variables:

```bash
SDK_AUTH_ROOT="$(mktemp -d)"
(
  umask 077
  TOKEN_FILE="$SDK_AUTH_ROOT/token"
  trap 'rm -f "$TOKEN_FILE"' EXIT
  python3 -c 'import getpass, pathlib, sys; pathlib.Path(sys.argv[1]).write_text(getpass.getpass("Qoder token: "))' "$TOKEN_FILE"
  bash providers/observation/scripts/sdk_transport_auth_demo.sh \
    "$SDK_PYTHON" "$SDK_ENTRY" "$TOKEN_FILE" "$SDK_AUTH_ROOT/run"
)
```

The helper relays only exact official authentication/read-only bootstrap routes.
It rejects redirects and external inference, closes bootstrap before observation,
and removes its SDK HOME/TMPDIR on exit. The input token remains caller-owned;
the subshell above deletes its temporary copy. The tested account lacked the
required capability, so this case returned BLOCKED with no local TLS or capture.
An eligible account still needs a complete model/capture run. Review private
results under `SDK_AUTH_ROOT`, then remove that owned directory.

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
