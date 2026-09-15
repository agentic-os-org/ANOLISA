# Tokenless User Manual

[中文版](../../../zh/token-saving/tokenless/user-manual.md)

Tokenless is designed for tool-heavy AI agents. Its CLI compacts schemas and tool responses, while its adapters can also rewrite shell commands, check tool dependencies, and pass compressed results to an agent. The exact effect depends on the host framework: some adapters replace the original result, while others add compressed context without removing the original.

Start with the [Quick Start](QUICKSTART.md) if this is your first use.

## Build the standalone CLI from source

Source builds are intended for development and debugging. The project currently validates and supports source builds on Linux only:

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/tokenless
cargo build --release --locked -p tokenless-cli
./target/release/tokenless --version
```

This path produces only the standalone `tokenless` CLI. It does not install `rtk` or the agent integration resources. To use the complete feature set in an agent, install through the anolisa CLI as described in the [Quick Start](QUICKSTART.md).

## Build the Python SDK from source

CPython applications can use Tokenless in process instead of starting the CLI for every lifecycle
operation:

```bash
make python-wheel
python3 -m venv /tmp/tokenless-python
/tmp/tokenless-python/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

The build requires a discoverable CPython 3.11+ development environment. The wheel uses the
CPython 3.11 stable ABI and remains specific to the operating system and architecture for which it
was built.

The Python SDK has two layers. The `anolisa-tokenless` package exposes the framework-neutral
`TokenlessSdk`, direct `TokenlessRuntime` operations, and typed `TokenlessStats` queries. The
same-version `anolisa-tokenless-agentscope` package maps that generic lifecycle to AgentScope. See
the [Python SDK guide](sdk.md) for both layers, runnable examples, and configuration.

## Capabilities and boundaries

| Capability | Behavior implemented in the current code | Important boundary |
|------------|------------------------------------------|--------------------|
| Schema compression | Removes `title` and `examples`, removes fenced and inline code from descriptions, collapses whitespace, and truncates descriptions | Common BeforeModel passes lossy transformations through without marker-authorized recovery; OpenCode's per-tool path and the direct CLI still compress (Qwen Code skips the declared event) |
| Content-aware response compression | Successful PostTool JSON is routed to `JsonCompressor`; recognized successful build/test command output is routed to `BuildLogCompressor`; CSV/TSV is routed to `TabularCompressor`; supported search listings use `SearchResultsCompressor`; only a smaller end-to-end result is accepted | Other content domains and Tool Errors pass through; recoverable reduction requires either Marker-authorized framework retrieval or a supported Marker command path |
| Search path sharing | Shares paths across consecutive API search records, including native Claude Grep, retaining all received text and positions | Enabled by default; requires API response origin, text replacement and no-context records; file and command outputs pass through this domain |
| TOON encoding | Encodes JSON and keeps the JSON input when the estimated token count does not decrease | Replaces the original when the host accepts text replacement; hosts without replacement capability pass through |
| Command rewriting | Calls `rtk rewrite` and submits the rewritten shell input when a rule is available | Recognized build/test commands stay native for Build Log handling; other unsupported or denied rewrites pass through |
| Tool Ready | Legacy pre-call checks for declared binaries, versions, configuration, permissions, and optional dependencies | Hard-disabled; it cannot inspect, repair, or block tool execution |
| Stash | Stores content removed by string, array, depth, or schema-description truncation, complete arrays behind record reduction, omitted Build Log progress intervals, and complete original tables behind row reduction | One-hour TTL and 10,000 live entries by default; other removed fields are not stashed |

The implementation contains no fixed saving-rate guarantee. Results depend on the payload, adapter delivery semantics, and the share of the model context that came from tool data. Measure your own workload as described in [Measuring savings](measuring-savings.md).

## How Tokenless participates in a tool call

After an adapter is enabled, a tool call may pass through these stages:

```text
Before the tool: hard-disabled Tool Ready hook → command rewrite
Before the tool: reserve recognized build/test commands; otherwise RTK rewrite → carry output-optimization state
After the tool: status and optimization bypass → JSON/CSV/TSV/Search/Build Log PostTool Pipeline → optional Stash/TOON → statistics
Before the model: schema compression → visible Marker extraction → conditional Retrieve declaration
Retrieve: visible-Marker authorization → byte-identical Stash read
```

This is a capability map, not a pipeline that every framework runs. For example, the content-aware
protocol path currently serves Cosh-NG, OpenClaw, Hermes, Qoder, supported Claude Code releases,
OpenCode, and DeepSeek Harness. Codex and Qwen Code do not replace post-tool output under their
current host contracts. See
[Agent integration](framework-integration.md).

