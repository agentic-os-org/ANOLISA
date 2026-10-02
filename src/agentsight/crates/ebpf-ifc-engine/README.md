# ebpf-ifc-engine

**eBPF information-flow control engine for Linux.**

Kernel-level label propagation and policy rule matching across process,
file, and network boundaries. Loads prebuilt CO-RE eBPF programs via
[aya](https://aya-rs.dev/) — no clang or libbpf required at runtime.
[ActPlane](https://github.com/eunomia-bpf/ActPlane) uses this engine
for AI agent harness enforcement.

## How it works

Each node (process, file, network endpoint) carries a 64-bit label
bitmask. Labels propagate through fixed transfer functions at kernel
hooks:

| Hook | Propagation |
|------|------------|
| fork | child inherits parent labels |
| exec | process acquires source labels matching the binary |
| read/open | process acquires file labels |
| write | file acquires process labels |
| connect | endpoint acquires process labels |
| recv | process acquires endpoint labels |

Rules check accumulated labels at each hook. When a rule matches, the
engine emits a match event with one of three effects:

- **notify** — observe and report (operation proceeds)
- **block** — BPF-LSM returns `-EPERM` (pre-operation denial)
- **kill** — `SIGKILL` the matching task

The loader attaches hooks according to the loaded engine profile. Process
lifecycle and the argv-capable exec tracepoint are always attached. Exec scanning
is split across tail-call stages, so exact exec policies do not force the
verifier through prefix and conditioned-rule scanners in the same program.
Because argv tokens are observed after exec, argv-sensitive exec rules should
use `notify` or `kill`; they are not pre-exec `block` rules.

When BPF-LSM is active, the loader can also mark its own control pid as
protected. Runtime-domain subjects, including uid 0 subjects, cannot signal or
ptrace that protected pid, and they cannot use the `bpf()` syscall to create,
load, attach, pin, or fetch BPF programs, maps, or links. Lookup, update,
delete, and fd-info operations on already-held map fds remain available for
ActPlane's runtime control path.
Processes outside any ActPlane runtime domain
remain ordinary host administrators and can still stop or unload the engine.

## Kernel state

The engine uses separate maps for policy tables, process/domain state,
object labels, provenance, fd tracking, runtime control, and event output. The
most important maps are:

| Map | Purpose |
|-----|---------|
| `ts_updates`, `ts_rules`, `ts_counts` | Runtime-appendable compiled policy tables and active loop counts |
| `ts_proc`, `ts_proc_domains` | Global and runtime-domain process label state |
| `ts_root`, `ts_sess`, `ts_sess_zero` | Lineage roots and temporal gate/staleness epochs |
| `ts_file`, `ts_endp` | Per-domain file and IPv4 endpoint labels |
| `ts_file_prov`, `ts_endp_prov`, `ts_proc_prov` | Label provenance for corrective feedback |
| `cap_req`, `cap_state`, `cap_task`, `cap_policy` | Runtime domain and append admission state |
| `ts_fd`, `ts_fileptr`, `ts_sockfd`, `ts_mmap` | Tracepoint fallback fd, socket, and mmap tracking |
| `rb` | `TAINT_VIOLATION` ring buffer |

File identities are real `(dev,inode)` when hooks can recover a `struct file`.
Tracepoint-only path references fall back to a domain-scoped FNV-1a path id.

## Runtime model

The supported product entrypoint is the `actplane` CLI. The runtime installs or
opens one bpffs-pinned engine under `/sys/fs/bpf/actplane/v1` by default. Set
`ACTPLANE_BPF_PIN_ROOT` to use a different pin root.

The first runtime client installs and pins the maps, programs, and links. Later
clients open those pins and append domain-scoped policy deltas through pinned
control maps. Direct per-command private engine loading is not a supported
runtime model.

The daemonless runtime has a single active event reader. A `run`, `watch`, or
MCP auto-attach session holds the singleton runtime lock while it drains the
pinned ring buffer, and it clears policy/control-map state when that session
starts and exits. A second runtime session must wait or fail fast instead of
racing to consume the same ring-buffer events.

The `ebpf-ifc-engine` crate remains the low-level kernel ABI boundary used by
the runtime. Normal callers should use the CLI and runtime crate instead of
loading eBPF programs directly.

## Building the eBPF programs

The prebuilt CO-RE object ships in `prebuilt/process.bpf.o`. To rebuild:

```bash
# Requires: clang, llvm, libelf-dev, zlib1g-dev
cd bpf && make
```

Or via cargo:

```bash
ACTPLANE_REBUILD_BPF=1 cargo build -p ebpf-ifc-engine
```

## Binary config format

The compiler writes a fixed-size `taint_config` blob. The struct layout is
defined in `taint.h` and mirrored byte-for-byte in Rust (`lower.rs`). It
contains:

- `n_updates` plus up to 320 `taint_update` entries. Updates cover sources,
  declassify/endorse transforms, temporal gates, and `since` invalidators.
- `n_rules` plus up to 128 `taint_rule` entries. Boolean `or` clauses are
  lowered into multiple kernel rules.

The loader copies those entries into writable BPF array maps so admitted
runtime policy deltas can extend the active policy without rebuilding the eBPF
object.

## Requirements

- Linux kernel 5.8+ with BTF (`/sys/kernel/btf/vmlinux`)
- Root or `CAP_BPF` + `CAP_SYS_ADMIN`
- BPF-LSM active for `block` effect (`bpf` in `/sys/kernel/security/lsm`)

## Used by

- [ActPlane](https://github.com/eunomia-bpf/ActPlane) — programmable
  OS-level policy engine for AI agent harnesses

## License

MIT
