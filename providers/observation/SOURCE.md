# Source and extraction audit

Base: ANOLISA main `0848c7bd50760175cf4b631faddf3e27e1b1f6b8`.
Reference: [Daydreamer-Li/anolisa](https://github.com/Daydreamer-Li/anolisa/tree/b723c17910f9b2537e939d50634c7b0277c998ed/src/agentsight/crates/agentsight-rewrite), commit `b723c17910f9b2537e939d50634c7b0277c998ed` (`feature/sight/rewrite-engine`).
Reference author: liyuqing <liyuqing@alibaba-inc.com>. Copyright 2026 Alibaba Cloud.
The reference repository and AgentSight component carry Apache-2.0; this package
uses the repository [LICENSE](../../LICENSE) and retains its [NOTICE](../../NOTICE),
including AgentSight's eunomia-bpf ancestry. Distribute those files with an extracted package.

`src/ptrace.rs` adapts the reference's checked ptrace-wrapper pattern and
register handling. The implementation was rewritten around per-thread hardware
execution breakpoints. Other modules are newly written against the inspected
reference behavior and existing AW contracts. No complete AgentSight crate,
daemon, probe subsystem or private source tree is copied. Cargo.lock fixes the
independent dependencies; no AW crate is linked.

| Reference path / finding | Extraction decision |
| --- | --- |
| `engine.rs`: payload/after-image printed at Info level | Metadata-only events; no payload formatting or writable target memory handle |
| 50 ms task rescanning before SEIZE | Initial stopped-thread fixed point plus TRACECLONE stop; bounded to 256 threads |
| Global int3 disarm, single-step, re-arm window | Per-thread DR0/DR1 execute breakpoints; RF suppresses only the resumed instruction |
| `ptrace.rs`: seize failures collapsed into false | Preserve syscall error and fail attachment; second debugger is explicitly rejected |
| `watchdog.rs`: restores bytes after tracer death | No code bytes written; no watchdog. Deterministic SIGKILL tests fail both after arming and during debug stops; default observation is denied |
| `remote.rs`, `rules.rs`: remote mmap and payload rewrite paths | Excluded because P1's crash-safety prerequisite fails |
| `maps.rs`: static prologue pattern fallback | Exact dynamic OpenSSL symbols via ELF PT_LOAD mapping; no guessing |
| Live uProbe int3 handling | Inspect entry byte and reject existing int3 without altering it; shifted software-breakpoint coexistence is not inherited |
| CLI `--offsets` match unreachable | Parser recognizes it, then returns `unsupported_offsets`; offset assets are outside the supported ABI |
| ITIMER_REAL / SIGALRM scan timer | Bounded WNOHANG loop; measured idle CPU is reported |
| Unbounded latency vector | At most 65,536 samples; bounded event/file/rate retention |

The hardware-breakpoint implementation is an intentional deviation from the
suggested software-int3 extraction. It removes the shared instruction race and
allows newly cloned threads to stop before enabling their own debug state. It
does not satisfy forced-tracer-death safety. The proof applies only to the
declared dynamic OpenSSL fixtures, with no claimed compatibility for real Agents.

The uProbe comparison is newly written, with no AgentSight BPF source copied.
Its small instruction sequence declares the kernel BPF program license as GPL,
following the repository's existing BPF loaders. Syscall layouts come from the
installed Linux UAPI headers. The experiment uses `BPF_TRACE_UPROBE_MULTI`, whose
PID-filtered attachment is exercised by the Linux 6.6
[uprobe multi self-test](https://github.com/torvalds/linux/blob/v6.6/tools/testing/selftests/bpf/prog_tests/uprobe_multi_test.c).
The earlier perf-PMU inheritance experiment is retained only as failed diagnostic
evidence outside the source package; it is not an available collection backend.

The bounded transport variant is newly written in `src/bpf/transport.bpf.c`,
`fixtures/transport_loader.c` and the Python projection/fixture modules. It uses
Clang's BPF target and the host's libbpf shared library (tested Clang 17,
libbpf 1.2.2); no vendored library is added. The loaded BPF program declares GPL
for kernel helper compatibility. Host library licensing remains with the system
package. Its x86-64 uprobe register layout is explicit; there is no kernel BTF
or guessed instruction-offset dependency. The loader uses Linux UAPI link
creation because the tested libbpf version lacks the multi-uprobe wrapper.
