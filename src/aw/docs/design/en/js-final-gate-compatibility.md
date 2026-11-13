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

The shared Node runtime should own only three operations: feature-probe
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

## Release evidence at the baseline

| Agent | Published build | Observed sequence | Final-gate status |
| --- | --- | --- | --- |
| DeepSeek Harness | 0.2.1-rc.5 | `finalize/finish` -> exact-build preload -> top-level Session commit | Verified for the existing headless tests only |
| OpenClaw | 2026.9.6 | sequential result middleware -> `after_tool_call` -> model request; a later middleware can change an earlier middleware's result | Unsupported; native final sink needs validation |
| Qoder SDK | SDK 1.0.56, Worker 1.1.78, Linux arm64 | `PostToolUse` -> `updatedToolOutput`/Read rendering/additional context -> Worker result delivery | Unsupported; the public Post callback is earlier than final rendering |

The OpenClaw 2026.9.6 middleware runner awaits each handler and passes the
previous result to the next. Its built-in `tool_result` extension applies that
runner, then the observed `after_tool_call` callback runs. The local Gateway
fixture records an early middleware observing a safe result, a later middleware
injecting `AW_FINAL_GATE_SECRET_MARKER`, and the subsequent model HTTP request
containing that marker. This is a red baseline for an early-middleware check.
The `tool_result_persist` hook changed the persisted transcript in a probe,
but that change was absent from the next model request; it is not a substitute
for model-request capture.

The Qoder Worker calls `firePostToolUseEvent` before applying
`getUpdatedToolOutput()`/`getUpdatedMCPToolOutput()`. The same return path can
rewrite `llmContent` for Read and append Hook context. The shipped Worker is an
obfuscated, separately versioned bundle; these source observations identify a
candidate checkpoint, not an adopted final-result gate. Native SDK/Worker
acceptance must capture the actual model input before support is declared.

The inspected OpenClaw package requires Node `>=24.16.0 <25 || >=26.1.0`;
this Gateway probe used Node 24.16.0. Its middleware runner digest is
`23deae7d2d70f52451eeb8bb64ab8bd9542d464cabecdd80db6b771dca0278db`.
The inspected Qoder SDK `dist/index.js` digest is
`e76eb037555c5b8e74c0cbe7df9071f343be8d081d4c27738cb73491bdf827c1`;
the Linux arm64 Worker digest is
`dcab64392be3381ec82c3def0ca60667f1d8c5d834733f0fc6bb028e349f83c6`.
These hashes identify inspected files, not verified final-gate profiles.

## Admission and scope

OpenClaw and Qoder SDK currently omit `security.violation` from the adapter
catalog. Required final policy therefore fails admission; ordinary `tool.after`
and empty-policy launches retain their existing behavior. Keep that state until
an exact build, an awaited final checkpoint, identity correlation, Provider
failure withholding and native adopted-content test all pass. An unsupported
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
model fixture; it does not count as a passing final gate. The native acceptance
matrix pins the package and schedules this case. Qoder Worker source inspection
alone does not count as model adoption evidence.
