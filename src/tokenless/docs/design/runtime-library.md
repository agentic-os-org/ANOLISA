# Tokenless Runtime Library

[中文版](runtime-library_zh.md)

## Purpose

`anolisa-tokenless` is the framework-neutral Tokenless SDK. BeforeModel, PostTool and Retrieve run
in process through the PyO3 runtime; PreTool runs in a child process, as described below. A platform
wheel contains the PyO3 runtime and the matching `tokenless` executable, so Python applications do
not require `tokenless` or `rtk` on `PATH`.

The public `TokenlessSdk` maps four host lifecycle boundaries to Tokenless behavior:

| Lifecycle | Behavior |
|---|---|
| `before_model` | Reversible Function Calling schema compression and conditional retrieve-tool publication |
| `pre_tool` | RTK rewrite for an adapter-declared command field |
| `post_tool` | Status routing, response compression, TOON selection, and environment-error guidance |
| `retrieve` | Marker-authorized, byte-exact Stash retrieval |

Tool Ready is product-wide hard-disabled and is not part of this API.

## Contracts and state

Python hook integrations keep host protocol rules in
`adapters/tokenless/<host>/hook_adapter.py`: payload parsing, capability checks,
and replacement envelopes belong to that host. For example, Cosh-NG serializes
shell replacements as text, while Claude Code and QwenWork preserve structured
slots; Qoder preserves its native Bash stdout object. `common/hooks` contains
Protocol v2 transport, normalized result transformations, and lifecycle state.
Its historical command files remain bootstrap entrypoints; host selection lives
in `adapters/tokenless/hook_dispatch.py`. Cosh extensions bundle their adapters,
and a detached QwenWork plugin loads its own adapter beside the shared runtime.
OpenClaw, DSH, Hermes, and the Python SDK integrations retain their existing
host-specific entrypoints. Compression selection and acceptance remain in Core.

Adapters translate framework objects into four immutable request/response pairs:
`BeforeModelRequest`, `PreToolRequest`, `PostToolRequest`, and `RetrieveRequest`. `Attribution`
requires agent and session identifiers; PreTool and ordinary PostTool requests also require a
tool-use identifier. OpenAI Function Calling JSON is the normalized schema representation, but the
lifecycle operations are Tokenless contracts rather than OpenAI requests.

`tokenless-runtime` owns one SQLite Stash and statistics recorder. Schema and response compression
share that Stash and roll back keys whenever a candidate is discarded. TOON is linked as a Rust
library and never starts a process. RTK is linked into the `tokenless` executable, and
`TokenlessSdk.pre_tool` starts `tokenless compress` for each call when an adapter supplies
`command_field`, because RTK assumes one process per command: it caches trusted filters and project
files for the process lifetime and can wait on `git rev-parse` without a deadline. Each call therefore
sees current RTK state. The SDK gives the child 5 seconds, kills its process group (`taskkill /T` on
Windows) on timeout, and on any failure logs a warning and passes the command through. Every
rewritten wrapper calls the packaged `tokenless` executable as `tokenless rtk` and carries
per-execution attribution.
Content detection, thresholds, TOON selection, diagnostics, authorization, and Stash policy remain
in Rust Core rather than Python configuration. A shell tool's `PostToolRequest` carries the executed
command line in `command`; Core, not the adapter, reports a plain print of local files as
`file_read` so that a printed HTML page stays verbatim while data in that output still compresses.

PreTool preserves recognized native `head` and `tail` calls using the same direct-call and
built-in wrapper recognition as `find`, including nesting and shell line continuations. Every
argument form keeps native execution, including byte windows and follow mode. A recognized call
keeps its entire pipeline or compound command outside RTK rewriting. This avoids structural
truncation in RTK's `read --max-lines` path; explicit RTK calls retain their existing behavior.
Custom `hooks.transparent_prefixes` are not inspected, so calls behind those prefixes may still
be rewritten. The rule is independent of search path sharing and retains the existing PostTool
file-read policy.

The same native-command recognition covers every `grep` and `rg` mode. `git grep`,
`xargs grep`, and custom wrapper prefixes are outside this recognition. PostTool reuses
that parser for `command_output` searches and routes them, like the native `Grep` tool, only to
`SearchResultsCompressor`. Its unified entry recognizes numbered path matches and path lists;
format recognition and savings checks stay inside the compressor. Disabled search compression
or no candidate returns the original, including with `force_json`, without trying another domain.
Qoder's native Bash adapter sends only `stdout` to Core, even when it is short or empty or
`stderr` is larger, and preserves every other field when rewrapping the result.

