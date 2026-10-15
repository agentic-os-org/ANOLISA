# Compress model-visible tool results with AW

[中文版](../../zh/user-entrypoint/aw-tokenless.md)

The optional Tokenless Provider replaces a completed tool's model-visible text
with compression selected under Tokenless's configured reversibility policy. AW
checks replacement bounds; it does not independently prove semantic losslessness.
Qoder, OpenClaw, QwenPaw and
Hermes share one Provider configuration for PreTool retry suggestions and PostTool
compression. Schema compression and authorized retrieval remain separate.

## Install and enable

Use the [AW Preview packages](aw-preview.md). `aw-provider-tokenless` contains
`aw-provider-tokenless` and the `tokenless` CLI built from the same source commit.
It requires matching `aw-core`; `aw-all-in-one` includes core, sec-core and
Tokenless. PostTool works without RTK. PreTool additionally requires a compatible
RTK installation resolvable by the Agent environment, following the
[Tokenless installation guide](../token-saving/tokenless/QUICKSTART.md); the AW
package does not bundle RTK. Python wheels and Tokenless native plugins are not
needed. Agent installations and their runtime dependencies remain external.

After installing core, install the matching Tokenless extension into the same
prefix, or install all-in-one into a new prefix. For Qoder/OpenClaw, initialize
with `aw-package configure --provider tokenless` and the usual Agent flags.
Choose `--provider sec-core+tokenless --socket ABS_FILE` to generate sec-core
checks before execution, append PreTool, retain its completion observer, and append
compression after the existing after steps. Installing a component
does not enable it. The existing plain `sec-core` selection retains its behavior.

