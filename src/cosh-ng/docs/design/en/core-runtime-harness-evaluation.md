# Cosh AgentRuntime Evaluation Basis and Decision Record

[中文版](../zh/core-runtime-harness-evaluation.md)

Date: 2026-11-14. Status: design research conclusions; unimplemented and not accepted through runtime or release validation.

Companions: [Architecture design](core-runtime-harness-design.md), [Phased plan](core-runtime-harness-implementation-plan.md).

## 1. Core Judgment and Necessity

Cosh needs a small Rust agent runtime suited to a Linux Shell, rather than another Coding CLI. Retain existing provider/tool/Hook/Skills/MCP/session/compaction/audit capabilities and connect Shell through an official ACP frontend in the same process. Add only negotiated host-shell operations for the current persistent PTY. Internal cleanup covers host interaction, fact projection, and cancellation/finalization within a run, without a second execution loop or generic runtime platform.

ACP standardizes client/Agent interaction, not a single internal loop, compaction, database, or sandbox implementation. A “standard AgentRuntime” does not require replacing the runtime with a framework. Verifiable standardization here means official protocol implementation, explicit capabilities/execution responsibilities, and boundaries proven through cross-implementation interoperability.

| Change | Necessary? | Actual consequence of omission | Adoption condition |
|---|---|---|---|
| Concrete result-pairing, final-action validation, cancellation gaps | Prioritize after reproduction | Possible incomplete history, false success, or incorrect approval binding | Independent fixture FAIL→PASS per issue; static inference is not reproduction |
| ACP Shell/Core boundary | Required for the user's explicit bidirectional replacement goal | Private coupling remains; different clients/Agents need custom adapters | Official SDK wiring, baseline compliance, foreground fidelity, staged real interoperability |
| Minimal host interaction interface | Needed for ACP, not a reason for wholesale refactoring | Direct stdin waits conflict with asynchronous bidirectional calls | Equivalent original path; interfaces serve existing callers only |
| Short prompts, final-tool consistency, request-scoped stagnation | Worth testing first; benefit is not automatic | Conflicting guidance and unnecessary calls may persist | Static matrix and small real-model comparisons; adopt only without regressions |
| New runtime crate, event bus, policy DSL, workflow serialization | Not needed now | No demonstrated loss | Revisit only with two real consumers and concrete benefit |
| Restricted helper, cross-turn scope, additional review model | Not a base requirement for this round | Some automatic diagnostics still need foreground review | Independent real demand, runtime boundaries, and demonstrated net benefit |

Expected effects are less protocol coupling, clearer execution ownership, more trustworthy cancellation/results, and prompts consistent with tool capabilities, while the default remains the existing two Shell/Core processes. Fewer approvals, lower cost/latency/memory are unmeasured benefits, not percentage commitments. ACP itself neither improves model capability nor guarantees read-only or exactly-once execution.

## 2. Evaluation Method and Evidence Levels

Four independent evaluation domains covered standards/SDK, runtime/reference source, PTY/safety, and task experience/cost. Independent judgments preceded cross-examination with counterexamples; the lead evaluator then selected changes based on source and requirements. Agent votes are neither evidence nor product acceptance. Existing Qwen, Pi/Rig, and Goose/OpenCode research informed the work without making earlier drafts conclusions that had to be defended.

The source baseline is ANOLISA `1300cfcd50938af9323c0c5df4faaaf00ff6be60`, not an old local checkout or installed RPM. Reference repositories were fetched locally and pinned; the workspace inventory is `third-part/cosh-runtime-review-20261113/source-manifest.json`. Research paths and temporary snapshots support local traceability only. Durable references use repository-relative paths at fixed revisions and do not depend on temporary directories surviving.

| Evidence | Supports | Does not support |
|---|---|---|
| Pinned source and static counterexamples | Responsibility boundaries, possible paths, next reproduction steps | Executed FAIL, actual leakage, or a complete safety proof |
| Existing local tests | Behavior of the corresponding fixtures | New architecture, Linux PTY, other peers, or correct installation |
| Independent Agent questioning | Exposing assumptions and narrowing scope | Model agreement as truth or acceptance |
| Subsequent real Linux/model/peer/RPM checks | Behavior of the specified candidate/environment | Untested images, permissions, versions, or release scope |

Historical research checks: 33 targeted ACP passes on macOS, four failures before startup due to Linux descriptor-pinning requirements, exit code 101; two foreground serialization passes; another test filter matched zero, which is not a pass. No real provider was called and no new-architecture Linux PTY/bidirectional interoperability/RPM tests ran. All new counterexamples remain static review findings without complete end-to-end reproduction.