## Behaviors to understand

### Installation does not enable every adapter

`anolisa install tokenless` installs the component and its adapter resources. To make an agent use Tokenless automatically, also run:

```bash
anolisa adapter enable tokenless <framework>
```

CLI-only use does not require an adapter.

### “Compression off” affects only compression operations

With `compression_enabled=false` or `TOKENLESS_COMPRESSION_ENABLED=0`, `compress`,
`compress-schema`, `compress-response`, and `compress-toon` still calculate predicted savings and
may write statistics, but return the original input. They do not write Stash entries in this mode.

This setting does not disable RTK command rewriting, adapter execution, or retrieval. Tool Ready is independently hard-disabled. To stop all Tokenless behavior in an agent, disable the adapter:

```bash
anolisa adapter disable tokenless <framework>
```

### Compression trigger conditions and thresholds

Adapters do not compress every tool result. For response compression, compressed content is produced only when all of the following hold:

1. Compression is not switched off. With `compression_enabled=false` or `TOKENLESS_COMPRESSION_ENABLED=0` the run becomes a dry-run: statistics are still calculated, but the original text is returned (see the previous section).
2. The tool is not a content-retrieval tool. Read/Glob/Grep/LSP/NotebookRead and their aliases skip response compression so their content stays intact. Search path sharing adds one narrow exception: a native Claude Code `Grep` result in no-context content mode is routed to that lossless compressor instead, and still keeps every received match (see [Controlling search path sharing](#controlling-search-path-sharing)).
3. The response reaches the minimum length. The shared response hook, OpenClaw, and Hermes skip responses shorter than 200 characters. Length is counted in characters, not bytes.
4. The content is valid JSON. Threshold-based response compression works on JSON objects and arrays.
   - **4a. Shared response hook path:** output that arrives as plain text (not JSON) is routed to the content-aware text compressors (build/test log terminal cleanup and progress reduction, CSV/TSV table compaction, API search path sharing) described in [Adapter processing rules](framework-integration.md#adapter-processing-rules); the table rules are detailed in [CSV/TSV views can be incomplete](#csvtsv-views-can-be-incomplete) and the search rules in [Controlling search path sharing](#controlling-search-path-sharing).
   - **4b. OpenClaw and Hermes:** both unwrap a shell result's dominant text field (for example the `output` of a `{"stdout": ...}` envelope) and allow text replacement, so Core can also route that plain text to the build-log and CSV/TSV compressors; OpenClaw's structured slots accept JSON only.

   The shared response hook additionally skips skill-like text with YAML frontmatter before spawning a compression subprocess (Core passes such text through anyway).
5. The compressed result is strictly smaller. When neither response compression nor TOON encoding makes the content smaller, the original text is kept.

After these checks, truncation strength depends on the tool category. Categories and thresholds are defined in `tool_categories.json` inside the adapter directory (the single source of truth shared by all adapters); built-in safe fallbacks are used when the file is missing or invalid:

| Category | Representative tools | String truncation threshold | Array item cap | Maximum nesting depth |
|----------|----------------------|------------------------------|----------------|-----------------------|
| Content retrieval | Read, Glob, Grep, LSP, NotebookRead and aliases | Compression skipped | — | — |
| Shell/exec | Bash, Shell, exec, terminal, etc. | 65,536 characters | 128 items | 8 |
| Other structured tools | Any tool not in the two categories above | 1,048,576 characters | 65,536 items | 32 |

Threshold semantics: a string longer than the threshold is cut at the threshold (retrievable through Stash when Stash is enabled); an array over the cap keeps only its leading items while the tail is truncated (also retrievable through Stash when enabled); subtrees nested deeper than the depth cap collapse into a truncation marker.

Per-path differences worth noting:

- Running `tokenless compress-response` standalone uses the CLI's own defaults (4,096 characters / 32 items / depth 8), overridable with `--truncate-strings-at`, `--truncate-arrays-at`, and `--max-depth`; see the [CLI reference](cli-reference.md).
- Codex and Qwen Code do not run response compression under their current host contracts: Codex relies on RTK source reduction plus environment-failure diagnostics, and Qwen Code has no post-tool replacement slot. See the adapter table below for what each integration provides.
- The OpenClaw plugin reads the same `tool_categories.json` lists to map each tool to a content origin (file content, command output, or API response), falling back to its built-in lists when that file is missing or invalid; Core then applies the matching thresholds. Its former `skip_tools` and `shell_tools` overrides have been removed and no longer control the adapter. See [Configuration and data privacy](configuration-and-privacy.md) for the current options.
- TOON encoding is a separate trigger decision: it runs only on payloads of at least 500 characters and only when the host slot accepts text, and it is adopted only when the encoded result is smaller than the current content.
- The AgentScope framework integration does not use the adapter thresholds above; it selects thresholds by `conservative` / `balanced` / `aggressive` mode. See [Framework integration](framework-integration.md).

### Controlling search path sharing

API search path sharing is enabled by default. Set `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`
in the agent process environment to disable it through the CLI. When unset it stays enabled;
`1`, `true`, and `yes` also enable it (case-insensitively). Empty and other values disable it.
This setting is independent of `config.json`. Python SDK callers can disable it with
`TokenlessConfig(search_path_sharing_enabled=False)`; Rust callers set
`RuntimeConfig.search_path_sharing_enabled` to `false`. All entry points default to enabled.

Disabling this feature returns search listings unchanged. JSON, table, and log compression
remain available for other tool names. The exact name `Grep` always excludes those compressors
to preserve received matches, even with path sharing disabled. A custom tool named `Grep`
therefore cannot restore its pre-feature JSON/table/log compression through this switch.
Supported no-context Claude Grep results retain all received matches; file reads and command outputs,
including Bash without RTK, do not enter search path sharing. Other API tools can use the same
Core capability. Whole-task savings depend on the workload; smaller search results do not
guarantee lower total token use.

### CSV/TSV views can be incomplete

Successful CSV/TSV tool results can be compressed when the host can replace output with text.
File-origin results, failed tools, RTK-optimized output and Retrieve output pass through.
A supported table has a header and at least two data rows of equal width, with an unambiguous
comma or tab delimiter. Malformed quoting, ambiguous delimiters, single-column text, Markdown
and fixed-width tables are not compressed by this compressor.

Full compaction preserves all cell strings, including empty cells, duplicate headers, leading
zeros and large numeric strings. It removes unnecessary quoting and normalizes record separators;
embedded cell line endings remain unchanged. This preserves cells, not the original bytes.
A full view saving at least 15% of estimated tokens takes priority.

Row reduction requires column labels: each nonempty header starts with a Unicode letter or `_`,
then contains only letters, numbers, `_`, `-` or `.`; at least one label must be nonempty.
Duplicate and empty labels are allowed. Headers containing spaces, expressions or sentence
punctuation keep all rows, preventing the reported source/prose patterns from being sampled.
This conservative heuristic also skips reduction for some genuine tables.

Otherwise, tables with more than 32 data rows may retain the first and last four rows,
rows containing diagnostic keywords, and evenly spaced ordinary rows up to a base budget of 32.
Protected rows may exceed that budget. The notice outside the table states the retained and total
row counts, original one-based data row ranges excluding the header, and how to recover the source.
The complete original CSV/TSV is stored in Stash; retrieval returns its original bytes.
Retrieve before complete enumeration or calculations: selected rows are an incomplete view.
Missing recovery or a failed Stash write permits only full compaction or the original input.
The same applies if the exact source-range list exceeds 1 KiB; diagnostic rows and their
provenance are never partially reported.

A reduced candidate must use fewer characters and estimated tokens than both the original and
full view, including the notice. These checks do not guarantee savings with every model tokenizer.

### Native Grep keeps every received match

On Claude Code 2.1.121 or newer, native Grep content results can share repeated file paths.
A `File="..."` heading supplies the full path for the following `line:text` rows, until the next
file heading. All received records, source text, whitespace and line endings are retained.
The view is used only when it is smaller; it needs no Stash entry or retrieval command.

This first version supports no-context `path:line:text` listings with at least three records
and paths without colons. Context queries, count/file-list modes, unsupported listings and
file reads keep their existing behavior. Bash searches continue through RTK. Grep may already
have applied a host limit before Tokenless receives the result; path sharing does not recover
those missing matches. Lower first-result size does not guarantee lower total task cost.

### Reversible compression is conditional

Active response and schema truncation stash the removed payload in
`~/.tokenless/stash.db` by default and add a marker such as:

```text
<<tokenless:0123456789abcdef01234567>>
```

The payload can be recovered locally through the trusted `tokenless retrieve` command. Supported
CLI adapters put that exact command in the Marker and let the model run it through an existing
shell tool; they enable recoverable compression only when bare `tokenless` is resolvable on the
shell `PATH`. DSH also requires that command to resolve to the same executable selected for its Core
call. AgentScope instead authorizes its static retrieval Tool against the model's current
`visible_markers` set. The old stateless MCP server was removed because it had no trustworthy
model-visibility context. Recovery is unavailable when:

- `--no-stash` was used.
- Compression was running in dry-run mode.
- The Stash database was unavailable or a write failed.
- The entry exceeded its TTL.
- The 10,000-live-entry capacity evicted an older entry.
- The caller uses a different Stash database path.
- In DSH, bare `tokenless` is missing from a stable absolute `PATH` entry or resolves to a different
  executable than `tokenlessBin`/`TOKENLESS_BIN`.

Stash does not make all compression reversible. Removed `debug`/`trace` fields, `null` and empty values, schema `title`/`examples`, and Markdown formatting are not stored for retrieval. Validate critical payloads with representative data before enabling active compression.

### Processing errors usually fail open

Compression and rewrite hooks normally return no modification when `tokenless` or `rtk` is missing
or compression provides no savings. For Protocol v2 `compress`, normal non-application outcomes
return a result with exit code `0`; malformed transport exits `2`, while RTK timeout, unauthorized
Retrieve, Stash failure, and Pipeline failure exit `1` without response JSON. Tool Ready is
hard-disabled before its legacy check, repair, and blocking logic. Post-tool failure attribution is
independent and remains unchanged.

Command rewriting also changes the shell command submitted by the host. Most adapters replace the command input directly; Hermes blocks the first call and tells the agent to retry with the rewritten command. Validate important command workflows as well as compressed output.

## Supported Agent adapters

| Agent product | Integration | Current code path |
|-----------|-------------|-------------------|
| cosh | Extension | Hard-disabled Tool Ready, rewrite, Schema; Cosh-NG replaces eligible pipeline output and supports Marker command recovery, while legacy Copilot Shell passes post-tool output through |
| OpenClaw | Plugin | Hard-disabled Tool Ready, `exec` rewrite, persisted-result replacement, optional TOON; no Schema |
| Hermes | Plugin | Hard-disabled Tool Ready, Core-owned block-and-retry rewrite, result replacement with Core-selected TOON, Marker command recovery; no Schema |
| Qoder | Plugin | Hard-disabled Tool Ready, rewrite, response pipeline and Marker command recovery through `updatedToolOutput`; no Schema |
| Claude Code | Marketplace plugin | Hard-disabled Tool Ready, Bash rewrite, response replacement and Marker command recovery on Claude Code 2.1.121 or later; conditional TOON; no Schema |
| Codex | Plugin | Hard-disabled Tool Ready, RTK rewrite, environment-failure diagnostics; no response/TOON replacement or Schema |
| OpenCode | Plugin | Hard-disabled Tool Ready, Bash rewrite, tool-output replacement with response + TOON, Marker command recovery, Schema |
| Qwen Code | Extension | Hard-disabled Tool Ready, rewrite; current host lacks post-tool replacement and skips the declared BeforeModel event |
| DeepSeek Harness | Native plugin | Single-text result replacement, Marker command recovery, and environment-error attribution; no Schema or command rewrite |

## Supported Agent development frameworks

| Framework | Integration | Current code path |
|-----------|-------------|-------------------|
| AgentScope | In-process Python middleware | Replaces successful final tool responses and exposes a marker-scoped retrieval Tool through a separate Python package |

## Find documentation by task

| I want to | Document |
|-----------|----------|
| Install and verify for the first time | [Quick Start](QUICKSTART.md) |
| Build the standalone CLI from source | [This page · Build the standalone CLI from source](#build-the-standalone-cli-from-source) |
| Use the in-process Python SDK | [Python SDK](sdk.md) |
| Integrate AgentScope | [AgentScope SDK integration](sdk/agentscope.md) |
| Connect an Agent product | [Agent integration](framework-integration.md) |
| Compress or retrieve manually | [CLI reference](cli-reference.md) |
| Understand when compression triggers and what the thresholds are | [This page · Compression trigger conditions and thresholds](#compression-trigger-conditions-and-thresholds) |
| Inspect savings or content changes, or run a dual comparison | [Measuring savings](measuring-savings.md) |
| Change settings or understand local data | [Configuration and data privacy](configuration-and-privacy.md) |
| Fix missing statistics, adapter, or Stash issues | [Troubleshooting](troubleshooting.md) |
| Diagnose missing schema-compression records | [Troubleshooting · Schema compression produces no statistics](troubleshooting.md#schema-compression-produces-no-statistics) |
| Upgrade or uninstall | [Troubleshooting · Upgrade and uninstall](troubleshooting.md#upgrade-and-uninstall) |

## Recommended rollout

1. Complete the [Quick Start](QUICKSTART.md) with non-sensitive test data.
2. Record a dry-run baseline for the same task.
3. Enable active compression and compare both output quality and savings.
4. Confirm that local-data and SLS behavior meets your requirements.
5. Enable the adapter for production agents.

The `tokenless --help` output from the installed version is the final authority for CLI and configuration behavior.