For all four frameworks, copy the
[shared example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider-tokenless/examples/aw.yaml),
replace its executable paths, and keep both state directories outside the immutable
installation prefix. Use the framework-specific profile and launch flags from the
[AW guide](aw.md#use-one-configuration-for-four-frameworks).

The Provider accepts only private configuration `{version: 1}`. Its fixed argv is
`aw-provider-tokenless --cli ABS_FILE --data-dir ABS_DIR`; it calls
`tokenless compress` using Protocol v2 and sets `TOKENLESS_DATA_DIR` for that child.
CLI statistics use the Agent, session and tool-call identifiers supplied by AW.
The CLI's statistics measure selected compression, not verified native adoption.

AW's journal and Tokenless statistics have separate retention boundaries. Tokenless
CLI defaults enable both SQLite statistics and SLS logging unless configuration or
inherited environment overrides them. SQLite records for compression savings can
retain the complete before/after tool text in `$TOKENLESS_DATA_DIR/stats.db`, even
with `recovery: none`. The SLS sink writes metrics and attribution to local JSONL
(default `/var/log/anolisa/sls/ops/tokenless.jsonl`); this implementation does not
include tool bodies in SLS records or upload them. On hosts running the anolisa
telemetry uploader or SLS/Logtail collection, those metrics and attribution IDs
can be uploaded separately; this channel still contains no tool text.
Set `TOKENLESS_STATS_ENABLED=0` and `TOKENLESS_SLS_ENABLED=0` in the environment used to launch AW before binding
an Agent to disable both sinks. These settings do not delete existing records.

## PreTool and permission behavior

The generated template and shared example include `tool.before` → `pre_tool` with
`effects: [block]` and `on_error: report`. Supported shell tools are `Bash`, `exec`,
`terminal`, `shell`, `run_shell_command` and `execute_shell_command` (case insensitive),
with a nonempty string `command`. Other inputs pass through without calling the CLI.
Tokenless chooses whether to suggest an RTK command. AW blocks the original attempt
and shows the full retry arguments to the model; it never silently modifies input
or approves/executes a command. A new call must pass sec-core and native permission
checks again. A scanner denial/failure remains authoritative even when PreTool is
neutral. Existing external Hook scheduling is unchanged.

The Tokenless integration recognizes a standalone attributed RTK wrapper in executed
arguments and supplies `output_optimization: rtk` to PostTool. The retry's call ID may
differ from the blocked attempt. Quoted or escaped argument literals and symlinked
state directories preserve this ownership. Supported execution prefixes are literal
environment assignments, `env NAME=value ...`, bare `sudo`, `noglob`, `command`,
`exec`, and `shadowenv exec --`; these forms may be combined. Other wrapper forms
(including extra flags), bare RTK commands, foreign attribution and compound shell
commands are not inferred as RTK-owned. Unavailable RTK, CLI errors, invalid
responses or timeouts are reported under `on_error: report`; they do not disable
sec-core's `on_error: block` or grant native approval. To keep PostTool alone, remove
only the `rewrite-command` before step and retain the shared Provider and after chain.

Recovery remains `none` in this delivery. Recovery entry points, Stash, retrieval
scope and retrieval-output bypass belong to the Tokenless integration layer; enabling
PreTool does not provide those capabilities or Schema compression.

## Result and failure behavior

| Framework | Model-visible replacement entry |
| --- | --- |
| Qoder | `PostToolUse` → `hookSpecificOutput.updatedToolOutput` string |
| OpenClaw | `registerAgentToolResultMiddleware` before the next model request |
| QwenPaw | Final AgentScope `ToolResponse` returned by acting middleware |
| Hermes | `transform_tool_result` final string callback |

OpenClaw requires its middleware API and explicit plugin enablement; AW's generated
native profile declares the middleware contract. An observational `after_tool_call`
return value cannot replace model-visible content.

Only a supported single text slot is projected. Media, multiple content blocks,
intermediate streaming chunks and unsuccessful tool results remain unchanged.
Replacement preserves native metadata and envelope fields. Qoder requires a
string replacement, so a structured envelope is serialized into that string. Known file-read tools
are classified as `file_content`; shell commands are supplied to Tokenless so it
can also recognize plain file reads. Other tools use `api_response`.
For OpenClaw, after-event `tool_name` statistics and AW journal metadata use
lowercase policy names (for example, `Bash` becomes `exec`); before events retain
the raw name for sec-core lookup. Join phases by session/tool-call IDs, not names.

Recovery is explicitly `none`, so Tokenless may only apply candidates permitted
without an authorized recovery path. There is no additional compressed copy or
context message. `dry_run`, `no_savings` and unavailable recovery keep the original
result. CLI failures, invalid responses, identity mismatches, timeout and output
limits are AW failures handled by `on_error: report`; the native result is retained.
AW's persistent journal remains metadata-only and does not store replacement text.

Keep existing completion observers and custom Hooks before compression in the
`tool.after.steps` list. Qoder, QwenPaw and Hermes run these steps in their declared order when result
replacement is enabled; all observers see the original result. Only the final structured
step may request `replace_result`. A replacement followed by another enabled step
or a second replacement is rejected during admission. Earlier observers retain
their original Provider input shape; only the replacement Provider receives the
text projection. All steps share one event deadline, and a reported observer or
custom Hook failure does not suppress later steps while budget remains.

Qoder keeps independent native callbacks and orders only AW's after invocations
inside the service; it does not force the native `sequential` flag or change
external Hook scheduling. An explicitly configured native `sequential` setting
still follows Qoder's global matching-plan semantics. QwenPaw registers the chain
in reverse so native middleware unwinding preserves its order. Hermes retains
completion observers before its final result transform.

OpenClaw's native completion event occurs after its result middleware. Compression
therefore runs first; completion observers then run serially in their declared
order at the original native after seam, with native `error`, `durationMs` and other
completion fields. They receive OpenClaw's sanitized view of the pre-compression
result. The two phases share one AW deadline and once-only step claims. Pending
original snapshots are bounded to 32 tool calls across all sessions in the plugin
instance, and cleared at completion/session teardown. The serialized callback
frame, including both native views, must fit within 1 MiB, so the usable tool-text
size is smaller and depends on JSON escaping and metadata. Compression is skipped
when either snapshot or frame limit is reached. A missing middleware API disables
compression with a warning while leaving before enforcement and native completion
observation registered. The launch receipt reports
`degraded: "result_middleware_unavailable"`, and AW prints
`native hooks ready (degraded: result replacement unavailable)` without failing
startup. The receipt still counts configured steps; it does not claim that the
missing replacement handler was registered.
Existing Agent hooks outside the AW step list retain their
framework's scheduling. Disable overlapping native Tokenless plugins for these
sessions to avoid duplicate compression/statistics.

Provider messages retain AW's 1 MiB limit, including the original event and its
projected text. If the projection alone would cross the limit, AW omits it and
keeps the original event auditable; compression then returns no replacement. The CLI invocation shares the remaining AW budget and bounded
stdout/stderr. The Host bounds the whole Provider process; direct callers must
close stdin and bound its lifetime themselves.

To disable PostTool, remove the compression step; remove its Provider only if no
PreTool step still references it. Then stop
and restart AW using the updated configuration. Existing sessions are not reloaded.
Keep or remove the external Tokenless state directory according to your data needs.

## Build from source

From the repository root on Linux:

```bash
cargo build --locked --manifest-path src/aw/Cargo.toml -p aw-provider-tokenless
cargo build --locked --manifest-path src/tokenless/Cargo.toml -p tokenless-cli
```

Use the resulting `src/aw/target/debug/aw-provider-tokenless` and
`src/tokenless/target/debug/tokenless` paths in the shared example. The clean-source
Preview builder accepts `--component tokenless` or `--component all`.