## 3. Source Risks, Independent Optimizations, and Existing Mechanisms to Preserve

Paths below are relative to baseline `src/cosh-ng/`; line numbers locate that revision and must be checked again during implementation.

| ID | Static fact and precondition | Impact and recommendation | Acceptance oracle |
|---|---|---|---|
| S01 | `crates/cosh-core/src/core.rs:1852–1869` records the current result after interrupted and directly returns Completed; 1195–1204 fills remaining call results after fatal, while 1874–1887 handles post-batch terminal state. Also inspect success mapping in `headless.rs:878–889` | An assistant declares A/B together; interruption during A's decision may omit B's pair and report success. Reuse the batch finalizer after reproduction | Every declared ID has a result; subsequent tools never start; interruption is distinct from task success |
| S02 | `core.rs:1346` classifies first; 1424–1437 merges Hook patches using the old outcome; 1445–1449 can execute directly; the audit input hash at 1247–1255 also precedes patching | Trusted Hook changes may leave final-argument classification/audit stale. Revalidate, reclassify, and bind the final action while preserving Ask/Block priority | Zero executions of that final sensitive write before approval; card/audit match final arguments; rejection never downgraded |
| S03 | `core/provider_access.rs:699–700` directly awaits without trial access; `core.rs:692–699,762–763` uses this path; outer Shell still has grace/kill in `cosh_core_service/runtime.rs:195–215` | Existing outer fallback does not replace cooperative cancellation in the shared kernel. Propagate cancellation through minimal host tasks | Every provider wait is cancellable; outer kill remains effective; never claim dispatched actions did not execute |
| S04 | `core.rs:2282–2304` retains only some host content/exit fields; missing exit at 1569–1577 does not produce is_error; `provider/mod.rs:127–134` drops `_is_error` | First carry existing signal/coverage/cleanup through; missing exit codes do not prove success. Separate domain facts from provider-supported wire fields | Missing exit/signal, truncation, and incompleteness retain actual state; model/UI never fabricate success |
| S05 | `loop_detect.rs:3–5,18–35` uses argument prefixes; detector is a Core field, with no reset evident at new-user entry `core.rs:388–393`; 1858–1860 only warns | Use request scope, complete final-action/result digests, and limited warnings; no identical-result hard stop | Different argument tails remain distinct, new requests reset, bounded polling never falsely stops |
| S06 (corrected) | `hook.rs:1494–1523` enforces the same BeforeModel count/names/order; 1531–1547 enforces no estimated-token growth; original-declaration budgets remain conservative upper bounds | Withdraw the earlier gap claim that Hooks remove tools or expand declarations and cause mismatches; preserve the existing compaction contract. Final measurement and trusted-profile filtering are independent requirements, not grounds to add filtering here | V03: reject invalid updates; valid compaction preserves the set and upper bound; if separately adopting profile filtering, apply it before prompts/budgets; never present the earlier misreading as a FAIL→PASS defect |

Precise S02 safety scope: Auto ShellExec normally already requires RequireApproval, so do not claim a read-only shell patched into a write directly bypasses approval. Ordinary `write_file(note.txt,"safe=true")` may initially be Allow under legacy Auto, then an enabled trusted Hook patches it to fake data `MII...\n-----END RSA PRIVATE KEY-----`. Preconditions: allowed_tools excludes write_file, aggregate Hook decision is Allow/Passthrough, and no Ask/Block/HookFailure occurs. The isolated END marker is sensitive under `redaction.rs:137–141`, but `redaction/private_key.rs:45–67` does not redact it as a BEGIN block; WriteFile at `tool/write_file.rs:75–94,623–649` may warn and still write this fake data. Reclassification should Ask; the current path may retain Allow. Ordinary AKs or complete private keys are redacted first, and the write tool rejects placeholders, so they cannot establish actual secret leakage. This issue concerns inconsistent final-action binding, not proven untrusted privilege escalation or leakage.

These are not capabilities to build from scratch: preserve existing max_turns, Stop Hook budgets, argument-correction budgets, rejection of missing MessageEnd/incomplete tool arguments, remaining-tool completion after fatal, BeforeModel same-set/no-growth checks, separate transcript/compaction projections, checked emit, owner receipts, and persistence ordering. Reuse first; refactoring must not weaken them.

## 4. What to Borrow from Harnesses and What to Reject

