# Tokenless Runtime Library

[中文版](runtime-library_zh.md)

## Purpose

`anolisa-tokenless` is the framework-neutral, in-process Tokenless SDK. A platform wheel contains
the PyO3 runtime and the pinned RTK executable, so Python applications do not require `tokenless`
or `rtk` on `PATH`.

The public `TokenlessSdk` maps four host lifecycle boundaries to Tokenless behavior:

| Lifecycle | Behavior |
|---|---|
| `before_model` | Reversible Function Calling schema compression and conditional retrieve-tool publication |
| `pre_tool` | RTK rewrite for an adapter-declared command field |
| `post_tool` | Status routing, response compression, TOON selection, and environment-error guidance |
| `retrieve` | Marker-authorized, byte-exact Stash retrieval |

Tool Ready is product-wide hard-disabled and is not part of this API.

## Contracts and state

Adapters translate framework objects into four immutable request/response pairs:
`BeforeModelRequest`, `PreToolRequest`, `PostToolRequest`, and `RetrieveRequest`. `Attribution`
requires agent and session identifiers; PreTool and ordinary PostTool requests also require a
tool-use identifier. OpenAI Function Calling JSON is the normalized schema representation, but the
lifecycle operations are Tokenless contracts rather than OpenAI requests.

`tokenless-runtime` owns one SQLite Stash and statistics recorder. Schema and response compression
share that Stash and roll back keys whenever a candidate is discarded. TOON is linked as a Rust
library and never starts a process. RTK is used only when an adapter supplies `command_field`; every
rewritten wrapper is anchored to the packaged executable and carries per-execution attribution.
Content detection, thresholds, TOON selection, diagnostics, authorization, and Stash policy remain
in Rust Core rather than Python configuration. A shell tool's `PostToolRequest` carries the executed
command line in `command`; Core, not the adapter, reports a plain print of local files as
`file_read` so that a printed HTML page stays verbatim while data in that output still compresses.

The SDK never stores a process-global current session. `before_model` returns the exact visible
marker set. An adapter declares whether it has a marker-authorized recovery path and owns any
Agent-facing command or tool declaration. AgentScope keeps its Retrieve tool static across model
calls, retains the marker set in framework session state, and `retrieve` authorizes only a marker
in that set. Host applications
retain raw tool values for UI and business logic and pass only copied, final model-visible text to
`post_tool`. Retrieve output never enters PostTool again.

Invalid inputs, missing packaged RTK, lifecycle operation failures, attachment failures, and
tool-name collisions fail fast. Normal Core dispositions such as passthrough, no savings, and
recoverability unavailable return typed results. A candidate is applied only when it is strictly
smaller; schema and response truncation must also remain retrievable.

## Search path sharing

PostTool routes detected `search_results` through `SearchResultsCompressor` only when search path
sharing is enabled, the origin is `api_response`, and text replacement is available. This is a
content-based Core capability; it is not restricted to a particular agent ID. File content and
command output do not enter it, including Bash output without RTK. Successful `path:line:text` listings with at least three records can share full
paths across consecutive rows. Line numbers, row text, order and line endings remain byte-reversible.
The input contract excludes colons in paths and context listings. Unsupported rows reject the whole
candidate; byte length and estimated tokens must both decrease before normal Runtime arbitration.
The applied operation is `search_path_sharing` (`AppliedOperation.SEARCH_PATH_SHARING` in the
Python SDK), with `lossless` recoverability and no Stash writes.

Search path sharing is enabled by default. CLI callers can disable it with
`TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`. When unset or empty it stays enabled; `1`, `true`,
and `yes` also enable it (case-insensitively), and any other value disables it. This variable
is independent of the JSON config file.
Rust callers use `RuntimeConfig.search_path_sharing_enabled`; Python callers use
`TokenlessConfig(search_path_sharing_enabled=False)` or the matching `TokenlessRuntime` keyword.
Disabling this domain returns search listings unchanged without computing a search candidate.
JSON, table, and log compression remain available for other tool names. The exact name `Grep`
always excludes those domains to retain every received match; disabling path sharing does not
restore pre-feature JSON/table/log dispatch for a custom tool named `Grep`.
The global compression switch still controls dry-run behavior for enabled domains.
Whole-task savings depend on the workload; preserving all received bytes does not guarantee
fewer follow-up tool calls or lower total token use.

The first line is the fixed format header. Subsequent `File=` lines contain a JSON-encoded path;
data lines always begin with ASCII digits followed by `:`, so they cannot be mistaken for file
headers. To reconstruct, prefix each data line with the decoded current path and `:` and retain
its original line ending. Repeated nonconsecutive paths receive separate headers.

The Claude Code adapter unwraps only native Grep `mode=content` responses without context options,
declares that slot as API response text, and replaces only `content` in the original output object.
It preserves host metadata, including any limits; completeness means all received rows, not all
possible matches before host truncation. Other Grep modes and hosts keep their existing route.
Unwrapped Grep listings are recorded as `api_response` in statistics, rather than `file_content`:
they are filtered tool responses, not authoritative file copies. Historical origin-grouped queries
therefore have a version boundary. Core treats the exact tool name `Grep` as a search-only tool:
it cannot enter JSON, table, or log compressors, even when search path sharing is disabled. SDK
callers should use that name only for this search contract. RTK-owned output still bypasses Core.

## Statistics queries

`TokenlessStats` is a read-only public query client backed by the same Rust `StatsRecorder` and
`stats.db` schema as the CLI. It exposes typed status, summary, recent-record, record-detail,
structured-diff, and baseline-comparison results. `TokenlessSdk.stats` creates this client lazily
against the Runtime data directory, so a damaged statistics database does not change lifecycle
initialization or compression fail-open behavior. Read-only describes the public operations: CLI
parity means opening the client may create or migrate `stats.db`, so its data directory must be
writable.

Summary, list, and comparison results expose metrics only. Record detail and detailed record or
tool-use diffs can expose stored tool content; the existing one-MiB input and 500-line diff bounds
still apply. Token counts are estimates, and the Runtime records only operations whose candidate
removes estimated tokens. `limit=None` for summary or comparison uses the recorder's 10,000-record
cap. Session and tool-use diffs also load at most the newest 10,000 matching records. Comparisons
expect a dry-run baseline session followed by an active Tokenless session; the client does not
infer or enforce those modes. The Python API does not clear data or change global recording
settings.

## Packaging and validation

`make python-wheel` builds the pinned RTK version, stages it as
`anolisa_tokenless/_bin/rtk`, and creates a CPython 3.11 stable-ABI platform wheel. Cross-platform
builders may set `PYTHON_RTK_BINARY` to the RTK executable built for the same wheel target.
`make test-python-runtime` installs the wheel in a fresh environment and exercises all four
lifecycles plus statistics queries without relying on a system RTK binary.

`anolisa-tokenless-agentscope` supports AgentScope 1.0.11 through 1.0.x and 2.0.x. The 1.x adapter
uses a Tokenless Toolkit, a model proxy, and public instance hooks. The 2.x adapter uses
`on_model_call` and `on_acting`; 2.0.0 keeps marker state in the paired Middleware/Tool, while later
versions also persist it in `AgentState.middle_context`. Both expose the complete SDK; 2.0.0 supports
direct Agent construction, while App integration starts at 2.0.1. Built-in tools have explicit
contracts; applications must register a `ToolContract` for each custom tool so `ContentOrigin` is
never inferred from output text.
