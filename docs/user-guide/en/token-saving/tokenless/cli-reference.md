# Tokenless CLI Reference

[中文版](../../../zh/token-saving/tokenless/cli-reference.md)

The `tokenless` CLI exposes a unified content-aware compression command, direct schema/JSON/TOON
operations, Stash retrieval, environment status, and statistics. Shared Agent hooks use
`tokenless compress`; the direct commands remain useful for scripts and diagnosis.

## Command overview

| Command | Purpose |
|---------|---------|
| `tokenless compress` | Run one Protocol v2 lifecycle operation |
| `tokenless compress-schema` | Compress Function Calling tool schemas |
| `tokenless compress-response` | Compress JSON/API/tool responses |
| `tokenless compress-toon` | Encode JSON as TOON |
| `tokenless decompress-toon` | Decode TOON to JSON |
| `tokenless retrieve` | Recover a payload truncated into Stash |
| `tokenless env-check` | Report the hard-disabled state of the legacy environment check |
| `tokenless stats` | Query and control local statistics |

Use the installed version's help as the final argument reference:

```bash
tokenless --help
tokenless <command> --help
```

## Common input rules

Compression and encoding commands accept input in two ways:

```bash
tokenless compress-response --file response.json

cat response.json | tokenless compress-response
```

- `-f` is the short form of `--file`.
- Without `--file`, input must be provided on stdin.
- The per-call input limit is 64 MiB.
- JSON commands require valid JSON.
- If compression does not reduce the estimated token count, the CLI explains this on stderr and returns the original.

### Minimum useful payload

`compress-schema` and `compress-response` have no fixed minimum input size. For
every accepted valid JSON input, the CLI builds a candidate and estimates both
versions as one token per CJK character plus one token per four other characters,
rounded up. In active mode, it emits the candidate only when its estimate is strictly
lower than the original (`after < before`). Otherwise stdout receives the original
input, stderr reports `did not reduce size`, and no statistics record is written.
In dry-run mode (`TOKENLESS_COMPRESSION_ENABLED=0` or
`compression_enabled=false`), stdout always receives the original input; a smaller
candidate is recorded as a predicted saving when statistics or SLS recording is enabled.