| Reference | Selected mechanism and local application | Not adopted and why |
|---|---|---|
| Qwen Code `packages/core/src/core/agent-loop.ts` | Shared model/tool batch entry, discard abandoned attempts, ordered tool progress; ACP/legacy share one loop | Wholesale Node/coding-session/ACP-branch migration conflicts with Linux Shell cost and existing owners |
| Pi `packages/agent/src/agent-loop.ts,types.ts` | Separate model projection/UI details; tool-dependent short prompts; final requests/host metadata | No default parallelism or Node migration; retain Cosh compaction/transcript |
| Goose `crates/goose-agent/src/machine.rs` | Progression/effect boundary and cancellation finalizer; host interaction/finalization | No alpha GDK/SessionManager/new database or complete state-machine platform |
| Rig `crates/rig-agent/src/run/mod.rs` | Small stages and selectively reusable provider-interface ideas | Serializable runs do not solve side-effect replay; MSRV 1.97.1/dependency upgrades cost effort; no wholesale adoption |
| OpenCode `packages/core/src/session/runner/step.ts` | Cooperative cancellation, output drain, terminal-state separation; bounded finalization | No Bun/Effect or Job/Inbox/Location migration; timeouts never automatically background actions |

Architecture design sections 5 and 14 contain revisions and pinned source links. Adopt a few mechanisms and their failure boundaries, not the reference projects' feature counts as a completeness measure. Existing ContentGenerator, tool registration, process cleanup, and evidence storage remain the implementation foundation.

## 5. Decisions Revised through Cross-Examination

| ID | Initial candidate/objection | Counterexample and final decision |
|---|---|---|
| D01 | Standard permission before host execute to maximize standard messages | Adds loss/cancellation/duplication states between approval/execution; the standard does not require this permission before client execution. Use one execute: Shell-local validation/card/one-time dispatch/actual result; owned only registers responsibility. Native tools retain standard permission |
| D02 | Standard terminal with currentShell metadata instead of an extension | create makes a new terminal with background execution; release must terminate unfinished commands. Shared user-Shell top-level eval, serialization, and OSC still need a contract. An explicit host-shell extension is thinner initially; standard terminal remains for independent profiles |
| D03 | command/cwd/env digest freezes the full approved environment | A finite digest cannot freeze aliases/functions/scripts/external files. Bind actual connection/PTY/action, approve displayed text in the actual current Shell, and add expected cwd if needed; no full-state generation system |
| D04 | Hard-stop persistent identical actions/results | Repeated activating can be valid; changed timestamps need not mean progress. Initially use limited warnings + max_turns; invalid arguments/rejection/unknown side effects retain existing hard boundaries; no semantic progress engine |
| D05 | Reject every late approval; interrupt or retry on Core death | Existing valid local cards support late-decision recovery. Undispatched/uncancelled actions on the same PTY may run locally after explicit notice, without reviving old RPCs. Shell retains running ownership; Core death never causes automatic replay |
| D06 | Clear all descendants before one terminal state | Normal background jobs/Hook services are legitimate. `crates/cosh-core/src/process.rs:225–237,755–803` explicitly preserves successful background descendants. PTY settlement uses foreground command boundaries; any adopted helper-wide cleanup applies only in its own domain |
| D07 | Return standard cancelled immediately after dropping a future | ACP requires ongoing operations to stop successfully before returning cancelled. UI may cancel first, independently of execution facts; unproven termination requires accurate failure/unknown, not normal cancellation |
| D08 | Latest SDK major means workspace-wide upgrade; SDK reuse is inherently bounded | Both 2.0/5.2 have serial callbacks and line reads/unbounded internal channels. Verify existing official connections first, comparing local 5.2 adoption and necessary-fix benefits; no custom wire or silent global upgrade |
| D09 | Build evidence/read everywhere; resource_link implies retrieval | ACP has no generic Agent resources/read; fs/read_text_file points in a different direction from Core ledger access. Start with bounded content/references/local ledger; study retrieval when needed |
| D10 | Behavior eval only after acceptance of the full migration | Original-path fixes/prompts can land independently; early paired real-model smoke in H1, one real peer for H2 development exit, and two independent implementations/RPM only for H4 broad compatibility/default switching |
| D11 | Auto draft is already in development, so adopt it as the new default | Investment proves neither necessity nor correctness. Improve deterministic flow first; test helpers independently with one explicit input set; scope/revoke/purpose platforms and reviewers need separate benefit evidence |
| D12 | Minimal host execute carries only command and owner | Existing Hook Ask also uses hook_requires_approval to prevent auto/trust/turn consent overrides; the extension must preserve trusted mustAsk semantics. Missing/unknown never means false, and false grants no automatic permission; verify with V01 |
| D13 | OSC tokens prove the current Shell's actual execution boundary | Same-Shell code can read the token and call emitters; initially provide only cooperative boundaries, with no promise against same-Shell forged markers. V02 characterizes actual blocking after a valid precmd; unprotected paths never count as passes, and profiles requiring stronger boundaries are rejected |
| D14 | BeforeModel can remove tools/grow declarations, so budgets must be reordered | The earlier judgment was wrong: existing validation forbids this, keeping budgets conservative. Preserve protections; adopt accurate measurement and independent profile filtering only as required, without relaxing constraints for an incorrect oracle; verify with V03 |
| D15 | Direct ACP in A necessarily adds an IPC hop; B's internal interface is inherently fast and already provides complete standard access | Both A/B can retain two processes and one kernel. B needs no proxy but retains an extra wire long term; either route needs an ACP Client for Shell to connect to other Agents. Measure both costs; incremental near-term wiring does not precommit to A as default, and long-term B's objective differences require a separate decision |

