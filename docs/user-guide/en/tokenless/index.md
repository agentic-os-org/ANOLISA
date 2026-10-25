# Tokenless User Manual

[中文版](../../zh/tokenless/index.md)

Tokenless is designed for tool-heavy AI agents. Its CLI compacts schemas and tool responses, while its adapters can also rewrite shell commands, check tool dependencies, and pass compressed results to an agent. The exact effect depends on the host framework: some adapters replace the original result, while others add compressed context without removing the original.

Start with the [Quick Start](index.md) if this is your first use.

## Quick start

Install Tokenless, connect it to Claude Code, run one real task, and verify a
before/after Token record in about three minutes. Tokenless works in the
background, so your prompts and normal Agent workflow do not change.

Savings vary by workload. Tool-heavy tasks usually show the clearest result;
short or conversation-only tasks may show little change.

### 1. Install Tokenless and connect Claude Code {#install-tokenless}

Choose an installation method based on your use case:

| Method | Use case | Description |
|--------|----------|-------------|
| [anolisa CLI](#method-a-anolisa-cli-recommended) | Full ANOLISA component management | Unified management of all components and adapters |
| [RPM](#rpm-alinux) | Alinux with the YUM repository configured | Managed package through `yum`, then adopted into anolisa system state |
| [npm](#method-b-npm) | Standalone CLI and adapter install | Prebuilt binaries + adapter resources for developers |
| [curl](#method-c-curl-standalone-install) | One-liner on Linux or macOS | Uses npm when available (needs Node.js 16.7+), otherwise builds from source (needs a Rust toolchain) |
| [Skill](#method-d-skill-for-agents) | Agent-driven install | Skill-based installation for agent frameworks |

#### Method A: anolisa CLI (recommended)

This quick path uses Claude Code as the example Agent:

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

If the anolisa CLI is already installed, start with `anolisa install
tokenless`. The PATH line is needed only when a fresh installation reports
that `~/.local/bin` is not available in the current Shell.

Using another Agent? Follow the matching setup path in
[Use another Agent](#use-another-agent); the remaining steps stay the same.
The Agents listed below use `anolisa adapter enable`, including OpenCode.
OpenCode also provides a bundled lifecycle script for npm or manual installations.

#### RPM (Alinux) {#rpm-alinux}

Alinux users with the YUM repository configured may install the managed RPM
instead of running the CLI installer:

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

`adopt` records the directly installed RPM in system state, so the adapter
commands can use its component contract. Upgrades and removal go through `yum`;
see [Troubleshooting · YUM/RPM installation](troubleshooting.md#yumrpm-installation).

The methods below install the CLI on their own and create no anolisa component
record, so `anolisa adapter enable` does not apply to them.

#### Method B: npm

Requires Node.js 16.7+ — that is the release `fs.cpSync` arrived in, and the package postinstall uses it to place the adapter resources. Automatically installs the prebuilt binaries (`tokenless`, `rtk`) and the framework adapter resources for your platform:

```bash
npm install -g anolisa-tokenless
tokenless --version
```

After installation, the adapter resources are located at `~/.local/share/anolisa/adapters/tokenless/`. An npm install creates no anolisa component record, so `anolisa adapter enable` does not apply to it — enable adapters as described in [Enable the adapter for your install method](#enable-the-adapter-for-your-install-method).

That directory is shared with the anolisa CLI. When it already belongs to a managed component install, the package postinstall keeps it unchanged and says so instead of replacing resources a component record still refers to; pass `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1` to take it over anyway. This protection ships in the first npm release built from the change described on this page: published packages up to and including `0.8.2` predate it and replace that directory unconditionally, so back up a managed adapter tree before installing one of them (check with `npm view anolisa-tokenless version`).

The postinstall also enables the `claude-code` adapter: it runs that adapter's own `install.sh`, which registers the plugin with the Claude CLI when one is reachable (`CLAUDE_BIN` selects which). With no Claude CLI it skips registration and says so rather than failing the install. That registration lives in Claude's own configuration — outside the npm prefix and outside the adapter directory — so removing the package does not by itself remove it.

**Uninstalling a direct npm install (Method B)** has its own entry point, and it is *not* `scripts/uninstall.sh`. Deregister first, then remove the package:

```bash
# 1. every adapter you enabled, e.g. claude-code
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/uninstall.sh
# 2. then the package itself
npm uninstall -g anolisa-tokenless
```

`scripts/uninstall.sh` is the **curl installer's** uninstaller (Method C). It is driven by the receipt at `~/.local/share/tokenless/install-receipt`, which only that installer writes, so on a direct npm install it stops immediately with `No install receipt found` and changes nothing. Use it only for a curl install.

Release package targets:

| Platform | Architecture | npm package |
|----------|-------------|-------------|
| Linux (glibc) | x86_64 | `@anolisa/tokenless-linux-x64` |
| Linux (glibc) | aarch64 | `@anolisa/tokenless-linux-arm64` |
| macOS | x86_64 (Intel) | `@anolisa/tokenless-darwin-x64` — declared build target, **not published yet** |
| macOS | aarch64 (Apple Silicon) | `@anolisa/tokenless-darwin-arm64` |
| Windows | x86_64 | `@anolisa/tokenless-win32-x64` — declared build target, **not published yet** |

Tokenless 0.9.0 builds packages for all five targets. At release preparation, the npm registry's latest root package is still 0.8.8, and the Intel Mac and Windows platform packages are not published. Generating release assets does not publish them to npm: npm installation requires the root package and the matching platform package at the same version. Check availability with `npm view anolisa-tokenless version` and `npm view @anolisa/tokenless-darwin-x64 version` or `npm view @anolisa/tokenless-win32-x64 version` before installing on these new targets. See [Platform support](#platform-support) for the currently available installation routes.

#### Method C: curl standalone install

A one-liner install script that prefers npm and builds from source instead. The choice between the two is made **before** npm runs — when npm is missing, the platform is musl Linux, or you set `TOKENLESS_FORCE_BUILD=1`. Once `npm install` has been invoked the installer does **not** switch method automatically: npm's exit status cannot prove that its postinstall left no framework registration behind, so a failure at that stage is reported as an incomplete installation — keeping and naming whatever could not be rolled back — rather than as a successful source build. Re-run it, or build from source explicitly with `TOKENLESS_FORCE_BUILD=1`:

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | bash
```

Prerequisites depend on which path the script takes:

| Path | Taken when | Requires | Installs |
|------|-----------|----------|----------|
| npm | npm is present and the platform is glibc Linux or macOS | `curl`, `tar`, Node.js 16.7+ with `npm` | `tokenless`, `rtk`, and the adapter resources |
| Source build (Linux only) | chosen **before** npm runs: npm is missing, the platform is musl Linux, or `TOKENLESS_FORCE_BUILD=1`. Not selected automatically once `npm install` has been invoked — see the failure note above | `curl`, `tar`, a Rust toolchain (`cargo`) | the `tokenless` CLI only — no `rtk` and no adapters |

The script supports Linux and macOS only; on Windows it exits with an error, so use WSL2 there. Its source-build path is Linux-only as well: on macOS the installer either takes the npm path or exits with an error, and never invokes `cargo`.

Pin a version or set a custom install directory. Pass the variables to `bash`, not to `curl`:

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | TOKENLESS_VERSION=0.7.4 bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/install.sh | TOKENLESS_INSTALL_DIR=/usr/local/bin bash
```

A pinned version is a hard pin: the source build downloads only the matching `tokenless/v<VERSION>` tag. If that tag does not exist the installer fails instead of silently building `main`.

The installer records what it created in `~/.local/share/tokenless/install-receipt`. To remove exactly those paths later:

```bash
curl -fsSL https://raw.githubusercontent.com/agentic-os-org/ANOLISA/main/src/tokenless/scripts/uninstall.sh | bash
```

#### Method D: Skill (for agents)

When an agent framework (cosh-ng, OpenClaw, Hermes, etc.) needs to install and manage Tokenless on its own, use the Skill method.

The Skill file is at `src/os-skills/ai/install-tokenless/SKILL.md` in the repository. An agent that loads this file can complete installation and configuration automatically.

It is declared in the `os-skills` component manifest (`src/os-skills/component.toml`), so the next `os-skills` release ships it to `/usr/share/anolisa/skills/install-tokenless/` and `anolisa adapter enable os-skills openclaw` (or the `hermes` adapter) deploys it into that framework's skill directory. The distribution contract for the *currently published* `os-skills` artifact does not list it yet, and deliberately so: that contract is pinned by `index.toml` to one immutable artifact with a fixed sha256, so declaring a skill the artifact does not contain would make the bundle copy fail at enable time. It gains the skill together with the next version bump. Until then, load the Skill from its repository path.

To use it, point your agent framework at the Skill file path, or pass its contents directly to the agent. The Skill contains complete installation, verification, and framework integration guidance.

After installing Tokenless, enable the adapter for the target agent framework. The Skill guides this step automatically and follows whichever method it used, so apply the matching row of [Enable the adapter for your install method](#enable-the-adapter-for-your-install-method).

#### Enable the adapter for your install method {#enable-the-adapter-for-your-install-method}

Installation only places files on disk; it does not register Tokenless with an agent. How you enable it depends on how you installed it:

| Install method | Adapter resources | How to enable |
|----------------|-------------------|---------------|
| anolisa CLI (Method A) | installed with the component | `anolisa adapter scan`, then `anolisa adapter enable tokenless <framework>` |
| npm (Method B), or curl (Method C) through its npm path | copied by the package postinstall to `~/.local/share/anolisa/adapters/tokenless/` | run the framework's bundled script, for example `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh`. `anolisa adapter enable` is unavailable here because an npm install creates no anolisa component record |
| curl (Method C) through its source-build path | none | not applicable — this is a CLI-only install. Use the `tokenless` subcommands directly, or reinstall through Method A or B for agent integration |
| Skill (Method D) | whichever method the Skill ran | follow that method's row |

Restart the agent CLI, IDE, or gateway after enabling.

### 2. Run one real task

Restart Claude Code so it loads the adapter, then start a new session and run a
tool-heavy task. For example:

> Run the full test suite for this repository and summarize only the failures.

You do not need to mention Tokenless in the prompt.

### 3. Verify the saving

After Claude Code uses a Shell, API, or another supported tool, run:

```bash
tokenless stats list --limit 5
tokenless stats summary
```

Example output (values vary by workload):

```text
Showing 1 record(s):
================================================================================
[ID:42] 2026-08-12 10:20:30 | claude-code | Session:- | Tool:- | Chars:5120→2880(-2240) | Tokens:1280→720(-44%)

Tokenless Statistics Summary
============================================================
Total Records: 1

Character Savings:
  Before: 5120 chars
  After:  2880 chars
  Saved:  2240 chars (43.8%)

Token Savings:
  Before: 1280 tokens
  After:  720 tokens
  Saved:  560 tokens (43.8%)

Breakdown by Operation:
----------------------------------------
  compress-response: 1 records
    Chars: 5120 -> 2880 (-43.8%)
    Tokens: 1280 -> 720 (-43.8%)
```

You are done when `stats list` contains a record whose estimated Token count
decreases from before to after. To inspect exactly what changed, copy its ID:

```bash
tokenless stats diff <record-id>
```

For a visual view of savings over time, follow the
[AgentSight guide](../agentsight/integrations.md#tokenless-token-savings).
When Tokenless and AgentSight run as the same user, the Dashboard reads local
Tokenless statistics without requiring SLS.

If no record appears, the content may not have passed through Tokenless or may
not have become shorter. Check the adapter and component health:

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

Then see
[No statistics appear after setup](troubleshooting.md#no-statistics-appear-after-enabling-the-adapter).

Token counts are estimates for content processed by Tokenless, not a direct
measurement of the model bill. Statistics and diffs may contain original tool
content; avoid sharing their output when it contains sensitive data. See
[Measuring savings](measuring-savings.md) and
[Configuration and data privacy](configuration-and-privacy.md) for details.

### Use another Agent

Scan the machine, then enable only the Agent you use:

```bash
anolisa adapter scan
```

| Agent | Setup |
|-------|-------|
| cosh-ng | `anolisa adapter enable tokenless cosh` |
| OpenClaw | `anolisa adapter enable tokenless openclaw` |
| Hermes | `anolisa adapter enable tokenless hermes` |
| Qoder | `anolisa adapter enable tokenless qoder` |
| Claude Code | `anolisa adapter enable tokenless claude-code` |
| Codex | `anolisa adapter enable tokenless codex` |
| DeepSeek Harness (dsh) | `anolisa adapter enable tokenless dsh --profile <profile>` |
| OpenCode | `anolisa adapter enable tokenless opencode` |
| Qwen Code | `anolisa adapter enable tokenless qwencode` |
| QwenPaw | `anolisa adapter enable tokenless qwenpaw` |

Restart the Agent CLI or IDE after setting it up. OpenClaw also requires
`openclaw gateway restart`; if its security check rejects the plugin, follow
the [OpenClaw integration instructions](framework-integration.md#2-enable-one-adapter).
For DeepSeek Harness, `<profile>` is required and must match the name used by
`dsh --profile <profile>`; restart that profile after enabling the bundle.
To enable more than one profile, repeat `--profile` in the same command:

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

Every later enable or re-enable replaces the entire recorded profile set.
Include every profile that should retain Tokenless each time.

OpenCode is available through the same adapter command. The bundled lifecycle script remains an
alternative for npm installations that have no ANOLISA component record.

### Optional: test compression without an Agent

Use this deterministic check when you want to confirm the standalone CLI
before enabling an adapter:

```bash
tokenless compress-response <<'JSON'
{
  "status": "ok",
  "data": {
    "name": "demo",
    "items": [1, 2, 3]
  },
  "debug": {
    "trace": "verbose"
  },
  "metadata": null
}
JSON

tokenless stats list --limit 1
```

This example compacts formatting whitespace while retaining all fields and
values, including `debug`, `trace`, and `metadata: null`. Already compact input
that does not shrink is returned unchanged and is not recorded.

### Platform support

| Platform | anolisa CLI | npm | curl | Skill |
|----------|-------------|-----|------|-------|
| Linux x86_64/aarch64 (glibc) | Supported | Supported | Supported (npm path) | Supported (follows curl) |
| Linux with musl, such as Alpine | Not currently supported | Not currently supported | Source build only, needs a Rust toolchain | Source build only, needs a Rust toolchain |
| macOS Apple Silicon | Supported | Supported | Supported (npm path) | Supported (follows curl) |
| macOS x86_64 | Not currently supported | Pending registry publication | Pending registry publication | Pending registry publication |
| Windows | Not currently supported | x64: pending registry publication | Not supported, use WSL2 | Not supported, use WSL2 |

Notes on the boundaries above:

- macOS x86_64 is a supported 0.9.0 build target. Its npm and curl npm routes require `@anolisa/tokenless-darwin-x64` to be published alongside the matching root package. Until then, those routes cannot deliver a binary there. The curl source-build fallback is refused on macOS — `scripts/install.sh` exits with an error instead of running `cargo`.
- curl on macOS relies on its npm path. Its source-build fallback is validated on Linux only, and the installer refuses to run it on macOS, so a macOS machine without npm has no supported curl path.
- The 0.9.0 npm manifest declares `os: win32, linux, darwin` and includes a Windows x64 package; Windows ARM64 is unsupported. Native Windows npm installation awaits registry publication. The curl installer rejects Windows in `scripts/install.sh`; run that installer inside WSL2, where the Linux rows apply. The bundled installation Skill also documents a Linux/macOS Bash workflow.
- The Skill method delegates to the anolisa CLI, npm, or curl, so its support follows the method it selects.

To build the standalone CLI from source, see [User manual · Build the standalone CLI from source](index.md#build-the-standalone-cli-from-source).

### Next steps

- [Python SDK](../../../developer-guide/en/sdk/tokenless/index.md): framework-neutral and AgentScope layers with runnable examples
- [AgentScope SDK integration](../../../developer-guide/en/sdk/tokenless/agentscope.md): AgentScope 1.x, 2.x, and App attachment
- [Agent integration](framework-integration.md): product adapter activation
- [Capabilities and boundaries](#capabilities-and-boundaries): runtime behavior and limits
- [CLI reference](cli-reference.md): all subcommands and options
- [Measuring savings](measuring-savings.md): statistics, dual runs, and AgentSight/SLS
- [Configuration and data privacy](configuration-and-privacy.md): toggles, storage, and sensitive data
- [Troubleshooting](troubleshooting.md): common errors, upgrades, and uninstall

## Build the standalone CLI from source

Source builds are intended for development and debugging. The project currently validates and supports source builds on Linux only:

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa/src/tokenless
cargo build --release --locked -p tokenless-cli
./target/release/tokenless --version
```

This path produces only the standalone `tokenless` CLI. It does not install `rtk` or the agent integration resources. To use the complete feature set in an agent, install through the anolisa CLI, npm, curl, or Skill method as described in the [Quick Start](index.md).

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
the [Python SDK guide](../../../developer-guide/en/sdk/tokenless/index.md) for both layers, runnable examples, and configuration.

## Capabilities and boundaries

| Capability | Behavior implemented in the current code | Important boundary |
|------------|------------------------------------------|--------------------|
| Schema compression | Removes `title` and `examples`, removes fenced and inline code from descriptions, collapses whitespace, and truncates descriptions | Common BeforeModel passes lossy transformations through without marker-authorized recovery; OpenCode's per-tool path and the direct CLI still compress (Qwen Code skips the declared event) |
| Content-aware response compression | Successful PostTool JSON is routed to `JsonCompressor`; recognized successful build/test command output is routed to `BuildLogCompressor`; CSV/TSV is routed to `TabularCompressor`; supported search listings use `SearchResultsCompressor`; only a smaller end-to-end result is accepted | Other content domains and Tool Errors pass through; recoverable reduction requires either Marker-authorized framework retrieval or a supported Marker command path |
| Search path sharing | Shares consecutive paths in numbered API search records, including native Claude Grep, or a directory prefix in pure path lists from commands and APIs; retains every received record | Enabled by default; requires text replacement and a smaller byte-reversible view; file reads skip the path-list strategy |
| TOON encoding | Encodes JSON and keeps the JSON input when the estimated token count does not decrease | Replaces the original when the host accepts text replacement; hosts without replacement capability pass through |
| Command rewriting | Calls `rtk rewrite` and submits the rewritten shell input when a rule is available | Recognized build/test commands and direct or built-in-wrapper `find` invocations keep the entire command native; disabling search path sharing does not restore rewriting for those calls; custom RTK wrapper prefixes are outside this protection |
| Tool Ready | Legacy pre-call checks for declared binaries, versions, configuration, permissions, and optional dependencies | Hard-disabled; it cannot inspect, repair, or block tool execution |
| Stash | Stores content removed by string, array, depth, or schema-description truncation, complete arrays behind record reduction, omitted Build Log progress intervals | One-hour TTL and 10,000 live entries by default; JSON field names and empty values alone do not trigger Stash writes |

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

Whichever method you used — anolisa CLI, npm, curl, or Skill — installation only puts the component and its adapter resources on disk. It never registers Tokenless with an agent. The enable step differs per install method, because the three production paths leave different things behind:

| Install method | Adapter resources | Enable step |
|----------------|-------------------|-------------|
| anolisa CLI | installed with the component, plus an anolisa component record | `anolisa adapter enable tokenless <framework>` |
| npm, or curl through its npm path | copied by the package postinstall to `~/.local/share/anolisa/adapters/tokenless/`; no anolisa component record exists | run the framework's bundled script, for example `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh`. `anolisa adapter enable` cannot be used here |
| curl through its source-build path | none — the build installs the `tokenless` CLI only | not applicable. This is a CLI-only install; use the `tokenless` subcommands directly, or reinstall through the anolisa CLI or npm for agent integration |
| Skill | whichever of the above the Skill ran | follow that method's row |

CLI-only use does not require an adapter.

Disabling follows the same split: `anolisa adapter disable tokenless <framework>` applies to anolisa CLI installs, while an npm-based install is disabled through the framework's own uninstall script or by removing the hook registration the install script created.

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
2. File-content tools such as Read/Glob/LSP/NotebookRead and their aliases retain their content. Native Claude Code `Grep` results in no-context content mode can use lossless search path sharing. Non-file command and API results can also share pure path lists by content; this does not expand file-read eligibility (see [Controlling search path sharing](#controlling-search-path-sharing)).
3. The response reaches the minimum length. Core skips responses shorter than 200 characters on the shared response hook, OpenClaw, and Hermes paths. Length is counted in characters, not bytes.
4. The content matches a supported domain. Threshold-based response compression works on JSON objects and arrays; plain text is compressed only by a matching text compressor, and which compressors can fire depends on the path:
   - **4a. Shared response hook path:** output that arrives as plain text (not JSON) is routed to the content-aware text compressors (build/test log terminal cleanup and progress reduction, CSV/TSV table compaction, search path sharing for API records and non-file command/API path lists, HTML page rendering, and opt-in Git diff context cropping) described in [Adapter processing rules](framework-integration.md#adapter-processing-rules); the table rules are detailed in [CSV/TSV compaction preserves complete tables](#csvtsv-compaction-preserves-complete-tables) and the search rules in [Controlling search path sharing](#controlling-search-path-sharing). For shell tools, the hook first unwraps the envelope's dominant text field (`stdout` or `stderr`, at least 2,000 characters; a Bash `stdout` that starts with `diff --git` is unwrapped even below that minimum) into the text slot and later re-injects the compressed text into a same-shaped envelope.
   - **4b. OpenClaw:** a plain string, or a `toolResult` message whose content is exactly one valid text block, takes the replaceable text path. Any other `toolResult` — multiple text blocks, image blocks, or empty/invalid content — is skipped as-is: the plugin returns before calling Core, so such results are neither compressed nor recorded in statistics. Non-`toolResult` objects and arrays, including a shell envelope such as `{"stdout": ...}`, are passed to Core whole as structured JSON with text replacement disabled, so the envelope keeps its top-level shape and only JSON-domain compression applies.
   - **4c. Hermes:** for shell tools, Hermes unwraps the envelope's `output` field, sends that text to Core with replacement allowed, and restores the compressed text into the same envelope; other tools' results go through directly.

   The shared response hook additionally skips skill-like text with YAML frontmatter before spawning a compression subprocess (Core passes such text through anyway).
5. The compressed result is strictly smaller. When neither response compression nor TOON encoding makes the content smaller, the original text is kept.

After these checks, truncation strength depends on the tool category. Categories and thresholds are defined in `tool_categories.json` inside the adapter directory (the single source of truth shared by all adapters); built-in safe fallbacks are used when the file is missing or invalid:

| Category | Representative tools | String truncation threshold | Array truncation threshold | Maximum nesting depth |
|----------|----------------------|------------------------------|----------------|-----------------------|
| Content retrieval | Read, Glob, Grep, LSP, NotebookRead and aliases | Compression skipped | — | — |
| Shell/exec | Bash, Shell, exec, terminal, etc. | 65,536 characters | 128 items | 8 |
| Other structured tools | Any tool not in the two categories above | 1,048,576 characters | 65,536 items | 32 |

Threshold semantics: a string longer than the threshold is cut at the threshold (retrievable through Stash when Stash is enabled). An array is truncated only when it is longer than the category threshold plus the tail window: the leading items up to the threshold and the last 8 items (the default tail window) stay inline, the dropped middle segment is retrievable through Stash when enabled, and a marker separates the two windows. Arrays of at least 33 JSON objects ignore these thresholds and go through record reduction instead: a base budget of 32 selected records (the first 4 and the last 4, records carrying error or anomaly signals, numeric outliers, and a stable sample of the rest) plus a retrieval marker, with the complete original array written to Stash; record reduction requires Stash — without it, every record is kept. Subtrees nested deeper than the depth cap collapse into a truncation marker. See the [CLI reference](cli-reference.md) for the full rules and flags.

Per-path differences worth noting:

- Running `tokenless compress-response` standalone uses the CLI's own defaults (4,096-character strings, a 32-item head window plus an 8-item tail window, depth 8), overridable with `--truncate-strings-at`, `--truncate-arrays-at`, `--array-tail-preserve`, and `--max-depth`; see the [CLI reference](cli-reference.md).
- Codex and Qwen Code do not run response compression or TOON because their current PostToolUse contracts cannot replace the original model-visible output: Codex keeps the original and adds context only for classified environment failures, while Qwen Code passes through. See the adapter table below for what each integration provides.
- The OpenClaw plugin reads the same `tool_categories.json` lists to map each tool to a content origin (file content, command output, or API response), falling back to its built-in lists when that file is missing or invalid; Core then applies the matching thresholds. Its former `skip_tools` and `shell_tools` overrides have been removed and no longer control the adapter. See [Configuration and data privacy](configuration-and-privacy.md) for the current options.
- TOON encoding is a separate trigger decision: it runs only on payloads of at least 500 characters and only when the host slot accepts text, and it is adopted only when the encoded result is smaller than the current content.
- Git diff context cropping is a separate opt-in decision, disabled by default: with `TOKENLESS_DIFF_COMPRESSION_ENABLED=1` in the agent process environment (or the SDK's `diff_compression_enabled` option), Core crops unchanged context from command-output Git diffs when the slot accepts text; every changed line is preserved, the complete original output is stashed behind a recovery hint, and a candidate is rejected unless it saves at least 16 estimated tokens net of that wrapper text.
- The Python SDK and AgentScope layers do not set these thresholds through Python configuration: compression thresholds, content detection, and TOON selection are Core behavior. Direct `TokenlessRuntime.compress_response` calls can still override the truncation limits per call. See the [Python SDK](../../../developer-guide/en/sdk/tokenless/index.md) and [AgentScope integration](../../../developer-guide/en/sdk/tokenless/agentscope.md) docs.

### Controlling search path sharing

Search path sharing is enabled by default for numbered API search records and pure path lists from commands or APIs. Set `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0`
in the agent process environment to disable it through the CLI. When unset it stays enabled;
`1`, `true`, and `yes` also enable it (case-insensitively); an empty value counts as unset, and
any other value disables it.
This setting is independent of `config.json`. Python SDK callers can disable it with
`TokenlessConfig(search_path_sharing_enabled=False)`; Rust callers set
`RuntimeConfig.search_path_sharing_enabled` to `false`. All entry points default to enabled.

Disabling this feature returns search listings unchanged. JSON, table, and log compression
remain available for other tool names. The exact name `Grep` always excludes those compressors
to preserve received matches, even with path sharing disabled. A custom tool named `Grep`
therefore cannot restore its pre-feature JSON/table/log compression through this switch.
Supported no-context Claude Grep results retain all received matches. Numbered search records require
API origin; pure path lists also accept command output, including Bash without RTK. File content
and file reads skip the path-list strategy; existing file-read JSON, table, and log handling remains available.
Other non-file tool results can use the same Core capability regardless of tool name.

Pure lists need at least three POSIX paths with uniform LF or CRLF endings. The smaller view contains
a shared directory `base`, all `paths`, `line_ending`, and `trailing_newline` as compact JSON after a
fixed English header. Decode each path as `base` plus its entry, preserving order and duplicates;
join with `line_ending` and append it only when `trailing_newline` is true. No Stash or recovery call
is needed. Mixed output, blank lines, NUL separators, ambiguous formats, and candidates without net
byte and estimated-token savings pass through.

Direct native `find` and calls behind the [built-in RTK prefixes](cli-reference.md#shell-rewriting)
keep the entire command, including compound commands, outside RTK rewriting. Nesting and shell
backslash-newline continuations are supported; custom `hooks.transparent_prefixes` are not inspected
and may still allow wrapped `find` to be rewritten. Disabling path sharing retains the built-in protection so complete paths remain
available for subsequent reads. Mixed or unprofitable native output may therefore remain uncompressed.
Whole-task savings depend on the workload; smaller search results do not guarantee lower total token use.

### CSV/TSV compaction preserves complete tables

Successful CSV/TSV tool results can be compacted when the host can replace output with text.
File-origin results, failed tools, RTK-optimized output and Retrieve output pass through.
A supported table has a header and at least two equal-width data rows with an unambiguous
comma or tab delimiter. Malformed quoting, ambiguous delimiters, single-column text, Markdown
and fixed-width tables are not compressed by this compressor.

Compaction preserves every cell string and row/column order, including duplicate rows,
empty cells, duplicate headers, leading zeros and large numeric strings. It removes
unnecessary quoting and normalizes record separators to LF without changing line endings
inside cells. No final record separator is appended, so an existing trailing separator may
be removed even when that is the only formatting change. This preserves cells, not the
original bytes, and does not guarantee unchanged model reading accuracy.

The complete candidate is applied only when both character count and estimated token count
decrease; otherwise the original is returned unchanged. There is no percentage savings
threshold. The estimate does not guarantee savings with every model tokenizer.

Tables are never sampled or reduced, regardless of size or recovery availability. Table
compaction does not write Stash, emit an incomplete-table notice, or require Retrieve before
enumeration or calculations. Applied compaction reports `lossless` and `tabular_compaction`.
Historical table originals already in Stash remain retrievable under the existing
expiry and authorization rules; new table compactions do not store original formatting.

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

JSON response compression retains diagnostic-named fields, `null`, and empty values. Stash does not make every compression domain reversible: removed schema `title`/`examples` and Markdown formatting are not stored for retrieval. Validate critical payloads with representative data before enabling active compression.

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
| Install and verify for the first time | [Quick Start](index.md) |
| Install via npm, curl, or Skill | [Quick Start · Install Tokenless](index.md#install-tokenless) |
| Build the standalone CLI from source | [This page · Build the standalone CLI from source](#build-the-standalone-cli-from-source) |
| Use the in-process Python SDK | [Python SDK](../../../developer-guide/en/sdk/tokenless/index.md) |
| Integrate AgentScope | [AgentScope SDK integration](../../../developer-guide/en/sdk/tokenless/agentscope.md) |
| Connect an Agent product | [Agent integration](framework-integration.md) |
| Compress or retrieve manually | [CLI reference](cli-reference.md) |
| Understand when compression triggers and what the thresholds are | [This page · Compression trigger conditions and thresholds](#compression-trigger-conditions-and-thresholds) |
| Inspect savings or content changes, or run a dual comparison | [Measuring savings](measuring-savings.md) |
| Change settings or understand local data | [Configuration and data privacy](configuration-and-privacy.md) |
| Fix missing statistics, adapter, or Stash issues | [Troubleshooting](troubleshooting.md) |
| Diagnose missing schema-compression records | [Troubleshooting · Schema compression produces no statistics](troubleshooting.md#schema-compression-produces-no-statistics) |
| Upgrade or uninstall | [Troubleshooting · Upgrade and uninstall](troubleshooting.md#upgrade-and-uninstall) |

## Recommended rollout

1. Complete the [Quick Start](index.md) with non-sensitive test data.
2. Record a dry-run baseline for the same task.
3. Enable active compression and compare both output quality and savings.
4. Confirm that local-data and SLS behavior meets your requirements.
5. Enable the adapter for production agents.

The `tokenless --help` output from the installed version is the final authority for CLI and configuration behavior.
