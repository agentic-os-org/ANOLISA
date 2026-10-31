# Tokenless Agent Integration

[中文版](../../zh/tokenless/framework-integration.md)

Tokenless connects to Agent products through plugins, hooks, and extensions. This guide covers
product adapters. The Python SDK and its AgentScope-specific child document live under
[Python SDK](../../../developer-guide/en/sdk/tokenless/index.md).

## Agent adapter support matrix

| Agent product | Value | Tool Ready | Rewrite behavior | Response delivery | TOON | Schema |
|-----------|-------|------------|------------------|-------------------|------|--------|
| cosh-ng | `cosh` | Hard-disabled | Replaces supported shell input | Cosh-NG replaces supported JSON results; legacy Copilot Shell passes through | Pipeline-selected for replaceable text | Lossless-only through the Common Hook |
| OpenClaw | `openclaw` | Hard-disabled | Replaces the `exec` command input | Replaces single-text tool results before model consumption | Core-selected for replaceable text | — |
| Hermes | `hermes` | Hard-disabled | Blocks the first call and suggests Core's rewrite | Replaces accepted results or adds error guidance; supports Marker command recovery | Core-selected for replaceable text | — |
| Qoder | `qoder` | Hard-disabled | Emits rewritten shell input | Replaces output through `updatedToolOutput` | Pipeline-selected for replaceable text | — |
| QwenWorkCN (macOS) | `qwenwork` | — | Replaces native Bash and workspace Shell input | Replaces eligible full results; telemetry-only MCP wrappers pass through | Pipeline-selected for replaceable text | — |
| Claude Code | `claude-code` | Hard-disabled | Replaces Bash input | Replaces output on 2.1.121 or later; otherwise passes through | Pipeline-selected for replaceable text | — |
| Codex | `codex` | Hard-disabled | Replaces supported shell input | Keeps the original; adds context only for classified environment failures | — | — |
| DeepSeek Harness | `dsh` | — | — | Delegates accepted single-text results to Core; supports Marker command recovery | Core-selected for replaceable text | — |
| OpenCode | `opencode` | Hard-disabled | Replaces Bash input | Replaces tool output | Pipeline-selected for replaceable text | ✅ |
| Qwen Code | `qwencode` | Hard-disabled | Emits rewritten shell input | Passes through because the host has no replacement field | — | — |
| QwenPaw | `qwenpaw` | — | Replaces the `execute_shell_command` input | Replaces text blocks of the tool result inside the AgentScope middleware chain | Core-selected for replaceable text | ✅ |
| Trae | `trae` | Hard-disabled | Replaces RunCommand input | Keeps the original; adds context only for classified environment failures | — | — |

“—” means that the capability is not available: the current adapter does not register it, or current host releases do not run it. The corresponding Tokenless CLI command may still be available.

Cosh-NG uses the adapter value `cosh` and the existing `.copilot-shell` extension directory.

Schema compression reaches the model path differently per host: cosh and Cosh-NG fire the `BeforeModel` hook; OpenCode compresses each tool definition through its `tool.definition` plugin hook (MCP tools do not pass through that hook); Qwen Code's manifest declares a `BeforeModel` hook, but current Qwen Code releases skip that unknown event name at registration, so the schema hook does not run there and the matrix marks it unavailable. The entry stays registered, so a future Qwen Code release that implements the event picks it up automatically.

Tool Ready remains registered by these adapters but is unconditionally hard-disabled before checking, repair, or blocking. No runtime setting can re-enable it. Post-tool failure attribution is independent.

`additionalContext` is an additive hook field. The shared hook does not place compressed copies
there because the original would remain visible and total context would grow. It uses that field
only for additive environment-error guidance. A statistics record proves that a candidate became
smaller, not by itself that the host removed the original from its model request.

PostTool leaves the entire JSON response unchanged when its parser cannot preserve a numeric value. Otherwise, cleanup and record reduction can proceed; Core selects TOON only when decoding it recovers the same retained values and types.

Trae currently uses the bundled lifecycle scripts documented below and is not registered with the
`anolisa adapter enable` driver set in this release.

