# JS final tool-result gate and compatibility

[中文版](../zh/js-final-gate-compatibility.md)

This design covers a tool result about to enter an Agent's model context. A
tool may already have caused side effects. The gate controls delivery of its
result and retains the existing `security.violation` Provider operation.

## Boundary and shared runtime

```text
native tool -> native Post/Tokenless -> native finalization
  -> verified final-result checkpoint -> AW security.violation
  -> allow original snapshot | withhold -> model-visible commit
```

The checkpoint must run after the last supported transformation and before the
first model-visible commit. It passes the exact result snapshot to the existing
Provider and either commits that snapshot or a safe withheld result. Later
mutation invalidates the decision. Unknown result shapes and paths cannot be
admitted as covered. Earlier same-process observers remain outside this boundary.

The shared Node runtime owns three operations: feature-probe
`node:module.registerHooks`, verify the exact loaded module SHA-256 and unique
patch anchor, and remove its own launch-scoped `--import` from `NODE_OPTIONS`
before tool and Provider children start. Each adapter retains its own patch
anchor, identity mapping, result projection and native callback. An adapter
reports `registered`, `instrumented`, `verified`, `active`, or `unsupported`
with package, Worker, entry, module digest and covered consumer. A readiness
file or Hook registration establishes only `registered`.

An admitted result identity includes the AW instance, process, session, tool
call and result generation; a native turn or batch identity is included when
available. A missing or stale field withholds the result under a required gate.
No public configuration field or Provider protocol is needed for this phase.
`tool.after` replacement runs before the final check. Empty policy has no
preload, final callback or extra per-tool process.

## Published release evidence

| Agent | Published build | Observed sequence | Final-gate status |
| --- | --- | --- | --- |
| DeepSeek Harness | 0.2.1-rc.5 | `finalize/finish` -> exact-build preload -> top-level Session commit | Verified for the existing headless tests only |
| OpenClaw | 2026.9.5, 2026.9.6, 2026.9.7 | middleware -> `after_tool_call` -> context conversion -> provider payload hook -> HTTP request | Verified for the owned embedded Gateway and explicit OpenAI-completions path below |
| Qoder SDK | SDK 1.0.55/Worker 1.1.75, 1.0.56/1.1.78, 1.0.57/1.1.81, Linux arm64 | `PostToolUse` -> `updatedToolOutput`/Read rendering/additional context -> Worker result delivery | Partial release inspection and parser acceptance; final gate unsupported |

The tested OpenClaw middleware runners await each handler and pass the
previous result to the next. Its built-in `tool_result` extension applies that
runner, then the observed `after_tool_call` callback runs. The local Gateway
fixture records an early middleware observing a safe result, a later middleware
injecting `AW_FINAL_GATE_SECRET_MARKER`, and the subsequent model HTTP request
containing that marker. This is a red baseline for an early-middleware check.
The `tool_result_persist` hook changed the persisted transcript in a probe,
but that change was absent from the next model request; it is not a substitute
for model-request capture.

The verified OpenClaw path uses two launch-scoped checkpoints. The exact
published Agent core return from `convertToLlm` checks source identities,
text-only results and provenance of non-tool messages after context conversion.
The exact `@openclaw/ai/dist/transports.mjs` OpenAI-completions branch checks
the final HTTP payload after `onPayload` and `before_provider_request` handlers,
and, on 2026.9.7, after the asynchronous native body encoder. It submits each actual tool
message text to the existing `security.violation` Provider and replaces blocked
or failed results with a safe text envelope. The transport sends an owned
snapshot; 2026.9.7 sends the checked JSON as an immutable body string. Changes
to non-tool messages, tool metadata or other payload fields stop the model request.
Session teardown or invocation replacement during asynchronous checking invalidates
the decision before any result is published. These two checkpoints
prevent a context converter from copying tool output into a user message and
prevent a later payload hook from bypassing the Provider.

Admission checks the actual `openclaw.mjs` entry, Agent core, the built-in
converter, session projection, call-ID normalizer, and the installed
matching `@openclaw/ai` transport and serializer bytes. Node verifies the two
loaded patch targets again with exact SHA-256 and unique anchors. The native
2026.9.6 patch-target digests are `96221c65ea8e15d6499ca9f817e5d7629151b42c6d9742000791f2f48f2cebcd`
for Agent core and `038e2c72e79a3e1c9d06e9c5f3a61a13acc297e1026696da24fee634a3bc4abc`
for the transport. The entry digest is
`4dd36d2ed0cb154456c4a09a951a2e8f2508a5dd6ff2fe0688d4e1c8f773fcb5`.
2026.9.5 and 2026.9.7 have separate exact entry, Agent core, transport and
dependency fingerprints in `adapters/openclaw_bundle.rs`; their model and transport
anchors are checked independently. The native
result middleware only records session, run, call and tool name. OpenClaw
registers the plugin twice at Gateway startup, so both registrations share the
launch's correlation map. The pinned transport removes punctuation from some
call IDs; AW accepts that mapping only when it uniquely matches one pending
call in the same session and tool. Ambiguous and missing identities are withheld.
An identical, previously checked payload can reuse its digest receipt; changed
historical results are withheld. The active model API must be
`openai-completions`; admission also requires an explicit primary model using
that API and no configured fallback. A different runtime API stops before the
model call.