Source basis for these three revisions: mandatory Ask enters requests at `crates/cosh-core/src/core.rs:1455–1489`, is preserved by Shell `approval/requests.rs:217–276`, and blocks automatic/batch approval through `agent/approval_bridge.rs:44–46,209–211` and `approval/resolution.rs:135–140`. Same-Shell marker visibility is in `shell_host/marker/bash.sh:12–13,334–363`; parsing and precmd settlement are in `shell_host/osc.rs:309–319,410–470`. These are static sources. No new complete-chain fixtures ran in this round, so neither reproduced bypasses nor completed protection are claimed.

Re-review clarifies two compatibility details: an invalid later BeforeModel Hook update preserves the previous last valid declarations (`crates/cosh-core/src/hook.rs:852–873`). Old Core legitimately omits false (`protocol.rs:488–489`), normalized under the old version contract by an identified legacy adapter (Shell `adapter/control_protocol.rs:411–414`); new host-shell required fields cannot apply directly to old ordinary requests. V01/V03 cover the corresponding positive and negative cases.

Independently checked side-chat input informs the A/B comparison without authorizing implementation. [Qwen's official architecture](https://qwenlm.github.io/qwen-code-docs/en/developers/architecture/) describes direct runtime calls from TUI/headless, with ACP/daemon as another entry. [Qoder ACP](https://docs.qoder.com/cli/acp) establishes only external stdio access, not all internal UI paths. [ACP transport](https://agentclientprotocol.com/protocol/v1/transports) confirms JSON-RPC/newline framing without establishing A/B performance. Cosh has not run this comparison and claims neither path is faster.

## 6. ACP Adoption Gates and Extension Boundaries

Official references are the pinned [ACP v1](https://github.com/agentclientprotocol/agent-client-protocol/tree/3e4ce6468d68d89536b847628f6ff55239565583/docs/protocol/v1) and [Rust SDK](https://github.com/agentclientprotocol/rust-sdk/tree/91d368ad8b6761f314da9095c4a030d365c079a9) repositories. SDK 5.2.0, Rust schema crate 1.17.0, and specification-repository schema package 1.31.0 are separate release axes; the selected wire remains stable v1, not inferred v2.

Also checked the currently pinned registry source: SDK 2.0.0 `src/concepts/ordering.rs:9–21,43–58` likewise specifies serial callbacks and reentrant deadlock risks, while `src/jsonrpc.rs:1703–1705,5576` also has unbounded channels and line-reading issues. Old schema 1.5.0 `src/v1/content.rs:39–43,52–55` already mandates text/resource_link, and `src/v1/agent.rs:2834–2838,4975–4982` already defines stdio MCP and new-session responsibilities. These mandatory items are not new upgrade requirements inferred from the latest documentation. Verify existing official connections first, then compare local 5.2 integration costs and necessary fixes. Stable elicitation is available in 5.2; it cannot be claimed as a stable 2.0 baseline guarantee.

| Gate | Official source and required judgment |
|---|---|
| Baseline compliance | Mandatory text/resource_link in `initialization.mdx`, stdio MCP in `session-setup.mdx`; test each pinned MUST; incomplete implementations claim only a restricted profile |
| Connection scheduling | SDK `concepts/ordering.rs`: receive callbacks are serial. Short callbacks register work; model/UI/PTY waits leave the receive loop; official responders correlate replies; no cancel/reverse-call deadlocks |
| Memory/task bounds | `jsonrpc.rs` ByteStreams `.lines()` and transport-actor unbounded channels; validate pre-read frame limits, admission/queue/task budgets, and overload exits with real slow consumers |
| Old/new SDKs | 2.0/5.2 APIs/features/batch support differ; ACP stdio single-message behavior is the new boundary; evaluate old Gateway codec compatibility separately rather than copying/deleting it incidentally |
| User questions | Prefer standard elicitation in the selected stable schema; an old SDK's unstable requirement does not justify a private generic ask protocol or false stable-support claims to old peers |
| Cancellation | Separate UI cancellation projection from actual-abort requirements for prompt cancelled in `prompt-turn.mdx`; abnormal failure without abort does not count as normal-cancellation compliance; no custom Unknown stopReason |
| Terminal | Preserve create/output/wait/kill/release semantics in `terminals.mdx`; never impersonate the user's current Shell |
| Naming | `_` methods and controlled reverse-domain namespaces in `extensibility.mdx`; register naming authority; provisional responsibility names are not published APIs |

Permission requests are cooperative and cannot constrain an external Agent's executor. Confirm execution restrictions for hard analysis-only/execution-disabled profiles before sending session/prompt. Do not infer them from modes, prompts, metadata, or disabled terminal/fs. Explicitly reject such profiles without verifiable peer constraints; ordinary interoperability may still use the peer's actual independent execution semantics.

## 7. Implementation Order, Benefits, and Stop Conditions

1. Start with corresponding fixtures, pinning the actual candidate/existing behavior. Verify and fix S01–S05 separately; preserve S06's existing protections and validate only independent optimization requirements, rather than treating it as a known defect to fix.
2. Independently compare prompts/request projection using small real-model samples with identical input, tools, parameters, and budgets; fewer bytes do not establish cost savings.
3. Clean up only ACP-required host interaction; validate one reader, cancellation, and domain projection while retaining one loop.
4. Run official ACP adoption experiments, mandatory-input/MCP checks, one real peer, and fault peers without changing the default.
5. Connect one host execute/owned to the original PTY; test approval, input waiting, signals, partial writes, late decisions, disconnects, and deaths.
6. After broader task samples, two independent implementations, target installation/upgrade/rollback checks, and A/B comparisons, record the adoption decision. Switch an explicit scope's default only with actual delivery authorization; otherwise retain the original path, retiring legacy later using evidence.

See H0–H6 for scope, risks, and oracles. Reject frameworks/abstractions without demonstrated Cosh benefit. Keep unknown SDK boundaries, unreproduced counterexamples, and model variability as gaps instead of hiding them behind more components. Abandon helpers/reviewers without sufficient benefit; ACP mainline is unaffected. Freeze performance targets before measurement, never relax thresholds afterward.

## 8. Open Questions and Delivery Status

| Open question | Latest resolution point | Current status |
|---|---|---|
| S01–S05 end-to-end reproducibility, scope, fixes | Before corresponding H1 changes | Static evidence, not accepted runtime defects; S06 corrected to independent optimization and existing protections |
| Cross-protocol mustAsk fidelity and OSC trust-domain limits | Before H3 specification and G3 | Document boundaries and V01/V02 defined; fixtures unimplemented/unrun; same-Shell anti-forgery is outside initial guarantees |
| SDK5.2/old2.0 dependency wiring, reentrancy, end-to-end bounds | H2 selection exit | Recommended target/adoption gate, not measured |
| Final namespace, error codes, field bounds, instance-identity reuse | Before H3 wire specification | Responsibilities defined; names/fields unpublished |
| Esc/foreground-signal product policy and ACP finalization mapping | H3 behavior exit | Preserve current behavior; no silent merging of Ctrl-C semantics |
| Peer versions, image/identity/ARM64/installation coverage, numeric budgets | Corresponding phases and before default switch | Execution records required; no invented completion |
| Prompt net benefit and whether auto warrants another backend | Independent H1c/H6 exits | Await real samples; narrowing/abandonment remain options |

This round produces bilingual architecture, plan, and evaluation documents. Product code, dependencies, and default behavior are unchanged. Document checks/review do not substitute for implementation, interoperability, Linux PTY, RPM, or release acceptance.

Technical open questions do not mean waiting for item-by-item user authorization. [Plan section 11.1](core-runtime-harness-implementation-plan.md) defines the scope of implementation, optional-experiment adoption, and actual delivery. This document work has no unresolved user-input dependency.