## Adapter processing rules

The shared Cosh-NG, Qoder, QwenWork, Claude Code, and OpenCode PostTool hook sends one
`post_tool` request to `tokenless compress`. When the host can replace the result and bare
`tokenless` resolves on its shell `PATH`, a Marker can direct the model to recover omitted content
with the existing shell tool. Otherwise Core accepts only lossless candidates. Every non-`applied`
disposition keeps the original. The hook currently routes:

| Content | Current shared-hook behavior |
|---------|------------------------------|
| JSON | Lossless structural cleanup; TOON may be selected for text-capable replacement slots |
| JSON requiring record reduction or string, array, or depth truncation | Applied only when Marker command recovery is available; otherwise rejected with `recoverability_unavailable` |
| Build/test/package logs from command output | Terminal cleanup and routine-progress reduction; every omitted run carries an in-place retrieval marker |
| CSV/TSV tables when the host can replace output with text | Complete cell-preserving compaction only when characters and estimated tokens decrease; otherwise passthrough. No Stash writes or recovery requirement |
| Numbered API search records, or pure path lists from non-file command/API results, when path sharing is enabled and the host can replace output with text | Lossless sharing of consecutive paths or a directory prefix; every received record is retained, without Stash or retrieval |
| Git diffs from command output when `TOKENLESS_DIFF_COMPRESSION_ENABLED` opts in (default off) and the host can replace output with text | Unchanged-context cropping with per-hunk selection; every changed line is kept, the complete original stays retrievable through Stash, and marginal candidates are rejected |
| Complete HTML documents from command output or API responses (not file reads) when the host can replace output with text and Stash-backed recovery is available; enabled by default, `TOKENLESS_HTML_EXTRACTION_ENABLED=0` disables it | Main-content rendering as Markdown with removal counts in the view header; the complete original stays retrievable through Stash, and marginal candidates are rejected |
| Long plain text, stack trace, source code, unknown | Passthrough until a matching domain compressor is connected |

Content detection, the 200-character PostTool gate, tool-origin thresholds, diagnostics, TOON
selection, and final acceptance are Core policy. The hook maps host objects to v2 fields and may
skip obvious non-JSON skill files only to avoid an unnecessary subprocess.

The Common BeforeModel hook likewise has no marker-authorized recovery path. Current schema
transformations are lossy, so Core passes the tools through unchanged. OpenCode's separate per-tool
definition path and the direct `compress-schema` command are unchanged.

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

Common Hooks and OpenClaw carry RTK ownership into the matching PostTool call. Hermes supports older
host releases by blocking and suggesting a retry; its final-result hook recognizes the attributed
RTK wrapper, including optional execution-generation assignments, from the command Hermes actually executed. Core resumes PostTool only after complete
receipts confirm unchanged output from a successful call, using the saved original command for
file-origin checks. The host must return the exact registered execution command in PostTool `command`;
missing or modified commands retain the bypass, as does changed or unconfirmed output. All three adapters declare
`recovery: none` for RTK output before receipt lookup, so fallback accepts only lossless candidates;
it does not enable Stash/retrieve recovery for these calls.

Claude Code requires version 2.1.121 or later for `updatedToolOutput`. On older or unknown versions, response compression is disabled to avoid duplicating the original. Structured tool outputs preserve their host schema and do not switch to textual TOON; JSON carried as a string can use TOON when it is smaller.

### QwenWork (Windows x64 local mode)

Tokenless automatically reduces supported command and tool output in QwenWork,
while preserving file reads. Windows support covers QwenWorkCN 1.3.2 local
sessions on x64, with Node.js/npm and Python 3.10+ installed. Windows ARM64 and
security-workspace/VM sessions are unsupported; other Windows agents have not
been validated.