The break-even point therefore depends on content and JSON shape, not only on
bytes or characters. A small payload with a removable field can compress, while
a larger already-compact payload can pass through unchanged. The description,
string, array, and depth thresholds below only decide when individual
transformations run; they are not minimum total payload sizes. Agent adapters
can apply separate pre-spawn size gates; see
[Adapter processing rules](framework-integration.md#adapter-processing-rules).

## `compress`

`compress` is the Protocol v2 transport used by shared Agent hooks. Every envelope has exactly
four top-level fields: `protocol_version`, `operation`, `attribution`, and `input` for a request
or `result` for a response; unknown fields are rejected.

PostTool example:

```bash
jq -n \
  --rawfile content build.log \
  '{
    protocol_version: 2,
    operation: "post_tool",
    attribution: {
      agent_id: "my-agent",
      session_id: "session-42",
      tool_use_id: "tool-7"
    },
    input: {
      result_kind: "tool",
      tool_name: "Bash",
      content: $content,
      status: "success",
      content_origin: "command_output",
      output_optimization: "none",
      capabilities: {
        replace_output: true,
        recovery: {kind: "none"},
        replace_with_text: true
      }
    }
  }' \
  | tokenless compress \
  | jq '{operation, result: (.result | {disposition, content_type, applied_operations, recoverability, output})}'
```

Operations:

| `operation` | Required `input` facts | Result |
|-------------|------------------------|--------|
| `before_model` | `tools`, model-visible context without tools, tool replacement capability, and whether the integration has marker-authorized recovery | Transformed tools and sorted visible Marker hashes |
| `pre_tool` | Tool name, arguments, explicit command field, and argument replacement/block capabilities | Arguments, `passthrough`/`replace_arguments`/`block_and_suggest` action, and `none`/`rtk` output optimization state |
| `post_tool` | Result kind, tool name, content, status, explicit content origin, carried output optimization, and output capabilities | Output, disposition, detected type, applied operations, recoverability, token counts, and Stash keys |
| `retrieve` | Hash or Marker plus the Marker set currently visible to the model | Normalized hash and byte-identical payload after authorization |

`post_tool` is the only operation that enters `PostToolPipeline`. Current production routing is:

- Retrieve results, interrupted or denied calls, and RTK-optimized output bypass compression.
- Tool errors pass through unchanged and may include bounded `additional_context` diagnostics.
- Successful JSON is cleaned up and may be encoded as TOON; large arrays may be truncated or
  record-reduced recoverably.
- Recognized successful build/test command output may get terminal cleanup and recoverable
  routine-progress reduction.
- Successful CSV/TSV, when the host supports text replacement, may get cell-preserving compaction
  or recoverable row reduction; file-origin results pass through. See
  [CSV/TSV views](user-manual.md#csvtsv-views-can-be-incomplete).
- Git diffs and HTML pages are handled only when their switches are on; see
  [Configuration and data privacy](configuration-and-privacy.md).
- API search-result listings get lossless search path sharing, on by default; see
  [Controlling search path sharing](user-manual.md#controlling-search-path-sharing).
- Other content types pass through.

Only `disposition: "applied"` means `output` differs from the original. `dry_run`, `passthrough`,
`no_savings`, `recoverability_unavailable`, `timeout`, and `tool_error` carry the original content.
`applied_operations` reports the transformations that ran.

`before_model` and `post_tool` require `capabilities.recovery`: `{"kind":"none"}`,
`{"kind":"shell"}`, or `{"kind":"tool","name":"tokenless_retrieve"}`. Tool names contain
1–64 ASCII letters, digits, underscores, or hyphens. Only a static Tool enables recoverable
BeforeModel schema truncation; shell recovery applies to PostTool and does not add or change Agent
tools.

Exit codes are part of the transport contract:

- `0`: a normal operation result, including passthrough, no savings, dry-run, unavailable
  recoverability, RTK not applicable, and tool errors.
- `1`: an operation failed, including RTK timeout, unauthorized or missing Retrieve, Stash failure,
  or Pipeline failure. The error is written to stderr and no response JSON is emitted.
- `2`: malformed JSON, unsupported protocol version, or an invalid envelope/payload shape.

The `pre_tool` operation resolves the separately packaged `rtk` executable from `PATH` and supported
install layouts, skipping candidates older than 0.35.0 and continuing to the next layout.
Recognized Cargo, pytest, npm/Jest, Go, and Make build/test commands stay native so PostTool owns
their output; other supported commands may be rewritten by RTK.
Agent-facing `retrieve` authorizes the requested hash against `visible_markers` before reading Stash;
the standalone `tokenless retrieve` command below is a trusted local operations path and does not
require model-visibility context.

## Bundled RTK commands

Tokenless bundles RTK 0.49.0. The behavior below applies to that binary; a compatible
`rtk` found earlier on `PATH` can be a different version. Check `rtk --version` when
using these commands directly.

### Search flags

`rtk grep` forwards native search flags to `grep`: `-l` lists matching file names,
and `-m N` limits matches per file. They no longer mean RTK's line-length or display
limits. Use the long options `--max-len` and `--max` before the pattern to control
those RTK display limits. The former RTK `-t` / `--file-type` option is removed;
use `rtk rg -t rust` for ripgrep's native file-type filtering. Passing `-t` to
`rtk grep` now exposes the underlying grep's unsupported-option error.

```bash
rtk grep -l 'TODO' src/*.rs
rtk grep -m 3 'TODO' src/*.rs
rtk grep --max 20 --max-len 120 'TODO' src/*.rs
rtk rg -t rust 'TODO' src/
```

### Shell rewriting

RTK handles supported commands across multiple lines and applies conservative
rules to pipelines. It can rewrite a supported final stage, or a supported producer
when every downstream stage is an allowed display command such as `head`, `tail`,
or `cat`. Pipelines that count or parse raw output may remain unchanged; rewriting
is not guaranteed for every shell expression. `sudo` commands pass through unchanged,
so automatic rewriting does not insert RTK into a privileged command.

Use `rtk rewrite 'COMMAND'` to inspect a proposed rewrite without executing the
command. Tokenless still keeps its recognized build/test commands native for
PostTool compression, as described above.

### Output recovery

With the default RTK configuration, supported filters store failure output of at
least 500 bytes and explicitly truncated output in a local SQLite recovery store.
When a recovery hint appears, run `rtk recall HASH` with the displayed hash;
`rtk recall --full HASH` requests all stored output, and `--from`, `--lines`, and
`--grep` narrow the result. `rtk recall --list` lists retained entries. Storage limits
and expiry apply, so recall is not a permanent archive. Existing legacy tee
configuration continues to use its file-based recovery mode.

RTK recovery state is scoped to the host OS user, not to a Tokenless tenant or
session. On Linux the default store is `$XDG_DATA_HOME/rtk/recall.db`, or
`~/.local/share/rtk/recall.db` when `XDG_DATA_HOME` is unset. `TOKENLESS_DATA_DIR`
does not relocate it. Sessions sharing the same OS user and RTK store can list
and recall each other's retained output. Configure `RTK_RECALL_DB` in the command
execution environment to select a separate store, or `RTK_RECALL=0` to disable
recovery. Separate paths do not provide an OS permission boundary for the same user.

RTK recall hashes belong to RTK's store. Use `tokenless retrieve HASH` for Tokenless
Stash markers; the two recovery commands are not interchangeable.

## `compress-schema`

Compress one OpenAI Function Calling schema:

```bash
tokenless compress-schema -f tool.json
```

Compress a JSON array:

```bash
cat tools.json | tokenless compress-schema --batch
```

Accepted item shapes (detected per item):

- OpenAI function wrapper: `{"function": {"name", "description", "parameters"}}`
- Direct schema: `{"name", "description", "parameters"}`
- Gemini / copilot-shell wrapper: `{"functionDeclarations": [{"name", "description", "parameters" | "parametersJsonSchema"}, ...]}`; declarations inside the wrapper are compressed individually, and the wrapper and its sibling fields are preserved.

An array input enables batch handling automatically.

A complete request object with a top-level `tools` array is also accepted. Its
Function Calling entries may be OpenAI `{"function": {...}}` wrappers, Gemini
`{"functionDeclarations": [...]}` tool objects, or bare
`{name, description, parameters}` declarations. Do not pass `--batch` for this
shape; non-function tools and fields outside `tools` are preserved.

```bash
tokenless compress-schema -f request.json
```

Common options:

| Option | Description |
|--------|-------------|
| `-f, --file <path>` | Input file; omit to read stdin |
| `--batch` | Treat the input as a schema array |
| `--agent-id <id>` | Agent identifier in statistics |
| `--session-id <id>` | Session identifier in statistics |
| `--tool-use-id <id>` | Tool-call identifier in statistics |
| `--no-stash` | Do not save truncated descriptions; truncation becomes irreversible |
| `--stash-db <path>` | Override the Stash database; an invalid path is rejected as an override and falls back to the environment or default path |

Default processing rules:

| Item | Default |
|------|---------|
| Maximum function-description length | 256 characters |
| Maximum parameter-description length | 160 characters |
| Drop `examples` | yes |
| Drop `title` | yes |
| Remove fenced and inline code, then collapse whitespace in descriptions | yes |
| Maximum recursion depth | 32 |

Example:

```bash
tokenless compress-schema -f tools.json --batch \
  --agent-id copilot-shell --session-id session-001
```

## `compress-response`

Compress a JSON response:

```bash
tokenless compress-response -f response.json
```

By default it removes exact, case-sensitive blacklisted keys, `null`, and empty strings/arrays/objects, including empty items inside arrays. It then truncates long strings, long arrays, and values beyond the configured nesting limit. Common options:

| Option | Default | Description |
|--------|---------|-------------|
| `-f, --file <path>` | stdin | Input file |
| `--truncate-strings-at <n>` | `4096` | String truncation threshold |
| `--truncate-arrays-at <n>` | `32` | Array length that triggers truncation; the first `n` items are kept. Object record arrays use record reduction instead (see below) |
| `--array-tail-preserve <n>` | `8` | Items preserved from the tail of truncated arrays; `0` disables tail preservation. Does not apply to object record arrays |
| `--max-depth <n>` | `8` | Maximum nesting depth |
| `--agent-id <id>` | `cli` | Agent identifier in statistics |
| `--session-id <id>` | — | Session identifier in statistics |
| `--tool-use-id <id>` | — | Tool-call identifier in statistics |
| `--no-stash` | off | Disable reversible Stash |
| `--stash-db <path>` | `~/.tokenless/stash.db` | Override the Stash database; an invalid path is rejected as an override and the CLI falls back to the environment or default path |

Array truncation keeps a head window of `--truncate-arrays-at` items and a tail window of `--array-tail-preserve` items, with a truncation marker in between. Middle items are dropped only when the array is longer than both windows combined, so under the defaults a command can retain `n + 8` items plus the marker; when the two windows cover the whole array, every item is retained without a marker. Set `--array-tail-preserve 0` for head-only truncation.

Object record arrays are an exception. Any array of at least 33 JSON objects is reduced against a base budget of 32 records plus a retrieval marker, regardless of the `--truncate-arrays-at` and `--array-tail-preserve` values. The selection keeps the first 4 and last 4 records, records carrying error or anomaly signals, numeric outliers, and a stable sample of the remaining records; critical records can exceed the base budget. The complete original array is written to the Stash as one entry so `tokenless retrieve` can restore it. Record reduction requires the Stash, so with `--no-stash` every record of such an array is kept instead of reduced.

Override thresholds:

```bash
tokenless compress-response -f response.json \
  --truncate-strings-at 2048 \
  --truncate-arrays-at 16 \
  --max-depth 6
```

The default field-name blacklist is:

```text
debug, trace, traces, stack, stacktrace, logs, logging
```

Field matching and truncation change the response representation seen by the model. Save representative samples and compare the result before processing critical payloads.

Stash applies to complete original arrays used by record reduction, truncated strings, the dropped middle segment of other truncated arrays, and deep subtrees. Tail items are kept inline, not stashed. Blacklisted fields, `null`, and empty values are removed without a retrieval marker.

Most adapters override these standalone defaults. Their shared shell profile uses `65536`, `128`, and `8`; the other-structured-tool profile uses `1048576`, `65536`, and `32`. Content-retrieval tools are skipped. See [Agent integration · Adapter processing rules](framework-integration.md#adapter-processing-rules) and [User manual · Compression trigger conditions and thresholds](user-manual.md#compression-trigger-conditions-and-thresholds).

## `compress-toon` and `decompress-toon`

JSON to TOON:

```bash
echo '{"name":"Alice","age":30}' | tokenless compress-toon --min-toon-chars 0
```

Payloads shorter than 500 characters pass through unchanged by default, the same minimum as the adapter hooks, because TOON savings on small JSON are near zero. Pass `--min-toon-chars 0` to encode any payload that yields token savings. Invalid JSON exits with code 2 regardless of length.

TOON to JSON:

```bash
printf 'name: Alice\nage: 30\n' | tokenless decompress-toon
```

Round-trip verification:

```bash
echo '{"name":"test","value":42}' \
  | tokenless compress-toon --min-toon-chars 0 \
  | tokenless decompress-toon
```

`compress-toon` supports `--agent-id`, `--session-id`, `--tool-use-id`, and `--min-toon-chars`. When a payload is below the minimum length or encoding provides no savings, it returns the original JSON byte-for-byte with exit code `0` and does not record that operation; the note on stderr is informational. When scripting, detect whether encoding happened by comparing stdout with the input.

## `retrieve`

This optional instruction in compressed output means that removed content was written to Stash:

```text
12 passing-test lines omitted. If needed, run in shell: tokenless retrieve 0123456789abcdef01234567
```

Retrieve by bare hash:

```bash
tokenless retrieve 0123456789abcdef01234567
```

AgentScope uses its configured static Tool instead of a shell command:

```text
12 passing-test lines omitted. If needed, call tool tokenless_retrieve with hash_or_marker=0123456789abcdef01234567
```

Historical markers remain accepted but are no longer generated:

```bash
tokenless retrieve '<<tokenless:0123456789abcdef01234567>>'
```

Override the database:

```bash
tokenless retrieve 0123456789abcdef01234567 \
  --stash-db ~/.tokenless/stash.db
```

The hash must contain 24 hexadecimal characters and is case-insensitive. The default SQLite Stash TTL is one hour and its live-entry capacity is 10,000. Retrieval fails after expiry or capacity eviction, with `--no-stash`, in dry-run mode, after a failed write, or when a different database path is used.

## `env-check`

Tool Ready is hard-disabled. Text output reports that state without reading the
specification or changing the environment. No environment variable can
re-enable it.

Every JSON invocation returns exactly three fields:

```json
{"tool":"Shell","status":"UNKNOWN","enabled":false}
```

`tool` is the requested tool name, `all`, or `checklist`.

Report the disabled state for one tool:

```bash
tokenless env-check --tool Shell
```

Report the disabled state for all-tools or checklist mode:

```bash
tokenless env-check --all
tokenless env-check --all --json
tokenless env-check --checklist
tokenless env-check --checklist --json
```

Automatic repair:

```bash
tokenless env-check --tool Shell --fix
```

> While the hard bypass is active, `--fix` does not invoke a package manager or modify the environment.

## `stats`

```bash
tokenless stats summary
tokenless stats summary --json
tokenless stats summary --limit 1000
tokenless stats list --limit 20
tokenless stats show <record-id>
tokenless stats diff <record-id>
tokenless stats diff --session <session-id>
tokenless stats status
tokenless stats enable
tokenless stats disable
tokenless stats clear --yes
```

Dual-run comparison:

```bash
tokenless stats summary --compare <baseline-session> <active-session>
```

A missing session ID fails with a non-zero exit instead of a 0% comparison, matching `stats diff --session`. `stats summary --limit` must be a positive integer; `--limit 0` is rejected at parse time, matching `stats diff --limit`.

The percentage fields in `stats summary --json` (`chars_saved_percent`, `tokens_saved_percent`) and in `--compare --json` (`saved_percent`) are all computed as a saved amount divided by the original, uncompressed amount; the summary and compare fields clamp the saved amount to zero while `stats diff --json` keeps negative values. See [Measuring savings → Saving-rate field definitions](measuring-savings.md#saving-rate-field-definitions).

Inspect one record or the verified stages of one tool call:

```bash
tokenless stats diff <record-id> -U 5
tokenless stats diff --session <session-id> \
  --tool-use-id <tool-use-id>
```

`stats show` prints the complete stored before/after text. `stats diff` explains estimated savings and renders changed lines. Its main options are:

| Option | Applies to | Behavior |
|--------|------------|----------|
| `<record-id>` | One record | Conflicts with `--session` |
| `--session <id>` | Session | Shows a metrics-only overview |
| `--tool-use-id <id>` | Session | Expands one tool call; requires `--session` |
| `-l, --limit <n>` | Session overview | Maximum chains, default `20` |
| `--sort saved\|time` | Session overview | Largest saving first by default, or newest first |
| `-U, --context <n>` | Content diff | Unchanged lines around changes, default `3` |
| `--no-color` | Text output | Disables ANSI colors |
| `--json` | Any scope | Emits schema `1.0` JSON with structured diff hunks |

Content diffing is omitted when either endpoint is unavailable or exceeds 1 MiB, and rendered hunks stop after 500 lines. Take care when using a shared terminal or collecting output because record and tool-use diffs can contain stored source text. See [Measuring savings](measuring-savings.md) and [Configuration and data privacy](configuration-and-privacy.md).

`stats status` reports the local-statistics and SLS switches and their source. The current status path does not read the compression switch, so it does not display `compression_enabled`; inspect `TOKENLESS_COMPRESSION_ENABLED` and `~/.tokenless/config.json` for that setting.

## Errors and degradation

- CLI errors are written to stderr and return a non-zero exit status.
- Hooks and plugins normally catch errors and pass through the original response.
- No compression savings is not an error; the CLI returns the original.
- Compression may continue after a Stash write failure, but the related truncated content cannot be retrieved.

See [Troubleshooting](troubleshooting.md) for input, database, and adapter errors.
