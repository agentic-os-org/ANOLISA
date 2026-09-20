# Tokenless Agent Integration

[中文版](../../../zh/token-saving/tokenless/framework-integration.md)

Tokenless connects to Agent products through plugins, hooks, and extensions. This guide covers
product adapters. The Python SDK and its AgentScope-specific child document live under
[Python SDK](sdk.md).

## Agent adapter support matrix

| Agent product | Value | Tool Ready | Rewrite behavior | Response delivery | TOON | Schema |
|-----------|-------|------------|------------------|-------------------|------|--------|
| cosh | `cosh` | Hard-disabled | Replaces supported shell input | Cosh-NG replaces supported JSON results; legacy Copilot Shell passes through | Pipeline-selected for replaceable text | — (hook runs, tools returned unchanged) |
| OpenClaw | `openclaw` | Hard-disabled | Replaces the `exec` command input | Replaces the persisted tool-result message | Off by default; opt in | — |
| Hermes | `hermes` | Hard-disabled | Blocks the first call and suggests Core's rewrite | Replaces accepted results or adds error guidance; supports Marker command recovery | Core-selected for replaceable text | — |
| Qoder | `qoder` | Hard-disabled | Emits rewritten shell input | Replaces output through `updatedToolOutput` | Pipeline-selected for replaceable text | — |
| Claude Code | `claude-code` | Hard-disabled | Replaces Bash input | Replaces output on 2.1.121 or later; otherwise passes through | Pipeline-selected for replaceable text | — |
| Codex | `codex` | Hard-disabled | Replaces supported shell input | Keeps the original; adds context only for classified environment failures | — | — |
| DeepSeek Harness | `dsh` | — | — | Delegates accepted single-text results to Core; supports Marker command recovery | Core-selected for replaceable text | — |
| OpenCode | `opencode` | Hard-disabled | Replaces Bash input | Replaces tool output | Pipeline-selected for replaceable text | — (hook runs, tools returned unchanged) |
| Qwen Code | `qwencode` | Hard-disabled | Emits rewritten shell input | Passes through because the host has no replacement field | — | — |
| QwenPaw | `qwenpaw` | — | Replaces the `execute_shell_command` input | Replaces text blocks of the tool result inside the AgentScope middleware chain | Core-selected for replaceable text | ✅ |