Install the published package with `npm install -g anolisa-tokenless`. For source builds, follow the
[Windows build instructions](../../../developer-guide/en/tokenless/windows.md#build-and-test-on-windows)
to build and install the local npm packages.

After installing the npm package, quit QwenWorkCN and run in PowerShell:

```powershell
$adapter = Join-Path (npm root -g) 'anolisa-tokenless/adapters/tokenless/qwenwork/scripts/lifecycle.ps1'
& $adapter -Action detect
& $adapter -Action install
```

Restart QwenWorkCN, enable the Tokenless plugin in the application if needed,
and open a new local session. Continue using QwenWork as usual; supported output
is reduced automatically.

If Python cannot be found, add `-Python 'C:/path/to/python.exe'` to the commands
above. For a custom QwenWork installation, set `TOKENLESS_QWENWORK_APP` to the
directory containing `QwenWorkCN.exe` before running them.

To remove the integration, quit QwenWorkCN, run the following, then restart it:

```powershell
& $adapter -Action uninstall
```

### QwenWork (macOS)

The `qwenwork` adapter targets QwenWorkCN (千问办公), independently of Qoder CLI,
Qwen Code, and QwenPaw. QwenWorkCN 1.2.5 macOS local sessions have been tested for
Bash rewriting, full MCP response replacement, JSON-to-TOON output, and Read
passthrough. The adapter also accepts the macOS workspace Shell contract observed
in QwenWorkCN 1.3.2. Linux VM execution remains unsupported; the hook dispatcher
passes through outside macOS.

PreToolUse matches `Bash`, `mcp__workspace__bash`, and
`mcp__qw-builtin__mcp_workspace_bash`, and rewrites supported commands through RTK.
PostToolUse has **no tool matcher**: eligible full Bash and non-Bash results enter
the shared policy. Metadata-only callbacks from `mcp_list`, `mcp_get`, or `mcp_call`
pass through because replacing their real output with telemetry would lose content;
full `mcp_call` responses still use normal compression. Accepted results replace
the original through `updatedToolOutput`; objects and arrays retain their types,
while text slots can use TOON. File-content protection, unsupported-content
passthrough, and RTK deduplication still apply. Statistics use the separate Agent
ID `qwenwork`. Tool Ready and Schema hooks are not registered.

A workspace Shell result containing exactly one plain MCP text block enters the
text slot, enabling JSON-to-TOON compression. Multi-block, multimodal, or annotated
block arrays pass through. Other MCP tools retain their existing structured-output
handling. For QwenWork workspace Shell hooks, Tokenless databases and Pre/Post
deduplication state use the default `~/.tokenless` directory or the absolute path
specified by `TOKENLESS_DATA_DIR`. The default home comes from the operating
system's user identity, as in the CLI/RTK, rather than `$HOME`.
The adapter preserves that variable
and no longer selects a workspace-local directory automatically. Existing databases in
`<cwd>/.tokenless` are not automatically migrated; explicitly select that directory
to query earlier records.

An unset or empty `TOKENLESS_DATA_DIR` uses the default. Invalid paths (relative,
filesystem root, parent traversal, non-directory targets, or unresolvable links)
make workspace Shell hooks pass through without creating a data directory.
The same applies when no system home is available and no override is set.

If the Shell sandbox cannot write to the selected directory, RTK's Tokenless
statistics and execution receipts may be missing while commands still run.
Missing receipts conservatively skip further compression. PostTool compression
statistics are written by the Hook process and depend on its permissions.
Statistics can therefore be incomplete even when the Agent completes its task.
Failed RTK records are not queued for later recovery.

RTK's native database has its own path configuration, independent of
`TOKENLESS_DATA_DIR`. Its writes are
subject to the Shell sandbox too. The adapter keeps this behavior unchanged.

Installation and detection output show the data directory and its source for the
current process, together with this sandbox limitation. They do not test sandbox
write access or verify that the desktop inherited terminal environment variables.
These notices are not added to Hook results or model context. For complete RTK
records, use a directory the Shell is authorized to write. The adapter does not
change sandbox permissions or transfer records across that boundary.

If a standalone installation includes `bin/` inside the plugin, the adapter uses
those binaries.
Workspace Shell and plugin-private binaries do not advertise bare
`tokenless retrieve` recovery, since the model shell does not inherit the hook
environment. Their PostTool compression therefore requires a lossless result.

Install Tokenless with `anolisa install tokenless` first. This adapter needs Bash,
Python 3.9+, and the Tokenless binary. RTK is required for command rewriting but
not for response compression; a missing RTK produces a warning. Ensure these
binaries are available to the desktop session, not only an interactive terminal.

Quit QwenWorkCN before installation or upgrade, then run:

```bash
anolisa adapter scan
anolisa --dry-run adapter enable tokenless qwenwork
anolisa adapter enable tokenless qwenwork
anolisa adapter status tokenless
```

The CLI copies a self-contained plugin to `~/.qwenworkcn/plugins-custom/tokenless`
and records file ownership. Restart QwenWorkCN and open a new session;
check plugin activation in the application. `status` verifies application presence
and managed files, while loading and activation remain `unknown`. Sideloading
never changes settings, permissions, databases, or disabled/blocked plugin lists.
Compatibility baselines are 1.2.5 native Bash and 1.3.2 workspace Shell; other
versions produce a compatibility warning.

`TOKENLESS_QWENWORK_APP` selects a non-default application bundle;
`TOKENLESS_QWENWORK_HOME` selects its data directory. Defaults search `/Applications`
and `~/Applications`, with data under `~/.qwenworkcn`. Use absolute paths (a leading
`~` is supported). Keep the same data directory for status, disable, and re-enable;
to move it, disable using the original setting first. Do not use Qoder CLI or
Qwen Code configuration directories.

Existing installations without a CLI receipt are not adopted. If you previously
used the bundled scripts or `make qwenwork-install`, uninstall with the original
script before enabling through the CLI (adjust the resource path as needed):

```bash
bash ~/.local/share/anolisa/adapters/tokenless/qwenwork/scripts/uninstall.sh
anolisa adapter enable tokenless qwenwork
```

Use the CLI to remove CLI-managed copies:

```bash
anolisa --dry-run adapter disable tokenless qwenwork
anolisa adapter disable tokenless qwenwork
```

Removal does not require the application, source bundle, or hook dependencies.
It removes only unchanged receipt-owned files and empty plugin directories;
unrelated files remain. Modified files are preserved and keep a failed-cleanup
receipt: restore or remove the reported conflict, then retry. Re-enable refuses
to overwrite user changes. Interrupted copies retain a receipt for retry or cleanup.
Restart the application after removal as well.

The standalone bundled scripts and source-checkout `make qwenwork-install` /
`make qwenwork-uninstall` remain separate alternatives. CLI copies exclude those
lifecycle scripts and their ownership marker. Neither route uses the embedded
Qoder CLI or undocumented desktop APIs. To return to standalone management,
disable the CLI-managed adapter first, then use the standalone installer.

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

The plugin runs on DSH's `tools/post-execute` waterfall and sends replaceable
root results containing one text block to `tokenless compress`. Core owns
content detection, JSON and Build Log compression, TOON selection, size gates,
tool-origin thresholds, and final acceptance. Unsupported content domains and
file-content results pass through. When bare `tokenless` resolves on DSH's shell `PATH` to the same
executable selected for the Core call, a Marker can ask the model to run a
standalone `tokenless retrieve` command; its successful output bypasses
compression. Multiple blocks, images, Code Mode child successes, and canonical
values replaced by a later waterfall listener remain untouched. A missing,
failing, or timed-out CLI also preserves the original content.

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
tools to `command_output`, and unknown tools to `api_response`. These mappings
only describe host facts; Core owns the resulting policy. Raw DSH failures and
structured command failures are sent to Core for environment diagnosis even
when compression is disabled. When a later waterfall listener replaces the
canonical `value`, Tokenless examines only that replacement and never applies
content compression to it.

For the full trigger conditions (compression switch, minimum response length, supported content domains, strictly-smaller guard) and threshold semantics, see [User manual · Compression trigger conditions and thresholds](index.md#compression-trigger-conditions-and-thresholds).

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

Running OpenClaw adapter enable or the tokenless OpenClaw `install.sh` accepts
the plugin's declared capabilities. Both entry points pass
`--accept-capabilities` only when `plugins install --help` advertises that
exact option, so older hosts keep working. The standalone `install.sh` also
passes `--dangerously-force-unsafe-install` only while the installer advertises
the option as effective; hosts that list it as a deprecated no-op (OpenClaw
2026.6.5+) no longer receive it, and the safety scan there follows
`security.installPolicy`.

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

On hosts whose installer still enforces the safety scan, the OpenClaw install script invokes `plugins install` with `--dangerously-force-unsafe-install` because the plugin launches the `tokenless` and `rtk` binaries through Node.js child-process APIs. Hosts that advertise the option as a deprecated no-op no longer receive it; there the scan follows `security.installPolicy`. Review the installed adapter source and your OpenClaw policy before running it. If that policy does not permit the override, do not install the plugin.

### npm with Cosh-NG

Cosh-NG uses an Extension directory and does not provide a separate `scripts/install.sh`. Copy the npm-installed shared resources into the user Extension directory:

```bash
mkdir -p ~/.copilot-shell/extensions/tokenless
cp -R ~/.local/share/anolisa/adapters/tokenless/common/hooks \
  ~/.local/share/anolisa/adapters/tokenless/common/commands \
  ~/.local/share/anolisa/adapters/tokenless/common/cosh-extension.json \
  ~/.copilot-shell/extensions/tokenless/
```

Restart Cosh-NG afterwards. Before removing it, exit Cosh-NG and confirm that the target directory is the Tokenless Extension created by this npm installation.

## Agent adapter activation notes

### Cosh-NG

Extensions are discovered at startup. Restart Cosh-NG, run a shell-tool task, and inspect `tokenless stats list`.

### OpenClaw

The install script uses OpenClaw's unsafe-install override on legacy hosts, as described above. Restart the gateway after accepting and installing the plugin. OpenClaw 2026.6.6 or later is required. Explicitly enable the Tokenless plugin so its declared tool-result middleware can replace text before the same-turn model request. Response compression and RTK rewriting default to enabled; Core selects eligible lossless TOON candidates. The plugin's Tool Ready option currently has no effect because the underlying check is hard-disabled.

Tokenless processes only the result delivered by OpenClaw. On legacy hosts such as
2026.8.2, registering any applicable result middleware causes the host to cap each
text block at 100,000 UTF-16 code units before the callback, even if the middleware
returns no replacement. Large `details` may also be truncated, losing fields such
as exit status. Fail-open preserves this received result; it cannot recover text
or metadata already removed by the host. A clipped JSON document may therefore
pass through without compression. These hosts remain supported within that limit.

For full native text input at this boundary, prefer an OpenClaw build containing
[upstream commit `2f620b970a04`](https://github.com/openclaw/openclaw/commit/2f620b970a0458531588d6c8067540164197580e).
The tested build and the earlier release both report `2026.10.2`, so the version
string alone does not establish that the fix is present. Legacy nested
`toolResult` normalization, individual tools and downstream model-context budgets
can still limit output. Tokenless does not automatically read a saved full-output
file or restore a discarded tail. Set `post_tool_enabled: false` to stop Tokenless
from registering this middleware; other plugins may still register their own.

The optional `selective-claw` context engine has its own configuration under
`plugins.entries.selective-claw.config`, separate from the response adapter:

```json
{
  "plugins": {
    "entries": {
      "selective-claw": {
        "config": { "enabled": true, "freshTailTurns": 3, "dbPath": "~/.openclaw/selective-claw.db" }
      }
    }
  }
}
```

`~/` expands to the current user's home directory. With no `dbPath`, the archive
is `selective-claw.db` under `OPENCLAW_STATE_DIR`, or under `~/.openclaw` when that
variable is unset. Setting the plugin's `enabled` option to `false` skips database,
context engine and recall tool registration.

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

Schema compression uses the host's model-facing JSON Schema while preserving its runtime
parameter validator. Hosts that expose only a runtime validator skip schema compression;
response compression and command rewriting remain available.

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

The adapter is a QwenPaw plugin: `anolisa adapter enable tokenless qwenpaw` and the bundled install script both run `qwenpaw plugin install <bundle> --force`, so QwenPaw copies the plugin into `<working dir>/plugins/tokenless/` and installs its `requirements.txt` into QwenPaw's own Python environment. That requirement is the `anolisa_tokenless` wheel from the matching GitHub Release, so the first install needs network access. QwenPaw only runs pip when `anolisa_tokenless` is missing from its interpreter's package metadata, so on an offline host `pip install` the wheel into QwenPaw's Python environment first; the same rule means an already installed older wheel is never upgraded by `plugin install`. The install script therefore checks, through the interpreter behind the `qwenpaw` command, that `anolisa_tokenless` imports and carries the SDK surface the plugin needs, and fails when no wheel matched the platform (`requirements.txt` lists Linux x86_64, Linux aarch64, and macOS arm64). The plugin itself refuses to register against an older wheel and logs the required release instead of failing at the first model call. The plugin requires the recovery entry points introduced in Tokenless 0.8.0. Install the SDK wheel matching the plugin release into QwenPaw's Python environment; the 0.7.14 wheel does not provide these APIs. The working directory is resolved like QwenPaw itself: `QWENPAW_WORKING_DIR`, else `COPAW_WORKING_DIR`, else an existing `~/.copaw`, else `~/.qwenpaw`. Without a `qwenpaw` command the install script prints a hint and exits 0 so `make setup` completes on hosts without QwenPaw.

A running QwenPaw hot-loads the plugin; otherwise start QwenPaw. Schema compression and the `tokenless_retrieve` tool apply from the next model call, and command rewriting runs after QwenPaw's approval step, so an approved `execute_shell_command` executes the rewritten command. Only QwenPaw's built-in tools are classified: `execute_shell_command` is command output, `read_file`, `recall_history`, `view_image`, and `view_video` are file content, and the remaining built-ins are API responses; skills, MCP tools, and tools added by later QwenPaw releases pass through untouched. QwenPaw's own tool-result pruning runs after Tokenless and keeps the head of each result (50000 bytes for the two most recent tool results, 3000 bytes for older ones, overflow written to `tool_results/`), so a recovery instruction at the end of a compressed result survives only while the result fits that budget; the omitted content stays retrievable from the stash with `tokenless retrieve`. Records land under `<workspace>/.tokenless` by default. Set an absolute `TOKENLESS_DATA_DIR` in QwenPaw's environment to override that location; relative paths are rejected. Point `tokenless stats list --data-dir` to the effective directory.

### Trae

Trae (TraeCode) has no plugin system for hooks. The bundled lifecycle script merges the Tokenless hook groups into the global `hooks.json` of every installed Trae edition (`~/.trae-cn/hooks.json` for the CN edition, `~/.trae/hooks.json` for the international edition):

```bash
# After `make -C src/tokenless install` (or the RPM) staged the adapter resources:
make -C src/tokenless trae-install
# or run the script directly:
bash ~/.local/share/anolisa/adapters/tokenless/trae/scripts/install.sh
# remove again:
bash ~/.local/share/anolisa/adapters/tokenless/trae/scripts/uninstall.sh
```

User-configured hooks are preserved; the uninstall script removes only the Tokenless-owned entries. Trae standardizes the terminal tool name to `RunCommand`, so the rewrite hook matches that name. Because Trae's PostToolUse does not support tool-output replacement, the response hook keeps the original output: it only adds `additionalContext` for classified environment failures and never appends a compressed copy, which would grow the model-visible payload. Restart Trae after installing or removing.

## AgentScope framework integration

AgentScope is the second Python SDK layer, not a product adapter. Its complete build, version,
attachment, configuration, and validation guidance now lives in
[AgentScope SDK integration](../../../developer-guide/en/sdk/tokenless/agentscope.md). This heading remains as a compatibility pointer for
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

- [Quick Start](index.md)
- [Python SDK](../../../developer-guide/en/sdk/tokenless/index.md)
- [AgentScope SDK integration](../../../developer-guide/en/sdk/tokenless/agentscope.md)
- [Measuring savings](measuring-savings.md)
- [Configuration and data privacy](configuration-and-privacy.md)
- [Troubleshooting](troubleshooting.md)
