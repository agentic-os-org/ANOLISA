# Transport observation in AW unified Hooks

[中文版](ARCHITECTURE_NOTES_zh.md)

This package supplies optional process-level transport evidence to AW. The next
target is read-only observation for pinned versions, a small number of Workers
and low-rate tasks: identify TLS writes, their lengths/digests and capture
coverage. BPF uprobes are the candidate collector. The ptrace experiment remains
disabled by default because killing its tracer can terminate the target.

## Qoder and Qoder SDK boundaries

The current [Adapter catalog](../../src/aw/crates/aw-adapter/src/adapter/catalog.rs)
declares `input.submit`, `tool.before` and `tool.after` for both adapters. Qoder
CLI uses native `UserPromptSubmit`, `PreToolUse` and `PostToolUse` callbacks. The
SDK Bridge manages daemon/Query lifecycle and uses native callbacks and execution
boundaries for checks and effect adoption. `model.before_request` remains planned
in the [capability reference](../../docs/user-guide/en/aw/capabilities.md).
Mode-specific limits belong to the [Qoder](../../docs/developer-guide/en/aw/agents/qoder.md)
and [SDK](../../docs/developer-guide/en/aw/agents/qoder-sdk.md) guides.

| Requirement | Current Source evidence | Requirement without native Hooks |
| --- | --- | --- |
| TLS write observation | PID, thread, time, length, digest, loss and Coverage | Confirm the actual sender uses supported dynamic OpenSSL for each version |
| Instance/Scope correlation | Private importer tested with real AW fixtures | Legal instance lifecycle and read-only identity export; PID discovery cannot establish Ready |
| `input.submit` observe/block | No submission boundary, input object or synchronous receipt | A new application input boundary with block adoption |
| `tool.before` observe/block | No tool identity, arguments or pre-execution stop | A semantic execution boundary; local tools may produce no TLS write |
| `tool.after` observe/replace | No tool result or authorized replacement slot | A result-delivery boundary with adoption confirmation |
| Model request checks/rewrite | TLS entry metadata with unknown send adoption | Request reassembly, protocol semantics, authorized actions and receiver validation; unimplemented |

Transport observation runs independently of native Hooks. Real LangChain model
calls now pass capture validation. Claude Agent SDK completes its local model
call with an opt-in fingerprint-pinned static TLS profile. Its dynamic-libssl-only
control remains unsupported. One interactive Claude Code / Token Plan turn also
completed with TLS metadata captured. Qoder's
authenticated direct-model fixture is blocked by the tested account's custom
provider capability; its transport path remains unverified. See the
[measured SDK matrix](POC_REPORT.md#sdk-boundaries-without-native-hooks).
The resolver recognizes mapped `libssl.so` symbols; static linking, other TLS
libraries and other ABIs require separate support. Static support is restricted
to the single validated binary profile; unknown builds fail admission.

## High-code SDK tools: two enforcement boundaries

A tool that computes in memory can produce no TLS or OS operation with tool
semantics. A transport digest contains no tool name, arguments or call identity.
The following experiments therefore validate their own execution boundaries:

| Mechanism | Verified behavior | Remaining work for unified Hooks |
| --- | --- | --- |
| External OS launcher | Across Qoder SDK, Claude Agent SDK and LangChain dispatchers, seccomp denies exec and write-open operations | Define the supported side-effect policy and lifecycle; tool bodies already entered, memory and inherited-FD effects remain possible |
| Version-pinned runtime injection | LangChain sync/async `BaseTool.run/arun` checks block before tool entry without native callbacks | Build a registered Adapter with call identity, arguments, synchronous AW decisions, timeout policy and adoption checks; direct function calls bypass the current guard |
| Read-only TLS Source | Captures supported sender writes without SDK hooks | Supply provenance and Coverage to a query consumer; no automatic tool invocation or enforcement boundary |

This supports continuing with a controlled-start, fixed-version runtime Adapter
for semantic checks, supplemented by OS side-effect restrictions and transport
evidence. The experiment does not intercept every arbitrary already-running
Agent. Qoder and Claude runtime tool-entry guards remain to be tested in their
full Agent loops; current cases exercise actual SDK tool dispatch only. Qoder's
and Claude's existing AW MCP adapters already place checks before registered tool
handlers, providing an implementation reference for a new optional entry adapter.

For a small first scope, support one known tool dispatcher and one active Query.
Admission must declare that exact path. Reject an unsupported SDK version or
missing guard before admitting the protected task; do not infer universal tool
coverage from process discovery. The current fixture guard only returns a preset
block result and has no AW control authority. This experiment leaves production
Adapter contracts and native capabilities unchanged.

## Framework responsibilities

The proposed integration path is:

```text
Authorized Qoder / SDK Worker
  -> independent BPF Source -> bounded private metadata store
  -> read-only Instance/Scope correlation -> Observation Provider query/projection
  -> AW context evidence, diagnostics or policy inputs at existing Hooks
```

The Source continuously collects; a future `aw-provider-observation` performs
bounded queries/projections within the existing per-invocation Provider
lifecycle. Native Hooks may consume this supplementary evidence at their own
boundaries. Disabling native Hooks leaves no automatic `tool.before` trigger.
A separate transport event consumer first needs reviewed provenance, admission
and Coverage contracts; this package's private schema is not a registered Hook.

The Source holds authorized process descriptors, without the full AW control
socket. A future identity bridge exports generation-bound validity intervals
and revocation, including the actual sender Worker's relationship to its owner.
The fixture preserves owner identity through same-PID exec; real SDK child-Worker
mapping still needs implementation and validation. Multiple instances on one PID
stay ambiguous; unassociated processes stay Observed. A single Query still
requires verified identity and validity intervals.

Events retain source, ProcessRef, monotonic clock domain, digest range and loss
accounting. Entry digests cannot establish receiver adoption. Missing final
summaries make interval loss unknown. Read-only evidence grants no execution
authority.

## Restricted pilot

The first candidate is one host, pinned Qoder/SDK/Worker versions, one managed
Worker and one active Query, used for diagnostics and supplementary audit.
Allowed gaps must be visible; this is not a sole security-enforcement or complete
audit source. Large-scale concurrency and extreme throughput can follow later.
The first acceptance should establish usefulness on real low-rate tasks.

| Next step | Restricted acceptance |
| --- | --- |
| Locate the real boundary | Identify sender PID, TLS library and symbol; confirm hits using authorized synthetic tasks, without production body capture |
| Verify ownership | Match real AW registration, owner/Worker identity and Query lifecycle; reject stale mappings across exit/restart |
| Measure the chosen workload | Reconcile hits/loss and measure latency, CPU/RSS under its task rate and bursts; declare the sustainable range |
| Manage collection lifecycle | Start with windows of at most 30 seconds; persistent use needs bounded rotation, status export and restart-gap handling |
| Fix the privilege entrypoint | Pin reviewed program/object assets, constrain attachment targets and handle helper/collector exits and permission failures |
| Connect the query consumer | Implement the read-only identity bridge and independent Provider; test missing/unknown evidence consumption |

Paced TLS fixtures, LangChain model transport and the pinned Claude static entry
pass; Qoder model transport,
the pinned Claude workload envelope and a restricted production pilot remain open. The lab helper loads operator-selected shared
libraries and BPF objects using existing sudo authorization; persistent use must
restrict this to reviewed assets. README commands launch synthetic fixtures only.
Completing this restricted acceptance can support a separately approved read-only
pilot without first meeting extreme concurrency or general production rewrite
requirements.