This protection covers the complete output of any compound command containing a recognized
search. For example, `grep -q needle Cargo.toml && cargo build` also keeps its build log
unchanged: the hook supplies no per-command output boundaries. This deliberately reduces
existing build/test and other domain compression coverage until mixed-output regions can be
handled without omitting search matches. Pipelines such as `rg -l TODO src | xargs cat` also
retain their output, even though the final content is file text rather than a path list.
Tokenless adds no truncation fallback; host output limits still apply. `max_input_bytes`
limits compression work and passes oversized input through rather than capping returned text.

An enabled search compressor that produces no candidate uses the common `no_savings`
arbitration and zero Stash counters when a persistent store is attached. Disabled search compression
and lifecycle gates still pass through. Command metadata on API and file-read results does
not activate shell-search protection; the exact native tool name `Grep` retains its contract.

The SDK never stores a process-global current session. `before_model` returns the exact visible
marker set. An adapter declares whether it has a marker-authorized recovery path and owns any
Agent-facing command or tool declaration. AgentScope keeps its Retrieve tool static across model
calls, retains the marker set in framework session state, and `retrieve` authorizes only a marker
in that set. Host applications
retain raw tool values for UI and business logic and pass only copied, final model-visible text to
`post_tool`. Retrieve output never enters PostTool again.

QwenWork's hook adapter normalizes recognized workspace Shell MCP names to `Bash`
before either lifecycle request. It unwraps only an exact single plain text block;
unknown block arrays pass through without compression. The workspace event's
absolute `cwd` anchors `.tokenless` state for both hooks and the RTK command.
Native Bash keeps the existing state location and structured response handling.
Plugin-private binaries are resolved by the adapter. On macOS, a verified PreTool
bridge anchors standalone recovery commands to that binary and the same state,
Stash selection and attribution. Without that bridge, private runtime paths declare
`recovery.kind = none`. The bridge and exact recovery recognition belong to the
QwenWork adapter, independently of other hosts.

Invalid inputs, a missing packaged `tokenless` executable, lifecycle operation failures, attachment failures, and
tool-name collisions fail fast. Normal Core dispositions such as passthrough, no savings, and
recoverability unavailable return typed results. A candidate is applied only when it is strictly
smaller; schema and response truncation must also remain retrievable.