The stream resolver is also fingerprinted and instrumented. Only the native
`boundary-aware:openai-completions` strategy is admitted; Provider stream
overrides and custom session streams are rejected before dispatch. Transcript
records without a native tool identity (`bashExecution`, custom messages,
branch and compaction summaries) stop projection before conversion. Session
teardown or request cancellation kills the active callback and skips later
steps; an interrupted final event is explicitly closed.

The native Gateway fixture runs allow, a later middleware injecting
`AW_FINAL_GATE_SECRET_MARKER`, Provider failure and Provider timeout. The Provider sees the
exact serialized tool text; the local model HTTP request receives only the
withheld envelope for that result. An unchanged safe result reaches the model.
The same Gateway path also passes with the real `agent-sec-core`
`check_final_tool_output` Provider and a deterministic local PII scanner.
The earlier `tool.before` denial still prevents its tool side effect. This
proof covers the tested OpenAI-compatible serializer. A unit test also checks
payload mutation after serialization; a native extension that uses
`before_provider_request` has not yet been exercised. Other model APIs,
reloadable policy, multimodal results, context projections that remove a tool
result identity, nested agents and background delivery remain unsupported.

All three inspected Qoder Workers call `firePostToolUseEvent` before applying
`getUpdatedToolOutput()`/`getUpdatedMCPToolOutput()`. The same return path can
rewrite `llmContent` for Read and append Hook context. The shipped Worker is an
obfuscated, separately versioned bundle; these source observations identify a
candidate checkpoint, not an adopted final-result gate. The release matrix
checks exact SDK and Worker bytes and runs the SDK parser for each version.
Native SDK/Worker acceptance must capture the actual model input before
support is declared. Isolated Worker initialization without credentials was
rejected by the model catalog or authentication gate on all three versions.

The inspected OpenClaw packages require Node `>=24.16.0 <25 || >=26.1.0`;
these Gateway probes used Node 24.16.0. The 2026.9.6 middleware runner digest is
`23deae7d2d70f52451eeb8bb64ab8bd9542d464cabecdd80db6b771dca0278db`.
The inspected Qoder SDK `dist/index.js` digest is
`e76eb037555c5b8e74c0cbe7df9071f343be8d081d4c27738cb73491bdf827c1`;
the Linux arm64 Worker digest is
`dcab64392be3381ec82c3def0ca60667f1d8c5d834733f0fc6bb028e349f83c6`.
These hashes identify inspected files, not verified final-gate profiles.

## Admission and scope

Qoder SDK omits `security.violation` from the adapter catalog. OpenClaw admits
it only for the verified 2026.9.5, 2026.9.6 and 2026.9.7 embedded Gateway
profiles described above;
other versions and configurations fail admission. Ordinary `tool.after` and
empty-policy launches retain their existing behavior. An unsupported
Node loader, module digest, unique anchor, SDK/Worker pair or delivery path
must fail closed for a required gate. Multimodal, nested, background, direct
tool-runtime, and third-party observer paths require separate evidence.

Run the OpenClaw baseline with the published package, Node satisfying its
declared engine range, and a locally built `aw`:

```bash
python3 src/aw/crates/aw-cli/tests/openclaw_gateway.py \
  --package /absolute/path/to/node_modules/openclaw \
  --node /absolute/path/to/node \
  --aw /absolute/path/to/aw \
  --output /absolute/path/to/empty-output \
  --model-turn --final-gate-order-probe
```

The output includes `final-gate-baseline.json` with the observed middleware,
Post and model-visible stages. `baseline_red` means the marker reached the
model fixture. Add `--final-gate-enforce` to run the verified final gate, or
also add `--final-gate-provider-error` to verify failure withholding. The
native acceptance matrix pins the package and schedules all three cases.
Pass `--sec-core-provider /absolute/path/to/aw-provider-sec-core` with
`--final-gate-enforce` for the real Provider case.
Qoder Worker source inspection alone does not count as model adoption evidence.