“—” means that the capability is not available: the current adapter does not register it, current host releases do not run it, or it runs without taking effect (see the cell's note). The corresponding Tokenless CLI command may still be available.

Schema compression reaches the model path differently per host: cosh and Cosh-NG fire the `BeforeModel` hook; OpenCode runs the same hook for each tool definition through its `tool.definition` plugin hook (MCP tools do not pass through it); current Qwen Code releases do not run the declared `BeforeModel` event. None of these hosts applies the result: the shared hook has no marker-authorized recovery (see [Adapter processing rules](#adapter-processing-rules)), and Qwen Code does not run the event at all. Only QwenPaw and AgentScope, which declare a static recovery Tool, replace tool definitions.

Tool Ready remains registered by these adapters but is unconditionally hard-disabled before checking, repair, or blocking. No runtime setting can re-enable it. Post-tool failure attribution is independent.

`additionalContext` is an additive hook field. The shared hook does not place compressed copies
there because the original would remain visible and total context would grow. It uses that field
only for additive environment-error guidance. A statistics record proves that a candidate became
smaller, not by itself that the host removed the original from its model request.

## Adapter processing rules

The shared Cosh-NG, Qoder, Claude Code, and OpenCode PostTool hook sends one
`post_tool` request to `tokenless compress`. When the host can replace the result and bare
`tokenless` resolves on its shell `PATH`, a Marker can direct the model to recover omitted content
with the existing shell tool. Otherwise Core accepts only lossless candidates. Every non-`applied`
disposition keeps the original. The hook currently routes:

| Content | Current shared-hook behavior |
|---------|------------------------------|
| JSON | Lossless structural cleanup; TOON may be selected for text-capable replacement slots |
| JSON requiring record reduction or string, array, or depth truncation | Applied only when Marker command recovery is available; otherwise rejected with `recoverability_unavailable` |
| Build/test/package logs from command output | Terminal cleanup and routine-progress reduction; every omitted run carries an in-place retrieval marker |
| CSV/TSV tables when the host can replace output with text | Full compaction; tables with more than 32 data rows may reduce rows when Stash-backed recovery is available |
| API search-result listings when path sharing is enabled and the host can replace output with text | Lossless search path sharing; every received match is retained |
| Git diffs from command output when `TOKENLESS_DIFF_COMPRESSION_ENABLED` opts in (default off) and the host can replace output with text | Unchanged-context cropping with per-hunk selection; every changed line is kept, the complete original stays retrievable through Stash, and marginal candidates are rejected |
| HTML pages from command output or API responses (not shell file reads) when `TOKENLESS_HTML_EXTRACTION_ENABLED` opts in (default off), the host can replace output with text and Stash-backed recovery is available | Markdown rendering of the page's content root; the received page stays retrievable through Stash; otherwise passthrough |
| Long plain text, stack trace, source code, unknown | Passthrough |

Content detection, the 200-character PostTool gate, tool-origin thresholds, diagnostics, TOON
selection, and final acceptance are Core policy; the hook maps host objects to protocol fields
and puts the result back into the host's shape.

The shared BeforeModel hook, which cosh, Cosh-NG and OpenCode's per-tool definition path all use,
has no marker-authorized recovery path, so schema compression returns the tools unchanged there.
Only the direct `compress-schema` command and in-process integrations that declare a static
recovery Tool (QwenPaw, AgentScope) apply it.

OpenClaw, Hermes, and DeepSeek Harness delegate their PostTool decisions to Core. The standalone
`compress-response` command remains the explicit JSON cleanup interface.

For JSON response cleanup, adapters map host tools to Core's content origins as follows:

| Class | Default adapter behavior |
|-------|--------------------------|
| Content retrieval, including Read/Glob/Grep/LSP/NotebookRead aliases | Skip response compression |
| Shell/exec | 65,536-character strings, 128 retained array items, depth 8 |
| Other structured tools | 1,048,576-character strings, 65,536 retained array items, depth 32 |

The PostTool size gate, tool-origin thresholds, and TOON selection belong to Core for Common Hooks,
OpenClaw, and Hermes. TOON runs only when the selected host slot accepts text and Core finds a
smaller valid representation. The standalone `compress-toon` CLI and SDK TOON path retain their
documented default minimum, while the CLI can lower it per call with `--min-toon-chars`. Codex and
Qwen Code do not run response compression or TOON because their current PostToolUse contracts
cannot replace the original model-visible output.

RTK output is never compressed a second time: the shared hooks and OpenClaw carry RTK ownership
into the matching PostTool call, and Hermes recognizes the RTK wrapper in the command it actually
executed.

Claude Code requires version 2.1.121 or later for `updatedToolOutput`. On older or unknown versions, response compression is disabled to avoid duplicating the original. Structured tool outputs preserve their host schema and do not switch to textual TOON; JSON carried as a string can use TOON when it is smaller.

### DeepSeek Harness native processing

The DSH bundle requires Node.js 22 or later and a compatible DSH profile. Pass
all desired profile names in the same enable command, then start DSH with one
of those names:

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
dsh --profile web
```

`--profile` is required and repeatable. Each enable or re-enable treats its
arguments as the complete desired profile set. It removes the bundle from any
profile recorded by the prior receipt but omitted from the new command, so
always include every profile that should retain Tokenless. ANOLISA records the
selected profiles and their resolved DSH home in the adapter receipt, so later
status, disable, and re-enable operations continue to address the same profile
tree.

The plugin sends replaceable root results containing one text block to
`tokenless compress`; Core owns content detection, compression, TOON selection,
size gates, tool-origin thresholds, and final acceptance. Unsupported content
domains and file-content results pass through. When bare `tokenless` resolves on
DSH's shell `PATH` to the same executable selected for the Core call, a Marker
can ask the model to run a standalone `tokenless retrieve` command; its
successful output bypasses compression. Multi-block results, images and the
successful results of Code Mode child calls stay untouched, a result whose value
another DSH policy has already replaced is never compressed (only a structured
command failure in that replacement still receives diagnostics), and a missing,
failing, or timed-out CLI preserves the original content.

DSH removes inherited `TOKENLESS_*` variables from model shell commands. The
adapter publishes managed aliases for the selected data directory and optional
statistics/Stash database overrides so Core and the shell recover from the same
state. By default this state is stored in `.tokenless` under the session
workspace. The adapter creates `.tokenless/.gitignore` with `*` so complete tool
text and Stash payloads are not included by `git add -A`. Set
`TOKENLESS_DATA_DIR`, `TOKENLESS_STATS_DB`, or `TOKENLESS_STASH_DB` before
starting DSH to use another absolute path accessible to its shell sandbox;
protect and exclude custom paths according to your repository policy.

Add an override for the installed row to
`$DSH_HOME/profiles/<profile>/cordis.patch.yml`, then restart that DSH profile:

```yaml
- id: anolisa-tokenless
  config:
    responseCompressionEnabled: true
    timeoutMs: 5000
    maxBuffer: 4194304
```

Later DSH patch layers replace the row's complete `config` value. The plugin
supplies defaults for omitted keys, so the override may contain only the keys
that need to differ.

| Option | Default | Behavior |
|--------|---------|----------|
| `responseCompressionEnabled` | `true` | Enables response compression. Setting it to `false` does not disable environment-error attribution. |
| `tokenlessBin` | `$TOKENLESS_BIN`, then `tokenless` | Selects the Tokenless CLI executable. A non-empty plugin value takes precedence over the environment variable. Marker recovery additionally requires bare `tokenless` on the shell `PATH` to resolve to this same executable. |
| `timeoutMs` | `3000` | Bounds one Tokenless child process in milliseconds. Only a positive integer is accepted. |
| `maxBuffer` | `2097152` | Bounds captured child-process output in bytes. Only a positive integer is accepted. |
| `agentId` | `dsh` | Sets the Agent attribution recorded by Tokenless statistics. |

The plugin maps DSH's built-in read/search tools to `file_content`, command
tools to `command_output`, and unknown tools to `api_response`; Core owns the
resulting policy. DSH-flagged failures, and command results whose structured value
reports a non-zero exit, a signal or a timeout, are sent to Core for environment
diagnosis even when compression is disabled.

For the full trigger conditions (compression switch, minimum response length, supported content domains, strictly-smaller guard) and threshold semantics, see [User manual · Compression trigger conditions and thresholds](user-manual.md#compression-trigger-conditions-and-thresholds).

## Manage adapters with anolisa (recommended)

These commands require an ANOLISA component record. If Tokenless was installed
directly with YUM, record the RPM once before continuing:

```bash
sudo yum install anolisa
sudo anolisa --install-mode system adopt tokenless
```

The YUM-installed CLI is available on sudo's system path; the user-local CLI
installed by `get.agentic-os.sh` may be hidden by sudo's `secure_path`.

Run the adapter commands below as the user who owns the target Agent
configuration. A user-scoped adapter operation can discover the adopted system
package while keeping the framework mutation in that user's configuration.

### 1. Scan Agent products

```bash
anolisa adapter scan
```

If the target framework is absent, confirm that its CLI or application is installed, then scan again.

### 2. Enable one adapter

```bash
anolisa adapter enable tokenless <framework>
```

Examples:

```bash
anolisa adapter enable tokenless cosh
anolisa adapter enable tokenless openclaw
anolisa adapter enable tokenless hermes
anolisa adapter enable tokenless qoder
anolisa adapter enable tokenless claude-code
anolisa adapter enable tokenless codex
anolisa adapter enable tokenless opencode
anolisa adapter enable tokenless qwencode
anolisa adapter enable tokenless qwenpaw
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

Enable only Agent products that you use. Run and verify each product's command
separately. For DSH, include every desired profile in its single enable
command.

DeepSeek Harness is profile-scoped and therefore requires at least one
`--profile`. Each name must match one passed to `dsh --profile <profile>`; the
generic command without a profile is rejected. A later enable or re-enable
must repeat every profile that should remain registered.

Enabling the OpenClaw adapter, through anolisa or the bundled `install.sh`,
accepts the plugin's declared capabilities; both pass `--accept-capabilities`
only when the host's `plugins install --help` lists it, so older hosts still
install. The standalone `install.sh` adds `--dangerously-force-unsafe-install`
only on hosts where the installer still treats it as effective; on OpenClaw
2026.6.5 and later the safety scan follows `security.installPolicy`.

For OpenClaw, anolisa first attempts a normal install and does not add an unsafe-install bypass by default. If OpenClaw rejects the plugin on its safety scan, read the reported findings. Only after accepting them, retry explicitly:

```bash
anolisa adapter enable tokenless openclaw \
  --allow-unsafe-plugin-install
```

On OpenClaw releases where the underlying bypass is unsupported or a deprecated no-op, anolisa refuses this option; follow the error's `security.installPolicy` guidance instead.

The component package may be system-scoped while the adapter receipt remains
user-scoped. Use `sudo` only when the target framework configuration and its
adapter receipt are intentionally owned by root.

### 3. Check status

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

Restart the target agent CLI or IDE afterwards. A running session normally does not load a newly installed hook or plugin dynamically.

### 4. Disable

```bash
anolisa adapter disable tokenless <framework>
```

Disable the adapter with the same user that enabled it. A root-owned receipt is
the exception and requires `sudo` for both operations.

Restart the target agent after disabling. All enabled adapters must be released before Tokenless can be uninstalled.

## Manual integration after npm installation

The npm postinstall script attempts to copy adapter resources under:

```text
~/.local/share/anolisa/adapters/tokenless/
```

Confirm that this directory exists. Adapter copying is supplementary and fails open with a warning; a successful binary install can therefore exist without this copy. If it is absent, review the npm postinstall warning and prefer an anolisa-managed installation.

An npm install does not create an anolisa component installation record, so do not assume that `anolisa adapter enable` can manage it. OpenClaw, Hermes, Qoder, Claude Code, Codex, OpenCode, Qwen Code, and QwenPaw provide their own install scripts:

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/install.sh
```

For example:

```bash
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh
bash ~/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh
```

Uninstall the same adapter with:

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/uninstall.sh
```

The scripts call the framework's own plugin or extension mechanism. Follow their restart instructions. If a script is missing, fails, or reports an incompatible framework version, prefer an anolisa-managed installation.

On hosts whose installer still enforces the safety scan, the OpenClaw install script passes `--dangerously-force-unsafe-install` because the plugin launches the `tokenless` and `rtk` binaries; on newer hosts the scan follows `security.installPolicy`. Review the installed adapter source and your OpenClaw policy before running it, and do not install the plugin where that policy forbids the override.

### npm with cosh

cosh uses an Extension directory and does not provide a separate `scripts/install.sh`. Copy the npm-installed shared resources into the user Extension directory:

```bash
mkdir -p ~/.copilot-shell/extensions/tokenless
cp -R ~/.local/share/anolisa/adapters/tokenless/common/hooks \
  ~/.local/share/anolisa/adapters/tokenless/common/commands \
  ~/.local/share/anolisa/adapters/tokenless/common/cosh-extension.json \
  ~/.copilot-shell/extensions/tokenless/
```

Restart cosh afterwards. Before removing it, exit cosh and confirm that the target directory is the Tokenless Extension created by this npm installation.

## Agent adapter activation notes

### cosh

Extensions are discovered at startup. Restart cosh, run a shell-tool task, and inspect `tokenless stats list`.

### OpenClaw

The install script uses OpenClaw's unsafe-install override on legacy hosts, as described above. Restart the gateway after accepting and installing the plugin. Response compression and RTK rewriting default to enabled in the plugin code; TOON defaults to disabled. The plugin's Tool Ready option currently has no effect because the underlying check is hard-disabled.

### Hermes

The plugin takes effect in a new Hermes session. Restart Hermes, run a shell-tool task to verify the
block-and-retry rewrite, then run a JSON-returning tool to verify result replacement. When bare
`tokenless` resolves on the shell `PATH`, a Marker can ask Hermes to run `tokenless retrieve`; the
successful recovery result is returned without another compression pass.

### Qoder

Qoder IDE and qodercli may cache plugin configuration. Fully restart the IDE after enabling or upgrading. If an old hook path is reported, see [Qoder plugin cache issue](troubleshooting.md#qoder-plugin-cache-issue).

### Claude Code

The marketplace plugin takes effect after restarting Claude Code. The install script may also offer a plugin refresh command.

### Codex

The plugin loads in a new Codex session. Close the old session and start a new one before verifying behavior. Codex PostToolUse cannot replace or suppress the original output, so the plugin does not append compressed content or record response-compression candidates. It adds context only for classified environment failures. Actual first-pass savings come from RTK rewriting supported shell commands before execution.

### DeepSeek Harness

The native bundle loads when the selected DSH profile starts. After enabling
or changing its profile patch, restart `dsh --profile <profile>`, run a tool
that returns compressible JSON, and inspect `tokenless stats list`. Disable the
adapter with `anolisa adapter disable tokenless dsh`; the receipt already
records the profile names, so disable does not accept another `--profile`.

### OpenCode

OpenCode discovers global local plugins at startup. For an ANOLISA-managed installation, use:

```bash
anolisa adapter enable tokenless opencode
anolisa adapter status tokenless
anolisa adapter disable tokenless opencode
```

The built-in driver resolves the configuration directory from `OPENCODE_CONFIG_DIR`, then
`XDG_CONFIG_HOME/opencode`, and finally `~/.config/opencode`. It does not read
`TOKENLESS_OPENCODE_CONFIG_DIR`; use `OPENCODE_CONFIG_DIR` for a custom directory shared with
the standalone scripts. Keep the same directory setting when disabling the adapter.

The bundled lifecycle scripts described above remain available for npm and manual installs;
source builds can use `make opencode-install`. These scripts additionally accept
`TOKENLESS_OPENCODE_CONFIG_DIR` as their highest-priority override. Both paths create
`plugins/tokenless.js` and refuse conflicting files or links. ANOLISA enable adopts an existing
link to the same plugin source into its receipt, and subsequent disable removes that link.
Relative links are resolved from their original directory and must match the recorded source
path lexically; links through directory aliases or a different installation prefix are preserved
as conflicts. This keeps cleanup possible after the source directory is removed.

Before enabling through ANOLISA, uninstall a conflicting standalone link using its original
installation profile and any original `PREFIX` or `SHARE_DIR` overrides. For example, from the
Tokenless source checkout, remove a link installed with the default system prefix:

```bash
make opencode-uninstall INSTALL_PROFILE=system PREFIX=/usr
anolisa adapter enable tokenless opencode
```

Keep the original configuration-directory settings for both commands. If using the bundled
`scripts/uninstall.sh`, run the copy in the original adapter bundle with a matching
`ANOLISA_ADAPTER_DIR` if that variable is set. Uninstalling from another prefix leaves the link
in place and exits 0 with a warning; check that the link was removed before enabling. If the
original bundle is unavailable, inspect `plugins/tokenless.js` with `readlink` and manually
remove only the confirmed stale symlink; preserve unrelated files and directories.

To return to standalone management, first complete `anolisa adapter disable tokenless opencode`,
including any pending recovery. Keep reported recovery directories until cleanup succeeds;
standalone scripts do not recover ANOLISA receipts or journals. Then rerun
`make opencode-install` or the bundled `scripts/install.sh`.

Restart OpenCode after enabling or disabling the plugin: an existing process keeps its loaded
plugin, including tool-output replacement, until restart. After enabling and restarting, run a
tool call and inspect `tokenless stats list`.

### Qwen Code

The extension loads in a new Qwen Code session. Restart and run one tool call to verify it.

### QwenPaw

The adapter is a QwenPaw plugin: `anolisa adapter enable tokenless qwenpaw` and the bundled install script both run `qwenpaw plugin install <bundle> --force`, which copies the plugin into `<working dir>/plugins/tokenless/` and installs the `anolisa_tokenless` SDK wheel from the matching GitHub Release into QwenPaw's Python environment, so the first install needs network access. QwenPaw runs pip only when the package is missing, so on an offline host install the wheel first; an already installed older wheel is never upgraded by `plugin install`. Wheels exist only for Linux x86_64, Linux aarch64 and macOS arm64; on any other platform the install script fails after confirming that QwenPaw's Python cannot import `anolisa_tokenless`, and without a `qwenpaw` command on `PATH` it only prints a hint and exits 0 without installing anything. The plugin refuses to register against a wheel that lacks the SDK entry points it imports (introduced in Tokenless 0.8.0) and logs which release to install. The working directory is resolved like QwenPaw itself: `QWENPAW_WORKING_DIR`, else `COPAW_WORKING_DIR`, else an existing `~/.copaw`, else `~/.qwenpaw`.

A running QwenPaw hot-loads the plugin; otherwise start QwenPaw. Schema compression and the `tokenless_retrieve` tool apply from the next model call, and an approved `execute_shell_command` executes the rewritten command. Only QwenPaw's built-in tools are classified: `execute_shell_command` is command output; `read_file`, `recall_history`, `view_image`, and `view_video` are file content; the remaining built-ins are API responses. Skills, MCP tools, and tools added by later QwenPaw releases pass through untouched. QwenPaw's own tool-result pruning runs after Tokenless and keeps only the head of each result, with a larger budget for the two most recent ones, so a recovery instruction at the end of a compressed result may be cut off; the omitted content stays retrievable through the `tokenless_retrieve` tool, or with `tokenless retrieve --stash-db <workspace>/.tokenless/stash.db`. Records land under `<workspace>/.tokenless` for each QwenPaw workspace; point `tokenless stats list --data-dir` there.

## AgentScope framework integration

AgentScope is the second Python SDK layer, not a product adapter. Its complete build, version,
attachment, configuration, and validation guidance now lives in
[AgentScope SDK integration](sdk/agentscope.md). This heading remains as a compatibility pointer for
existing links.

## Verify an Agent adapter

For an Agent adapter, do not treat a zero install exit code as the only success criterion. At
minimum, run:

```bash
tokenless --version
anolisa adapter status tokenless
tokenless stats list --limit 5
```

Then execute a tool task with visible output in the target agent. If `stats list` remains empty, follow [No statistics appear after enabling the adapter](troubleshooting.md#no-statistics-appear-after-enabling-the-adapter).

## Related documents

- [Quick Start](QUICKSTART.md)
- [Python SDK](sdk.md)
- [AgentScope SDK integration](sdk/agentscope.md)
- [Measuring savings](measuring-savings.md)
- [Configuration and data privacy](configuration-and-privacy.md)
- [Troubleshooting](troubleshooting.md)