OpenClaw 2026.6.6 or later uses declared, explicitly enabled tool-result middleware to call
PostTool before model consumption. The adapter preserves the result envelope and handles only
single-text results and retains per-call RTK attribution. A static `tokenless_retrieve` tool
declares tool recovery only after its factory receives a conversation UUID. The adapter rebuilds
visible markers at prompt submission through `llm_input`, skips Core when the history has no
recovery instructions, and adds newly emitted Stash keys. `after_compaction` revokes previous
grants until the next prompt supplies retained history; this event has no message contents.
Session end clears state. Tool execution supplies that conversation's marker set to Core Retrieve; recovered
payloads bypass PostTool. Missing tool APIs or UUIDs retain lossless-only compression. RTK output
still declares `recovery.kind = none` before receipt lookup.
No separate persistence hook reprocesses the result. Adapter failures retain the received
host result; the adapter catches them because the host treats thrown middleware errors as
tool failures. Legacy middleware preprocessing can already have clipped text or metadata.
Tokenless neither rejects such hosts above the API minimum nor reconstructs missing content.
Only `applied` compression replaces text; tool-error diagnostics may still append context.
See the [host compatibility boundary](../../../../docs/user-guide/en/tokenless/framework-integration.md#openclaw).

After building the CLI and OpenClaw adapter, run the optional container regression from
`src/tokenless` with a local host image:

```bash
python3 tests/test-openclaw-gateway.py --image <legacy-image> --host-output-policy legacy
python3 tests/test-openclaw-gateway.py --image <fixed-image> --host-output-policy normalized
```

Select the expected policy from the image's source provenance, not its version string.
The real Gateway uses a scripted local model with no cloud requests. The test covers ordinary
CSV and 150,000-character plain/JSON results, failure passthrough, first consumption, replay,
persistence and statistics. Legacy truncation is an expected limitation; the normalized policy
requires full native input at middleware entry and lossless JSON candidate delivery. This
checks transport behavior, not model answer quality or billing savings.

The Python lifecycle configuration reads the global, search, HTML, and Diff environment switches
at construction time. Explicit `TokenlessConfig` arguments win over environment defaults, and
the resolved flags are passed to the in-process Runtime. This also covers QwenPaw and AgentScope;
the direct Runtime constructor remains explicit. Diff is enabled by default, and disabling global
compression selects dry-run without disabling PreTool RTK.

## Search path sharing

PostTool routes native `Grep` and recognized shell `grep`/`rg` output directly through
`SearchResultsCompressor` when search path sharing and text replacement are available.
Other tools retain content-based routing: numbered records require `api_response` origin,
and plain path lists also accept command output. Successful `path:line:text` listings with at least three records can share full
paths across consecutive rows. Line numbers, row text, order and line endings remain byte-reversible.
The input contract excludes colons in paths and context listings. Unsupported rows reject the whole
candidate; byte length and estimated tokens must both decrease before normal Runtime arbitration.
The applied operation is `search_path_sharing` (`AppliedOperation.SEARCH_PATH_SHARING` in the
Python SDK), with `lossless` recoverability and no Stash writes.

Plain path lists can also share a common directory prefix for `command_output` and `api_response`
origins. At least three POSIX paths are required, with uniform LF or CRLF endings; all received
paths, their order, duplicates, and the final newline are retained. The entire input is validated,
including beyond the detector's bounded sample. Blank lines, control characters, colon-bearing
paths (including URLs and Windows paths), mixed endings, double-leading slashes, and ambiguous relative roots reject
the candidate. Bare relative roots use letters, digits, `.`, `_`, or `-`; paths starting with `/`,
`./`, or `../` can include spaces and quoted or backslash characters, encoded as JSON strings.
The compressor never reads files or normalizes paths. Both `file_content` and `file_read` origins
bypass this path-list strategy; existing file-read JSON, table, and log handling is unchanged.
For tools outside the search-only route, established source and table detection takes precedence over plain path-list detection. A bounded
path probe stops at its last complete line so a byte cut inside a path or CRLF is not a record.
Outside the dedicated search route, eligibility checks parse the full path list before the
compressor parses it to build a candidate. The candidate then goes through common arbitration.

The path-list view starts with
`[Path list; all received paths retained; full path = base + each paths entry]`, followed by compact
JSON with `base`, `paths`, `line_ending`, and `trailing_newline`. Concatenate `base` with each decoded
`paths` entry, join with `line_ending`, and append that ending only when `trailing_newline` is true
to reconstruct the input byte for byte. The shared prefix always ends at `/`. The full view,
including its header and metadata, must reduce bytes and estimated tokens before arbitration.

PreTool leaves recognized native `find` invocations unchanged so PostTool receives native results. This also
preserves an entire compound command containing such a call; mixed output is not a path list and stays
unchanged. The path-list route itself does not require `find`: other unoptimized tool results with
the same content shape may use it. Transparent shell keywords (`noglob`, `command`, `builtin`,
`exec`, `nocorrect`), `env`, process wrappers (`timeout`, `time`, `nice`, `nohup`), and `uv run`
are inspected through nesting. Process wrappers mirror the pinned RTK v0.49.0 option grammar:
value options, flags, attached values, numeric adjustments, `--`, and positional arguments.
Unknown options stop recognition. The tokenizer removes shell backslash-newline continuations
before inspecting the executable. RTK's source is unchanged. Custom `hooks.transparent_prefixes`
are not read or inferred; a configured custom prefix can still let RTK rewrite wrapped `find`.

This intentionally trades RTK coverage for faithful native execution: mixed compounds, `-print0`,
`-ls`, `-printf`, and other non-path-list actions do not receive path-list compression, and lists
with fewer than three paths or no profitable shared prefix remain unchanged. A leading `.` or bare
start-point directory also rejects the strict path-list format; this version does not add a separate
head-record encoding. In the deterministic
three-command native-find replay, the 60-path list compresses; the mixed compound with two paths
and the single-path query pass through, and every returned path can be read directly. This is fixture
coverage, not a measurement of whole-Agent savings or a comparison with RTK token usage.

Search path sharing is enabled by default. CLI callers can disable it with
`TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`. When unset or empty it stays enabled; `1`, `true`,
and `yes` also enable it (case-insensitively), and any other value disables it. This variable
is independent of the JSON config file.
Rust callers use `RuntimeConfig.search_path_sharing_enabled`; Python callers use
`TokenlessConfig(search_path_sharing_enabled=False)` or the matching `TokenlessRuntime` keyword.
Disabling this domain returns search listings unchanged without computing a search candidate.
Recognized native `find` calls still bypass RTK rewriting when path sharing is disabled. This is a correctness
boundary independent of compression: disabling an optimization must not reintroduce summarized or
altered lookup paths. Revert the change to restore the previous RTK routing policy completely.
JSON, table, and log compression remain available for other tool names. The exact name `Grep`
always excludes those domains to retain every received match; disabling path sharing does not
restore pre-feature JSON/table/log dispatch for a custom tool named `Grep`.
The global compression switch still controls dry-run behavior for enabled domains.
Whole-task savings depend on the workload; preserving all received bytes does not guarantee
fewer follow-up tool calls or lower total token use.

Both views intentionally use `search_path_sharing` to identify the optimization family without
changing Protocol v2. Statistics and `AppliedOperation.SEARCH_PATH_SHARING` cannot distinguish
the two formats. The headers described here are internal rendering details, not versioned SDK
discriminators; no public decoder or stable header constants are exported. These examples explain
the lossless views rather than establishing a programmatic SDK decoding contract.

The numbered-record view begins with `[Search results grouped by file; all received lines retained]`.
Subsequent `File=` lines contain a JSON-encoded path;
data lines always begin with ASCII digits followed by `:`, so they cannot be mistaken for file
headers. To reconstruct, prefix each data line with the decoded current path and `:` and retain
its original line ending. Repeated nonconsecutive paths receive separate headers.

The Claude Code adapter unwraps only native Grep `mode=content` responses without context options,
declares that slot as API response text, and replaces only `content` in the original output object.
It preserves host metadata, including any limits; completeness means all received rows, not all
possible matches before host truncation. Other Grep modes and hosts keep their existing route.
Unwrapped Grep listings are recorded as `api_response` in statistics, rather than `file_content`:
they are filtered tool responses, not authoritative file copies. Historical origin-grouped queries
therefore have a version boundary. Core treats the native search alias family (`Grep`, `grep`,
`grep_code`, `grep_search`, `search_files`) as search-only tools: they cannot enter JSON, table,
or log compressors, even when search path sharing is disabled. Replacement hosts declare the
search text slot — or the whole result when the host's text is the listing — as `api_response`
instead of `file_content`, so identical search text compresses identically through a search tool
and through the shell; file reads keep `file_content` and stay byte-for-byte. SDK callers should
use those names only for this search contract.

RTK ownership is provisional until execution completes. PreTool atomically stores the original and exact executed commands
and assigns each rewritten RTK invocation a generation and ordinal in `rtk-outcomes.db` under
the Runtime data directory, independently of statistics collection. Bundled RTK records whether
it changed any output bytes; a tied or increased token estimate does not prove unchanged output.
PostTool requires its `command` to match the registered command, including the generation, before
consuming evidence once per agent/session/tool call. Missing or modified commands retain the bypass,
so a failed retry registration cannot reuse older receipts. Complete unchanged receipts
allow a successful result back into the normal pipeline using the saved original command, so
file-read origin and other command-based guards still apply. A retry with an untrackable rewrite
attempts to invalidate the previous generation; even if this fails, command matching prevents
reuse by that retry. Late reports cannot recreate a deleted generation.
Changed output, missing or expired receipts, old RTK binaries, or state failures retain the RTK
bypass. Compound calls require receipts for every newly wrapped invocation; existing or
untracked RTK wrappers and ambiguous shell substitutions conservatively disable fallback.
PowerShell wrappers currently have no receipts and retain the RTK bypass.
Receipt lookup failures warn in Core; RTK-side writes fail silently to preserve tool output.
Abandoned receipts expire after one day and an indexed sweep removes them on a later rewrite.
The pipeline retains the host's declared capabilities. Common Hooks, Hermes and OpenClaw declare
`recovery: none` for RTK output before receipt lookup, so their fallback accepts only lossless
candidates; this change does not enable Stash/retrieve recovery for those calls.
Continuing PostTool does not guarantee compression: its candidate must still pass normal
fidelity and savings checks.

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

`make python-wheel` builds `tokenless`, stages it as `anolisa_tokenless/_bin/tokenless`, and
creates a CPython 3.11 stable-ABI platform wheel.
Cross-platform builders may set `PYTHON_TOKENLESS_BINARY` to the `tokenless` executable built for
the same wheel target. `make test-python-runtime` installs the wheel in a fresh environment and
exercises all four lifecycles plus statistics queries without relying on a system `tokenless` or
RTK binary.

`anolisa-tokenless-agentscope` supports AgentScope 1.0.11 through 1.0.x and 2.0.x. The 1.x adapter
uses a Tokenless Toolkit, a model proxy, and public instance hooks. The 2.x adapter uses
`on_model_call` and `on_acting`; 2.0.0 keeps marker state in the paired Middleware/Tool, while later
versions also persist it in `AgentState.middle_context`. Both expose the complete SDK; 2.0.0 supports
direct Agent construction, while App integration starts at 2.0.1. Built-in tools have explicit
contracts; applications must register a `ToolContract` for each custom tool so `ContentOrigin` is
never inferred from output text.
