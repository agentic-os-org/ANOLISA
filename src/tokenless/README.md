# Token-Less

[中文版](README_zh.md)

**LLM token optimization toolkit** — content-aware compression + command rewriting + diagnostics.

Token-Less combines complementary strategies to minimize LLM token consumption:

- **Lifecycle-aware Compression** — Protocol v2 owns BeforeModel schema handling, PreTool RTK rewriting, PostTool routing, and authorized Retrieve; the PostTool Pipeline compresses JSON, CSV/TSV tables, supported search listings, and recognized build/test command logs.
- **TOON Context Compression** — Encodes JSON responses to TOON (Token-Oriented Object Notation) format via the `toon-format` library linked into `tokenless`, reducing syntax overhead for suitable structured data.
- **Command Rewriting** — Integrates [RTK](https://github.com/rtk-ai/rtk) to filter and rewrite CLI command output, eliminating noise that would otherwise waste 60–90% of tokens.
- **Tool Ready (legacy, hard-disabled)** — Its pre-call dependency checks are retained in source but unconditionally bypassed while the readiness model is redesigned.

The bundled RTK 0.49.0 preserves native `grep -l` / `-m` semantics, rewrites pipelines
conservatively, and leaves `sudo` commands unchanged. RTK recovery hints use `rtk recall`; retained output is scoped to the host OS user,
not isolated by Tokenless tenant or session.
See [bundled RTK commands](../../docs/user-guide/en/token-saving/tokenless/cli-reference.md#bundled-rtk-commands)
for flag migration, pipeline behavior, and output recovery.

Agent adapters are available for:

- **OpenClaw plugin** — delegates PreTool RTK rewriting and PostTool optimization to Protocol v2 Core.
- **copilot-shell hook** — intercepts Shell commands via a PreToolUse hook and delegates to RTK for command rewriting + output filtering.
- **Hermes Agent plugin** — delegates block-and-suggest command rewriting and model-bound result optimization to Core, with Marker-directed recovery through its shell tool.
- **Qoder CLI plugin** — registered but hard-disabled Tool Ready, command rewriting, response compression, and Marker-directed recovery via Qoder's native hook system.
- **Claude Code plugin** — RTK command rewriting, response/TOON compression, Marker-directed recovery, and registered but hard-disabled Tool Ready via Claude Code's official plugin marketplace.
- **Codex plugin** — RTK command rewriting, environment-failure diagnostics, and registered but hard-disabled Tool Ready via Codex's native hook system.
- **OpenCode plugin** — schema/response/TOON compression, Marker-directed recovery, registered but hard-disabled Tool Ready, and command rewriting via OpenCode's local plugin API.
- **Qwen Code extension** — command rewriting and registered but hard-disabled Tool Ready; current host releases cannot replace post-tool output and skip the declared schema event.
- **QwenPaw plugin** — schema compression, RTK command rewriting, response/TOON compression, and static-tool recovery through an AgentScope middleware registered by QwenPaw's plugin system; the plugin embeds the `anolisa_tokenless` wheel in-process.
- **DeepSeek Harness plugin** — native response compression, Marker-directed recovery, and environment-error attribution through DSH's `tools/post-execute` seam.

For framework developers, the Python SDK has a framework-neutral layer and an **AgentScope-specific
layer**. Together they cover schema compression, RTK rewriting, response compression, TOON,
retrieval, and attribution.

## Features

| Capability | Savings indicator | Details |
|---|---|---|
| Schema compression | 47.3% on reference fixture | Compresses OpenAI Function Calling tool schemas |
| Content-aware response compression | 36.3% lossless savings on the JSON reference fixture | Routes successful JSON through `JsonCompressor`; lossless candidates saving at least 15% take priority, while recoverable record arrays can be reduced to a 32-record base budget |
| Build-log compression | workload-dependent | Cleans terminal control output and reduces repeated routine progress in recognized Cargo, pytest, npm/Jest, Go, Make/C, and generic command logs while preserving diagnostics, summaries, phases, and stack traces |
| Search path sharing | workload-dependent | Enabled by default: API search listings, including Claude native Grep, share consecutive file paths and retain every received match; disable with `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` or SDK `search_path_sharing_enabled=False`; command output remains on its existing route |
| Git Diff context cropping | workload-dependent | Opt in with `TOKENLESS_DIFF_COMPRESSION_ENABLED=1` or SDK `diff_compression_enabled=True`; preserves all changed lines, reduces context per hunk, and provides original recovery. Disabled by default; stable whole-Agent token savings have not been established |
| HTML page rendering | workload-dependent | Opt in with `TOKENLESS_HTML_EXTRACTION_ENABLED=1` or SDK `html_extraction_enabled=True`; renders complete HTML documents from commands or APIs as Markdown, removes only enumerated non-content elements (scripts, styles, navigation, banners, footers, asides, form controls, media embeds) with counts in the view header, and provides original recovery. Disabled by default; file reads pass through |
| CSV/TSV table compression | workload-dependent | Preserves every cell when compacting quoting and record separators; larger tables can retain selected rows with an explicit incomplete-table notice and byte-exact original retrieval. Requires a text replacement slot; file reads pass through |
| Reversible compression (stash) | — | Omitted record collections and bounded values are stashed; supported agents run `tokenless retrieve HASH` or call their static Retrieve Tool when full data is needed |
| TOON context compression | 17.0% on reference response | Encodes JSON to TOON format for LLMs |
| Command rewriting | 60–90% | Filters CLI output via RTK (70+ commands supported) |
| Tool Ready | reduces retry waste | Legacy pre-call check, auto-fix, and blocking; hard-disabled |
| OpenClaw plugin | — | RTK ✅, lossless transcript PostTool ✅, Schema/Retrieve unavailable in the host |
| copilot-shell hooks | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅; Cosh-NG supports response compression and Marker-command recovery, while legacy copilot-shell remains lossless-only; Common BeforeModel passes schemas through without authorized Retrieve |
| Hermes Agent plugin | — | Tool Ready ⛔ hard-disabled, Core-owned command rewriting/response/TOON ✅, Marker-command recovery ✅, Schema unavailable |
| Qoder CLI plugin | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅, Response compression ✅, Marker-command recovery ✅ |
| Claude Code plugin | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅, Response compression ✅, TOON ✅, Marker-command recovery ✅ on Claude Code 2.1.121 or newer |
| Codex plugin | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅, Environment diagnostics ✅, Response compression — protocol-blocked |
| OpenCode plugin | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅, Schema compression ✅, Response compression ✅, TOON ✅, Marker-command recovery ✅ |
| Qwen Code extension | — | Tool Ready ⛔ hard-disabled, Command rewriting ✅, Response/Schema replacement unavailable in current host |
| QwenPaw plugin | — | Schema compression ✅, Command rewriting ✅, Response compression ✅, TOON ✅, Retrieve Tool recovery ✅ |
| DeepSeek Harness plugin | — | Response compression ✅, Marker-command recovery ✅, Environment-error attribution ✅ |
| AgentScope framework integration | — | Schema ✅, RTK ✅, Response ✅, TOON ✅, Retrieval ✅ |
| Zero runtime deps | — | Pure Rust, single static binary |

The schema, response, and TOON figures above are isolated results on the
repository's committed reference fixtures; they are neither a
production range nor additive. Compression depends on payload size and shape,
removable fields, configured thresholds, and the share of tool data in the
session. Short or already compact payloads may save only a few percent or pass
through unchanged. See [Measuring Tokenless Savings](../../docs/user-guide/en/token-saving/tokenless/measuring-savings.md#run-the-repository-reference-workload)
for the exact inputs, command, full output, and limitations.

## Applicable Scenarios & Expected Effects

tokenless optimizes the tool-related content it handles—tool schemas, tool/API
responses, and supported shell output—before it enters the LLM context. It does
not touch model reasoning or conversation history. The payoff depends heavily
on the share and shape of that content in the session.

### Where it pays off

| Workload | Primary strategy | Why |
|----------|-----------------|-----|
| Shell-heavy (build/test/triage) | Build-log compression + RTK | Recognized build/test commands keep their native output for PostTool compression; other supported shell commands use RTK |
| API/fetch-heavy (REST, web_fetch) | Response compression + TOON | JSON may carry removable debug/null/empty fields; sufficiently large, regular structures also have reducible syntax overhead |
| Agents with many tools | Schema compression | Many Function Calling definitions carry verbose descriptions and removable metadata |
| Long responses that must stay faithful | Reversible compression (Stash) | Truncated content is `retrieve`-able end-to-end lossless; thresholds can be tightened safely |

### Where it pays little or doesn't apply

- **Chat-heavy / few tool calls**: tool-response share is tiny, overall savings approach 0.
- **No fixed minimum payload**: `compress-schema` and `compress-response` build a
  candidate for every accepted valid JSON input. In active mode, they emit it only
  when its estimated token count is strictly lower than the original. A small input
  with removable content can still compress, while a larger already-compact input can
  pass through unchanged; the CLI writes the reason to stderr and records no stats.
  In dry-run mode, the CLI always emits the original and may record a smaller candidate
  as a predicted saving.
- **Model inference tokens / billed tokens**: outside what tokenless touches.

### Estimating the effect

> The shares below are **illustrative estimates** that vary widely by task, not measured constants.

| Session component | Typical share | tokenless can optimize |
|-----------|-------------|----------------------|
| LLM reasoning output (text generation) | ~35% | ❌ Not involved |
| LLM input (system prompt + conversation history) | ~40% | ❌ Not involved |
| Tool call arguments | ~5% | ❌ Not involved |
| **Tool responses (API returns + command output)** | **~20%** | **✅ Optimization scope** |

**Actual savings rate = reported compression rate × tool response share**

Example: dashboard shows 60% compression rate, but if tool responses account for 20% of total consumption, the actual savings rate is 60% × 20% = **12%**. This is why savings feel "lighter than a feather" in experiments consuming 15 million tokens — tokenless only optimizes the ~3 million tokens of tool responses.

> Recoverable omissions include an optional action: `If needed, run in shell: tokenless retrieve HASH`. AgentScope instead sees `If needed, call tool tokenless_retrieve with hash_or_marker=HASH` (using its configured static Tool name). Recovery returns the saved payload while it remains in Stash; it costs additional tokens and is not required for every omission. Historical `<<tokenless:HASH>>` markers remain readable but are no longer generated. Use `TOKENLESS_COMPRESSION_ENABLED=0/1` dual runs to compare real savings.
> See [user manual](../../docs/user-guide/en/token-saving/tokenless/user-manual.md) for per-strategy trigger conditions.

## Architecture

```
Token-Less/
├── crates/tokenless-schema/   # BeforeModel tool-schema compressor
├── crates/tokenless-ccr/      # Reversible compression stash (Compress-Cache-Retrieve)
├── crates/tokenless-runtime/  # Lifecycle API and Runtime-owned PostTool pipeline
├── crates/tokenless-protocol/ # Versioned adapter contract and token estimator
├── crates/tokenless-compressors/ # JSON, tabular, and build-log compressors
├── crates/tokenless-cli/      # CLI binary: `tokenless` command (env-check, compress, retrieve, stats)
├── python/tokenless/          # PyO3 package: `anolisa_tokenless`
├── python/agentscope/         # Pure-Python AgentScope integration package
├── adapters/tokenless/        # FHS bundle for Agent plugins, hooks, and extensions
│   ├── manifest.json            # Adapter manifest for supported Agent products
│   ├── common/                  # Shared: hooks, spec, env-fix, commands, cosh-extension
│   │   ├── hooks/               # copilot-shell hooks (tool-ready + rewrite + compression)
│   │   ├── cosh-extension.json  # copilot-shell extension manifest (references common/hooks/)
│   │   ├── tool-ready-spec.json # Dormant legacy dependency specification
│   │   ├── tokenless-env-fix.sh # Auto-fix script for missing deps
│   │   └── commands/            # Hook command configs
│   ├── openclaw/                # OpenClaw plugin + agent scripts
│   ├── hermes/                  # Hermes Agent plugin + scripts
│   ├── qoder/                   # Qoder CLI plugin + scripts
│   ├── claude-code/             # Claude Code plugin + marketplace + hooks
│   ├── codex/                   # Codex plugin + scripts
│   ├── opencode/                # OpenCode local plugin + scripts
│   ├── qwenpaw/                 # QwenPaw plugin (AgentScope middleware) + scripts
│   └── dsh/                     # Native DeepSeek Harness bundle
├── third_party/rtk/           # RTK vendored source (justfile clone+patch from GitHub)
├── third_party/patches/      # Patches for vendored third_party sources
├── Makefile                   # Unified build system
└── scripts/                    # Helper scripts
```

## Quick Start

Install the published component with the ANOLISA CLI:

The install script places `anolisa` in `~/.local/bin`, and a user-mode
Tokenless installation places `tokenless` and `rtk` in that same
directory. Export it once if the current shell has not picked it up yet.

```bash
curl -fsSL https://get.agentic-os.sh | bash

# Make the default install directory available in this shell
export PATH="$HOME/.local/bin:$PATH"
anolisa --version
anolisa install tokenless
tokenless --version
```

Alinux users with the YUM repository configured may install the RPM instead:

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

Installing the CLI from the same YUM repository makes it available on sudo's
system path. `adopt` then records the directly installed RPM in system state so
adapter commands can use its component contract.

Current public packages support Linux x86_64/aarch64 and macOS Apple Silicon.
Intel macOS does not currently have a published package. The repository's npm
packaging sources are for release construction and are not a public
`anolisa-tokenless` installation route. The retained
`@anolisa/tokenless-darwin-x64` optional-dependency entry describes a release
build target; it does not indicate registry availability.

ANOLISA-managed and adopted RPM installations place the available adapters
without changing an Agent product's user configuration. Run these commands
as the user who owns that configuration, and enable only the adapter you need:

```bash
anolisa adapter scan
anolisa adapter enable tokenless openclaw
anolisa adapter status tokenless
```

DeepSeek Harness requires at least one explicit profile name. When enabling
multiple profiles, pass every name in the same command; see the plugin section
below for the complete-set behavior. Use an enabled name when starting DSH:

```bash
anolisa adapter enable tokenless dsh --profile <profile>
dsh --profile <profile>
```

Developers building from source can use:

```bash
# Clone repo (no submodules needed)
git clone <repo-url>
cd Token-Less

# Full setup: build + install binaries + deploy all adapters
make setup
```

The source setup installs `tokenless` to `~/.local/bin`, places the `rtk`
helper alongside it, and deploys all adapters for development.

### Build the Python SDK

Framework authors can build the in-process Python API from source:

```bash
make python-wheel
python3 -m venv /tmp/tokenless-python
/tmp/tokenless-python/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

This target requires a discoverable CPython 3.11+ development environment and
uses `uvx` to provision Maturin by default. Install
[`uv`](https://docs.astral.sh/uv/) first, or run
`make python-wheel MATURIN=maturin` with a compatible Maturin already on
`PATH`. The same Python environment is required by `cargo test --workspace`;
plain workspace-default Cargo commands exclude the Python extension.

The `anolisa_tokenless` module supports CPython 3.11 and later on the platform
where its native wheel was built. It exposes the four Tokenless lifecycle
methods and bundles the matching RTK executable; TOON is linked into the native
runtime. It does not require the Tokenless CLI or system helper binaries. The
package is built and tested in this repository but is not yet published to
PyPI. See the [Python SDK guide](../../docs/user-guide/en/token-saving/tokenless/sdk.md) for
runnable lifecycle and Stats examples, the
[AgentScope SDK integration](../../docs/user-guide/en/token-saving/tokenless/sdk/agentscope.md) for
AgentScope attachment, the
[Agent integration guide](../../docs/user-guide/en/token-saving/tokenless/framework-integration.md)
for product adapters, and the [runtime design](docs/design/runtime-library.md) for internal contracts.

The same wheel provides typed, read-only statistics queries without requiring
the CLI. Point `TokenlessStats` at the state directory used by the runtime, or
use the lazy `sdk.stats` client:

```python
from anolisa_tokenless import TokenlessStats

stats = TokenlessStats("/absolute/path/to/tokenless-data")
summary = stats.summary()
print(summary.total.tokens_saved, summary.total.tokens_saved_percent)
```

Token counts are estimates and only operations with positive savings are
recorded. `show()` and detailed `diff()` results may contain sensitive tool
input and output stored in `stats.db`. Read-only describes the API surface:
opening the client follows CLI initialization and may create or migrate
`stats.db`, so the data directory must be writable. `summary(limit=None)` and
`compare(..., limit=None)` inspect at most the newest 10,000 records. For a
session or tool-use diff, at most the newest 10,000 matching records are read.
For a meaningful comparison, pass a dry-run session first and an active
Tokenless session second.

## CLI Usage

The standalone `compress-schema` and `compress-response` commands use this
content-dependent savings check rather than a fixed byte or character minimum.
The description, string, array, and depth limits in the
[CLI reference](../../docs/user-guide/en/token-saving/tokenless/cli-reference.md)
trigger individual transformations; they are not minimum total payload sizes.
Agent adapters may apply separate pre-check thresholds; see the
[Agent integration guide](../../docs/user-guide/en/token-saving/tokenless/framework-integration.md#adapter-processing-rules).

### compress

Shared Agent hooks send lifecycle requests to `tokenless compress`; only
successful, non-bypassed PostTool JSON, CSV/TSV tables, and eligible command-output build logs
enter the Runtime-owned Pipeline. Tool errors bypass compression and keep their
original output while Core attaches environment-diagnostic context.

PreTool leaves recognized Cargo, pytest, npm/Jest, Go, and Make build/test
commands unchanged so their native output has a single PostTool owner. Other
supported commands may be rewritten by RTK, and their results continue to
bypass PostTool compression.

Claude Code 2.1.121 or newer, Qoder CLI, OpenCode, and Cosh-NG can replace the
live result. Their PostTool requests enable recovery when bare `tokenless`
also resolves on the shell `PATH`. A compression Marker then tells the model to
run the exact `tokenless retrieve` command through the existing shell tool. The
hook recognizes only a successful, standalone command
with a valid Hash or Marker and sends its output through the Retrieve bypass,
preventing a second compression pass. Legacy copilot-shell and other hosts that
cannot replace the result remain lossless-only. BeforeModel Schema compression
still requires its separate authorized recovery capability. See the
[CLI reference](../../docs/user-guide/en/token-saving/tokenless/cli-reference.md#compress)
for the request/response contract and an executable example.

### compress-schema

Compress a single tool schema:

```bash
# From file
tokenless compress-schema -f tool.json

# From stdin
cat tool.json | tokenless compress-schema
```

Compress a batch of tools (JSON array):

```bash
tokenless compress-schema -f tools.json --batch
```

A top-level request object with a `tools` array is also accepted without
`--batch`; OpenAI wrappers, Gemini `functionDeclarations` tool objects, and
bare Function Calling declarations are compressed while non-function tools and
fields outside `tools` are preserved:

```bash
tokenless compress-schema -f request.json
```

### compress-response

Compress an API response:

```bash
# From file
tokenless compress-response -f response.json

# From stdin
curl -s https://api.example.com/data | tokenless compress-response
```

Long arrays are truncated to a head+tail window: the first
`--truncate-arrays-at` items (default 32) plus the last
`--array-tail-preserve` items (default 8), with a truncation marker in
between; pass `--array-tail-preserve 0` for head-only truncation. By default
`compress-response` stashes the dropped middle segment so it can be retrieved
later (see [Reversible compression](docs/stash-reversible-compression.md)).
Pass `--no-stash` for lossy truncation, or `--stash-db <path>` to override the
stash database (default `~/.tokenless/stash.db`).

### retrieve

Recover a payload stashed during `compress-response`. Accepts a bare 24-hex
hash or any text containing a `<<tokenless:HASH>>` marker:

```bash
# Bare hash
tokenless retrieve c30ccf5ed1125e0ed871ba8e

# Historical markers remain accepted
tokenless retrieve '<<tokenless:c30ccf5ed1125e0ed871ba8e>>'
```

### compress-toon / decompress-toon

Encode JSON to TOON format (or decode back to JSON). Payloads shorter than
500 characters pass through unchanged by default (the same minimum the
adapter hooks apply); use `--min-toon-chars 0` to encode them anyway:

```bash
# Encode JSON to TOON (short payload, gate disabled for this call)
echo '{"name":"Alice","age":30}' | tokenless compress-toon --min-toon-chars 0
# name: Alice
# age: 30

# Decode TOON back to JSON
echo 'name: Alice\nage: 30' | tokenless decompress-toon
# {"name":"Alice","age":30}
```

### Inspect token savings

Use `stats summary` for totals, `show` for the stored before/after payload, or
`diff` to explain the estimated token saving and highlight only changed lines:

```bash
tokenless stats summary
tokenless stats summary --limit 1000
tokenless stats summary --compare <baseline-session> <active-session>
tokenless stats show 42
tokenless stats diff 42
tokenless stats diff --session <session-id>
tokenless stats diff --session <session-id> --tool-use-id <tool-use-id>
tokenless stats diff 42 --json
```

`stats summary --limit` must be a positive integer; `--limit 0` is rejected at
parse time. `--compare` fails if either session has no records instead of
reporting 0% savings. Session overviews contain metrics only. Record and
tool-use reports include a unified content diff; consecutive active stages are
linked only when their stored output/input content matches exactly, avoiding
duplicate intermediate token counts. See
[Measuring Tokenless Savings](../../docs/user-guide/en/token-saving/tokenless/measuring-savings.md)
for options and measurement limits.

### Trace correlation

Exported SLS records carry the trace identity of the host span they were
produced under, so an observability backend such as AgentLoop can attribute
token savings to a trace. Two optional environment variables supply it:

- `TOKENLESS_TRACEPARENT` — adapter-facing override, read first.
- `TRACEPARENT` — standard W3C variable, used when the override is absent,
  empty, or unparsable.

Injecting one is the launcher's responsibility: OpenTelemetry keeps the active
span in an in-process carrier and does not export it to child processes, so a
host or adapter that wants correlation must set one of them before spawning
Tokenless. Without a usable context the record shape is unchanged, and the
identity is written only to the SLS JSONL — never to the local `stats.db`. See
[Measuring Tokenless Savings](../../docs/user-guide/en/token-saving/tokenless/measuring-savings.md)
and
[Configuration and Data Privacy](../../docs/user-guide/en/token-saving/tokenless/configuration-and-privacy.md).

### Database location

Tokenless stores statistics and reversible-compression data in
`~/.tokenless/stats.db` and `~/.tokenless/stash.db`. Set one directory for both:

```bash
export TOKENLESS_DATA_DIR="$HOME/path/to/tokenless-data"
```

The directory may be any absolute path the current user can access, including
a managed service directory under `/var/lib`; filesystem root, relative paths,
and parent traversal are rejected. The existing `TOKENLESS_STATS_DB`,
`TOKENLESS_STASH_DB`, and `--stash-db` overrides take precedence but must stay
under the real user home or selected data directory. Configuration remains at
`~/.tokenless/config.json`.

## copilot-shell Hooks

The adapter provides hooks that are auto-discovered by copilot-shell via the cosh extension manifest:

| Hook | Event | File | Description |
|------|-------|------|-------------|
| Tool Ready (hard-disabled) | PreToolUse (all tools) | `tool_ready_hook.sh` | Silent pass-through; no check, repair, context, or block |
| Command rewriting | PreToolUse (Shell) | `rewrite_hook.py` | Rewrite commands via RTK |
| Response compression + attribution + TOON | PostToolUse | `compress_response_hook.py` | Compress + env error attribution + TOON |
| Schema compression | BeforeModel | `compress_schema_hook.py` | Passes through lossy transformations until the host exposes marker-authorized recovery |

### Install

```bash
make cosh-extension-install  # or: make openclaw-install, make hermes-install
```

Hooks are registered via the cosh extension manifest (`cosh-extension.json`) and auto-discovered by copilot-shell — no manual `settings.json` configuration needed.

## Tool Ready

Tool Ready was designed to prevent wasted LLM tokens from retrying commands that fail due to missing environment dependencies.

**Legacy behavior**: Before each tool call, the `tool_ready_hook.sh` hook checked the tool's dependency list (from `tool-ready-spec.json`). Missing dependencies could produce `NOT_READY` with "Skip retry" guidance.

Tool Ready is currently hard-disabled across all adapters. Its registered hooks return before reading the dependency specification, checking the environment, attempting repair, or emitting a block decision. No environment variable can re-enable the legacy behavior; doing so requires an intentional source change and a new release.

Post-tool failure attribution, response compression, command rewriting, TOON encoding, Stash, and statistics are independent and remain active.

### env-check CLI

```bash
# Report the disabled state for a specific tool
tokenless env-check --tool Shell

# Report the disabled state for all tools
tokenless env-check --all

# Report the disabled state for checklist mode
tokenless env-check --checklist

# Machine-readable disabled state; no tools/summary checklist is emitted
tokenless env-check --checklist --json

# Accepted for compatibility; does not inspect or repair the environment
tokenless env-check --tool Shell --fix
```

These commands currently report that Tool Ready is hard-disabled and do not inspect or modify the environment.
Every JSON mode returns exactly the same three-field schema:

```json
{"tool":"checklist","status":"UNKNOWN","enabled":false}
```

`tool` identifies the requested tool or the `all`/`checklist` scope. The dormant
legacy `tools` and `summary` checklist fields are never emitted while the hard
bypass is active.

### Configuration

The dormant legacy per-tool dependencies remain in `tool-ready-spec.json`
(shipped within the adapter bundle at `common/tool-ready-spec.json`). The hard
bypass does not read this file:

```json
{
  "Shell": {
    "required": [
      { "binary": "jq", "package": "jq", "manager": "apt" }
    ],
    "recommended": [
      { "binary": "rtk", "version": ">=0.35", "package": "rtk", "manager": "cargo",
        "fallback": [
          { "method": "symlink", "binary": "rtk", "source": "/usr/libexec/anolisa/tokenless/rtk" }
        ]
      }
    ]
  }
}
```

String format `"jq"` is also supported (auto-converts to object).

## OpenClaw Plugin

The plugin translates two OpenClaw events into Protocol v2 lifecycle operations:

| Hook | Event | Action | Status |
|---|---|---|---|
| Tool Ready | `before_tool_call` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| PreTool | `before_tool_call` | Sends `exec` arguments to Core and applies the returned RTK rewrite | ✅ Active |
| PostTool | `tool_result_persist` | Rewrites supported OpenClaw-owned transcript tool results | ✅ Active |
| BeforeModel / Retrieve | — | OpenClaw exposes neither a reliable schema-transform seam nor marker-authorized recovery | — |

Core owns RTK execution, JSON detection, cleanup, TOON selection, thresholds, diagnostics, and final
arbitration. The plugin carries Core's per-call `output_optimization` from PreTool into the matching
PostTool request, so RTK output is not compressed twice. The local CLI recovery command is a trusted
operator entry rather than Agent authorization, so the plugin applies only lossless candidates.

`tool_result_persist` is a synchronous OpenClaw transcript seam. It can replace a persisted string,
a structured value, or a single text block while preserving the surrounding Tool Result envelope;
media and multi-block results pass through. It does not replace a tool result already consumed by
the model in the same turn, and it does not cover non-OpenClaw transcript implementations.

Both operations use the single `tokenless compress` entry point and fail open if Tokenless is
missing or returns an invalid response.

### Configuration

The adapter requires OpenClaw Plugin API `2026.4.22` or newer; package metadata enforces this
minimum during installation on hosts that support compatibility checks.

Options in `openclaw.plugin.json`:

| Option | Default | Description |
|---|---|---|
| `rtk_enabled` | `true` | Enable RTK command rewriting |
| `post_tool_enabled` | `true` | Enable Protocol v2 PostTool handling of persisted tool results |
| `tool_ready_enabled` | `true` | Register the currently hard-disabled Tool Ready hook |
| `verbose` | `false` | Log lifecycle rewrites and applied PostTool results |

The previous response, TOON, skip-tool, and shell-tool configuration keys are removed; Core now
owns those decisions.

## Hermes Agent Plugin

The plugin registers hooks at three Hermes events while Core owns the lifecycle policy:

| Strategy | Event | Action | Status |
|---|---|---|---|
| Tool Ready | `pre_tool_call` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| Command rewriting | `pre_tool_call` | Sends the command to Core, then blocks and suggests the returned RTK form | ✅ Active |
| PostTool optimization | `transform_tool_result` | Sends the final model-bound result to Core and applies accepted output | ✅ Active |
| Session tracking | `on_session_start` | Propagates agent/session IDs for stats recording | ✅ Active |
| Schema compression | — | Hermes exposes no schema-transform seam | — |
| Marker recovery | `transform_tool_result` | Labels a successful standalone `tokenless retrieve` shell result for the Core bypass | ✅ Active |

**How command rewriting works in Hermes**: to remain compatible with Hermes releases that only
support blocking, the plugin asks Core for a rewrite, blocks the original shell command, and tells
the agent to retry with the returned command. The retry adds one tool-call round-trip. The final
hook recognizes Core's attributed RTK wrapper from the command Hermes actually executed, so RTK
output bypasses a second compression pass without correlating two different tool-call IDs.

When compression omits recoverable data, the Marker tells Hermes to run
`tokenless retrieve` through its existing shell tool. The adapter checks the
actual executed command; a successful standalone retrieve is returned unchanged
and never compressed again. If the Tokenless operation is unavailable or fails,
the hook leaves the host value unchanged.

### Install

```bash
make hermes-install
```

Enable the plugin:

```bash
hermes plugins enable tokenless
```

Or add to `~/.hermes/config.yaml`:

```yaml
plugins:
  enabled:
    - tokenless
```

## Qoder CLI Plugin

The plugin registers hooks at three Qoder events, covering three strategies:

| Strategy | Event | Action | Status |
|---|---|---|---|
| Tool Ready | `PreToolUse` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| Command rewriting | `PreToolUse` | Rewrites shell commands via RTK for token savings | ✅ Active |
| Response compression | `PostToolUse` | Compresses tool responses and encodes to TOON format | ✅ Active |

Each hook degrades gracefully — if the corresponding binary is not installed, that hook is silently skipped.

When a compressed result contains a Retrieve Marker, Qoder can execute its
command through the existing shell tool. The successful result bypasses response
compression so the recovered payload reaches the model unchanged.

### Install

```bash
make qoder-install
```

## Claude Code Plugin

The plugin registers hooks at two Claude Code events, covering four strategies:

| Strategy | Event | Action | Status |
|---|---|---|---|
| Tool Ready | `PreToolUse` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| Command rewriting | `PreToolUse` (Bash) | Rewrites shell commands via RTK for token savings | ✅ Active |
| Response compression | `PostToolUse` | Compresses tool responses and encodes to TOON format | ✅ Active |
| TOON encoding | `PostToolUse` | Pipeline step after response compression — encodes JSON to TOON format | ✅ Active |

Claude Code v2 requires plugins to be sourced from a registered marketplace. We expose the adapter's `claude-code/` directory as a single-plugin marketplace (`anolisa-tokenless`), then install `tokenless@anolisa-tokenless` from it. The marketplace name is component-scoped so multiple ANOLISA components can each register their own without colliding.

On Claude Code 2.1.121 or newer, a compressed result can direct the model to
run `tokenless retrieve` through Bash. The successful command result bypasses
compression and restores the complete payload.

### Install

```bash
make claude-code-install
```

## Codex Plugin

The plugin registers hooks at four Codex events, covering four strategies:

| Strategy | Event | Action | Status |
|---|---|---|---|
| Session check | `SessionStart` | Verifies tokenless CLI is installed and functional (non-blocking) | ✅ Active |
| Tool Ready | `PreToolUse` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| Command rewriting | `PreToolUse` | Rewrites shell commands via RTK for token savings | ✅ Active |
| Environment diagnostics | `PostToolUse` | Adds actionable context only for classified environment failures | ✅ Active |

> **Codex protocol constraint**: `PostToolUse` cannot replace or suppress the
> original tool output. Tokenless therefore does not append compressed content,
> which would make the model-visible payload larger. First-pass savings for
> supported shell commands come from RTK rewriting the command before execution.

### Install

```bash
make codex-install
```

## OpenCode Plugin

The local plugin uses OpenCode's mutable tool hooks, so compressed output
replaces the original model-visible response instead of being appended to it.

| Strategy | Event | Action | Status |
|---|---|---|---|
| Tool Ready | `tool.execute.before` | Registered silent pass-through; no check, repair, context, or block | ⛔ Hard-disabled |
| Command rewriting | `tool.execute.before` (bash) | Rewrites shell commands via RTK | ✅ Active |
| Response + TOON compression | `tool.execute.after` | Replaces structured tool output with a smaller representation | ✅ Active |
| Schema compression | `tool.definition` | Compresses tool descriptions and JSON Schemas | ✅ Active |

Enable the installed plugin through ANOLISA, then restart OpenCode:

```bash
anolisa adapter enable tokenless opencode
```

The driver creates `plugins/tokenless.js` under `OPENCODE_CONFIG_DIR`,
`XDG_CONFIG_HOME/opencode`, or `~/.config/opencode`, in that order. It ignores
`TOKENLESS_OPENCODE_CONFIG_DIR`; use `OPENCODE_CONFIG_DIR` for a shared custom directory.
Enable adopts an existing link to the same source; `anolisa adapter disable tokenless opencode`
removes that link. Restart OpenCode after either operation.

For standalone source installs, use:

```bash
make opencode-install
```

The bundled lifecycle scripts additionally honor `TOKENLESS_OPENCODE_CONFIG_DIR` as the
highest-priority override. Both methods refuse conflicting files or links. To return to standalone
management after ANOLISA disable, rerun `make opencode-install` or the bundled `scripts/install.sh`.
See [framework integration](../../docs/user-guide/en/token-saving/tokenless/framework-integration.md#opencode)
for the complete lifecycle.

If a response contains a Retrieve Marker, OpenCode can run the embedded
`tokenless retrieve` command through its existing shell tool. The adapter sends
the successful recovery result through the Core bypass without recompressing it.

## QwenPaw Plugin

The QwenPaw adapter is a native QwenPaw plugin. Its `plugin.py` registers an
AgentScope middleware through `api.register_middleware` and a
`tokenless_retrieve` tool through `api.register_tool`, and calls the in-process
`anolisa_tokenless.TokenlessSdk` directly:

| Feature | Middleware hook | Behavior | Status |
|---|---|---|---|
| Schema compression | `on_model_call` | Compresses tool schemas and appends the retrieve tool | ✅ Active |
| Command rewriting | `on_acting` | Rewrites `execute_shell_command` input via RTK after QwenPaw's approval step | ✅ Active |
| Response + TOON compression | `on_acting` | Replaces text blocks of the tool result for QwenPaw's built-in tools; file readers and tools outside the built-in table pass through untouched | ✅ Active |
| Recovery | `tokenless_retrieve` tool | Restores omitted content from the hash in a visible recovery instruction | ✅ Active |

```bash
make qwenpaw-install
```

The installer runs `qwenpaw plugin install <bundle> --force`; QwenPaw copies the
bundle into `<working dir>/plugins/tokenless/` (`QWENPAW_WORKING_DIR`, else
`COPAW_WORKING_DIR`, else an existing `~/.copaw`, else `~/.qwenpaw`) and installs
the `anolisa_tokenless` wheel listed in `requirements.txt` from the matching
GitHub Release. Records are written under `<workspace>/.tokenless`.

Before the bundle is handed over, the installer probes that pinned wheel URL and
stops with an explanatory error when the asset answers `404`. Set
`ANOLISA_SKIP_WHEEL_PREFLIGHT=1` to skip the probe on offline or mirrored
networks; `ANOLISA_TOKENLESS_PROBE_TIMEOUT` bounds each probe in seconds
(default 15). See
[troubleshooting](../../docs/user-guide/en/token-saving/tokenless/troubleshooting.md#qwenpaw-install-reports-an-unavailable-sdk-wheel)
for the full reference.

## DeepSeek Harness Plugin

The native DSH bundle sends replaceable single-text tool results through
Tokenless PostTool Core on `tools/post-execute`. Core owns content detection,
JSON cleanup, TOON selection, acceptance, and environment-error diagnostics.
When a reduced response contains a Retrieve Marker, DSH can run the embedded
`tokenless retrieve` command through its existing shell tool. The adapter
classifies the successful standalone command from `exec.arguments.command` and
sends its output through the Core Retrieve bypass, so it remains unchanged.
Recoverable compression is enabled only when bare `tokenless` resolves on the
shell `PATH` to the same executable selected by `tokenlessBin` or
`TOKENLESS_BIN` for Core. An absolute plugin-only or different binary is not
sufficient because the Marker emits the bare command.
DSH removes inherited `TOKENLESS_*` variables from model shell commands, so the
adapter publishes the selected state directory and optional statistics/Stash
database overrides as managed DSH shell facts. Core uses the same paths. The
data directory defaults to `.tokenless` in the session workspace and contains
a self-ignoring `.gitignore`; set `TOKENLESS_DATA_DIR`, `TOKENLESS_STATS_DB`, or
`TOKENLESS_STASH_DB` before starting DSH to select other absolute paths that
DSH's shell sandbox can access.
Error guidance stays active when response compression is disabled.

Enable the bundle for every desired DSH profile in one command by repeating
`--profile`:

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

Each enable or re-enable treats the supplied profiles as the complete desired
set. It removes the bundle from profiles recorded by the prior receipt but
omitted from the new command, so always include every profile that should keep
Tokenless. Each name must match a profile passed to `dsh --profile <profile>`.
Configuration belongs in that profile's `cordis.patch.yml`; see the
[DeepSeek Harness integration reference](../../docs/user-guide/en/token-saving/tokenless/framework-integration.md#deepseek-harness-native-processing)
for every option and default.

## AgentScope Framework Integration

AgentScope 1.0.11 through 1.0.x and AgentScope 2.0.x applications install two same-version Python
wheels explicitly.
The framework integration uses the `anolisa-tokenless` runtime directly and
does not start a CLI subprocess. Neither Python package is currently published
to a package index. Build and install both wheels from a source checkout:

```bash
make python-wheel agentscope-wheel
python -m pip install \
  target/wheels/anolisa_tokenless-*.whl \
  target/wheels/anolisa_tokenless_agentscope-*.whl
```

The public entry point and configuration are the same across both major
versions. AgentScope 1.x and 2.x expose different lifecycle hooks, so only the
final attachment step differs.

AgentScope 1.x uses a Tokenless Toolkit so tools registered before or after
Agent construction, including MCP tools, receive the same lifecycle handling.
Installation requires an explicit session identifier.

```python
from agentscope.agent import ReActAgent
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = integration.create_toolkit()
toolkit.register_tool_function(application_tool)
agent = ReActAgent(..., toolkit=toolkit)
integration.install(agent, session_id="conversation-id")
```

AgentScope 2.x receives the retrieval Tool and middleware during construction;
this works from 2.0.0 and does not depend on mutable Toolkit APIs added in later
patch versions.

```python
from agentscope.agent import Agent
from agentscope.tool import Toolkit
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
        # retrieve_tool_name="tenant_tokenless_retrieve",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = Toolkit(tools=[*application_tools, *integration.tools])

agent = Agent(
    ...,
    toolkit=toolkit,
    middlewares=integration.middlewares,
)
```

AgentScope App is supported from 2.0.3. It derives an isolated Tokenless data
directory for every user/agent/session below the configured absolute base
directory:

```python
from agentscope.app import create_app

app = create_app(..., **integration.app_options())
```

`app_options()` supplies one Middleware factory. AgentScope publishes that
Middleware instance's static Retrieve Tool through `list_tools()` and persists
Marker authorization in `AgentState.middle_context`.

Set a unique `retrieve_tool_name` in `TokenlessConfig` if the application
already defines `tokenless_retrieve`; App assembly does not expose the other
tools to this factory for a preflight collision check.

AgentScope 2.0.0 through 2.0.2 support direct Agent construction only; their App
APIs do not provide both Middleware-owned Tool publication and persisted
Middleware state. The existing `TokenlessMiddleware` 2.x API remains available
for compatibility; new code should use `TokenlessAgentScope`.

AgentScope supplies explicit contracts for its known shell, file, and API tools.
Register every custom tool with `ToolContract`: select `COMMAND_OUTPUT`,
`FILE_CONTENT`, or `API_RESPONSE`, and set `command_field` only for commands
that may be rewritten by RTK. Unknown custom tools fail during registration or
at the model boundary rather than guessing from output text. Compression
thresholds, TOON selection, diagnostics, and retrieval authorization remain in
Rust Core.

The read-only retrieval Tool has a static declaration and remains in the model
tool list across calls, avoiding tool-list churn when Marker visibility changes.
It accepts only a hash from the exact Marker set retained for the current model
call. Pass a different absolute `data_dir`
to each user or tenant for direct Agents;
`TOKENLESS_DATA_DIR` is only a process-wide fallback when `data_dir` is omitted.
Retain the default one-hour stash TTL unless the application has a deliberate
lifecycle policy, and do not expect retrieval across nodes.

Both AgentScope adapters enable schema compression, RTK command rewriting,
response compression, TOON, retrieval, environment-error guidance, and
per-call attribution. The native wheel contains RTK and links TOON directly;
it does not search for system executables. Host objects and streaming chunks
remain unchanged; only copied call arguments and final model-visible text are
transformed. Tool Ready remains hard-disabled.


## Build

For installed-package checks and optional real Agent tasks, see the
[release regression suite](tests/release_regression/README.md). It reports
tool-output savings and retrieval overhead separately.

| Target | Description |
|---|---|
| `make build` | Build `tokenless` + `rtk` (release mode) |
| `make build-tokenless` | Build `tokenless` + `rtk` (via justfile) |
| `make python-wheel` | Build the native `anolisa-tokenless` wheel |
| `make agentscope-wheel` | Build the pure-Python AgentScope integration wheel |
| `make test-python-runtime` | Install and test the wheel in an isolated environment |
| `make test-agentscope-integration` | Test both wheels with supported AgentScope versions |
| `make install` | Build and install binaries to `BIN_DIR` (default: ~/.local/bin) |
| `make test` | Run all tests (Rust + hooks) |
| `make test-hooks` | Run hook integration tests against the installed `tokenless` binary, whose version must match this checkout (`TOKENLESS_ALLOW_VERSION_SKEW=1` overrides) |
| `make lint` | Run clippy checks |
| `make fmt` | Format code |
| `make clean` | Clean build artifacts |
| `make package-raw` | Package prebuilt target binaries as an ANOLISA raw archive |
| `make adapter-install` | Install all available framework adapters |
| `make adapter-uninstall` | Remove all adapters |
| `make cosh-extension-install` | Install Copilot Shell extension |
| `make cosh-extension-uninstall` | Remove Copilot Shell extension |
| `make openclaw-install` | Install OpenClaw plugin |
| `make openclaw-uninstall` | Remove OpenClaw plugin |
| `make hermes-install` | Install Hermes Agent plugin |
| `make hermes-uninstall` | Remove Hermes Agent plugin |
| `make qoder-install` | Install Qoder CLI plugin |
| `make qoder-uninstall` | Remove Qoder CLI plugin |
| `make claude-code-install` | Install Claude Code plugin |
| `make claude-code-uninstall` | Remove Claude Code plugin |
| `make codex-install` | Install Codex plugin |
| `make codex-uninstall` | Remove Codex plugin |
| `make opencode-install` | Install OpenCode local plugin |
| `make opencode-uninstall` | Remove OpenCode local plugin |
| `make qwenpaw-install` | Install QwenPaw plugin via the qwenpaw CLI |
| `make qwenpaw-uninstall` | Remove QwenPaw plugin |
| `make setup` | Full setup: build + install + all adapters |

Override install paths:

```bash
make install BIN_DIR=/usr/local/bin
```

## Raw Packaging

Raw packaging accepts already-built `tokenless` and `rtk`
executables in one directory and applies the stable component payload layout:

```bash
make package-raw \
  BIN_DIR="$PWD/target/release-bins" \
  TARGET_OS=linux \
  TARGET_ARCH=aarch64 \
  OUTPUT_DIR="$PWD/dist"
```

Supported raw targets are `linux-x86_64`, `linux-aarch64`, and
`macos-aarch64`. `darwin`/`arm64` and `amd64`/`x64` are accepted as input
aliases, while artifact names always use the canonical ANOLISA labels. The
packer verifies the ELF or Mach-O architecture without executing cross-target
binaries, embeds the component-owned `.anolisa/component.toml`, materializes
adapter hook symlinks, and emits a reproducible
`tokenless-<version>-<os>-<arch>.tar.gz` archive. Set `SOURCE_DATE_EPOCH` when
the caller needs an epoch other than the source commit time.

npm packaging also accepts prebuilt `linux-x64`, `linux-arm64`, `darwin-x64`,
and `darwin-arm64` binary directories under `target/npm-prebuilt`. The packer
validates and assembles them:

```bash
node npm/scripts/package-npm.js --all
```

See [npm/README.md](npm/README.md#packaging-for-npm) for the fixed directory
layout and single-target interface.

## Project Structure

| Path | Description |
|---|---|
| `crates/tokenless-cli/` | CLI binary — `tokenless` command (compress, stats, env-check) |
| `crates/tokenless-schema/` | BeforeModel tool-schema compression — `SchemaCompressor` |
| `crates/tokenless-compressors/` | Content-domain engines — `JsonCompressor`, `TabularCompressor` and `BuildLogCompressor` are connected to PostTool |
| `crates/tokenless-runtime/` | Lifecycle API and Runtime-owned `PostToolPipeline`, shared by CLI and language bindings |
| `crates/tokenless-protocol/` | Versioned adapter contract and shared `heuristic-v1` token estimator |
| `python/tokenless/` | PyO3 package exposing `anolisa_tokenless` for CPython 3.11+ |
| `python/agentscope/` | Independent AgentScope framework integration and wheel metadata |
| `adapters/tokenless/` | FHS adapter bundle — manifest, env-check spec/fix, hooks, OpenClaw plugin |
| `adapters/tokenless/hermes/` | Hermes Agent adapter — plugin + detect/install/uninstall scripts |
| `adapters/tokenless/qoder/` | Qoder CLI adapter — plugin + detect/install/uninstall scripts |
| `adapters/tokenless/claude-code/` | Claude Code adapter — marketplace + plugin + hooks dispatcher |
| `adapters/tokenless/codex/` | Codex adapter — plugin + Python hook scripts |
| `adapters/tokenless/opencode/` | OpenCode adapter — local JavaScript plugin + lifecycle scripts |
| `adapters/tokenless/qwenpaw/` | QwenPaw adapter — plugin manifest, AgentScope middleware, wheel requirements + lifecycle scripts |
| `third_party/rtk/` | RTK vendored source — command rewriting engine (justfile clone+patch) |
| `third_party/patches/` | Patches for vendored third_party sources |
| `packaging/raw/` | Component-owned ANOLISA raw packer and target validation |
| `Makefile` | Unified build system for the entire workspace |

## Prerequisites

- **Rust** toolchain >= 1.89 — required by rtk (edition 2024) and toon-format (is_multiple_of). Install via [rustup](https://rustup.rs)
- **just** — build runner for rtk setup (clone + patch orchestration)
- **Git** — for rtk source download via justfile
- **CPython 3.11+ development environment and uv** — only for the Python wheel
  and commands that explicitly include all workspace members

## License

Apache License 2.0 — see [LICENSE](LICENSE).
