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

Transport observation can run independently of native Hooks. Actual Qoder CLI
and SDK Worker TLS paths have not been validated here. The resolver recognizes
dynamic symbols in mapped `libssl.so`; static linking, other TLS libraries,
other ABIs or a different sender process may be unsupported. Product names or
the use of Node do not establish compatibility.

The current Source cannot fully replace Qoder unified Hooks. Digests contain no
tool name, call ID or model-message structure, and TLS fragments do not map
one-to-one to application events. Lower concurrency simplifies ownership and
capacity validation; semantic boundaries still need implementation. A future
fixed-version runtime-instrumentation Adapter could replace native Hooks by
intercepting actual input/execution/result boundaries, constructing unified
events, calling AW synchronously and verifying block or replace_result adoption.
That requires separate development and acceptance; SSL_write hits do not prove it.

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

Paced TLS fixtures pass, but actual Qoder/SDK attachment and a restricted
production pilot have not run. The lab helper loads operator-selected shared
libraries and BPF objects using existing sudo authorization; persistent use must
restrict this to reviewed assets. README commands launch synthetic fixtures only.
Completing this restricted acceptance can support a separately approved read-only
pilot without first meeting extreme concurrency or general production rewrite
requirements.
