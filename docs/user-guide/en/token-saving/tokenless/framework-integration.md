# Tokenless Agent Integration

[中文版](../../../zh/token-saving/tokenless/framework-integration.md)

Tokenless connects to Agent products through plugins, hooks, and extensions. This guide covers
product adapters. The Python SDK and its AgentScope-specific child document live under
[Python SDK](sdk.md).

## Agent adapter support matrix

| Agent product | Value | Tool Ready | Rewrite behavior | Response delivery | TOON | Schema |
|-----------|-------|------------|------------------|-------------------|------|--------|
| cosh | `cosh` | Hard-disabled | Replaces supported shell input | Cosh-NG replaces supported JSON results; legacy Copilot Shell passes through | Pipeline-selected for replaceable text | Lossless-only through the Common Hook |
| OpenClaw | `openclaw` | Hard-disabled | Replaces the `exec` command input | Replaces the persisted tool-result message | Off by default; opt in | — |
| Hermes | `hermes` | Hard-disabled | Blocks the first call and suggests Core's rewrite | Replaces accepted results or adds error guidance; supports Marker command recovery | Core-selected for replaceable text | — |
| Qoder | `qoder` | Hard-disabled | Emits rewritten shell input | Replaces output through `updatedToolOutput` | Pipeline-selected for replaceable text | — |
| Claude Code | `claude-code` | Hard-disabled | Replaces Bash input | Replaces output on 2.1.121 or later; otherwise passes through | Pipeline-selected for replaceable text | — |
| Codex | `codex` | Hard-disabled | Replaces supported shell input | Keeps the original; adds context only for classified environment failures | — | — |
| DeepSeek Harness | `dsh` | — | — | Delegates accepted single-text results to Core; supports Marker command recovery | Core-selected for replaceable text | — |
| OpenCode | `opencode` | Hard-disabled | Replaces Bash input | Replaces tool output | Pipeline-selected for replaceable text | ✅ |
| Qwen Code | `qwencode` | Hard-disabled | Emits rewritten shell input | Passes through because the host has no replacement field | — | — |
| QwenPaw | `qwenpaw` | — | Replaces the `execute_shell_command` input | Replaces text blocks of the tool result inside the AgentScope middleware chain | Core-selected for replaceable text | ✅ |
| WorkBuddy | `workbuddy` | Hard-disabled | Passes Bash input through by default; `TOKENLESS_WORKBUDDY_AUTO_ALLOW=1` opts in to replacing it via `modifiedInput` | Replaces output on CodeBuddy Code CLI hosts; other WorkBuddy hosts pass through unchanged | Attempted after response compression | — |

“—” means that the capability is not available: the current adapter does not register it, or current host releases do not run it. The corresponding Tokenless CLI command may still be available.

Schema compression reaches the model path differently per host: cosh and Cosh-NG fire the `BeforeModel` hook; OpenCode compresses each tool definition through its `tool.definition` plugin hook (MCP tools do not pass through that hook); Qwen Code's manifest declares a `BeforeModel` hook, but current Qwen Code releases skip that unknown event name at registration, so the schema hook does not run there and the matrix marks it unavailable. The entry stays registered, so a future Qwen Code release that implements the event picks it up automatically.

Tool Ready remains registered by these adapters but is unconditionally hard-disabled before checking, repair, or blocking. No runtime setting can re-enable it. Post-tool failure attribution is independent.

`additionalContext` is an additive hook field. The shared hook does not place compressed copies
there because the original would remain visible and total context would grow. It uses that field
only for additive environment-error guidance. A statistics record proves that a candidate became
smaller, not by itself that the host removed the original from its model request.

WorkBuddy currently uses the bundled lifecycle script documented below and is not registered with
the `anolisa adapter enable` driver set in this release.

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
| Build/test/package logs, long plain text, diff, stack trace, HTML, search results, tables, source code, unknown | Passthrough until a matching domain compressor is connected |

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
RTK wrapper from the command Hermes actually executed. All three therefore bypass a second
compression pass over RTK output.

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

OpenCode discovers global local plugins at startup. Use the bundled Tokenless lifecycle script described above, restart OpenCode after installation or removal, then run a tool call and inspect `tokenless stats list`. The script resolves the configuration directory from `TOKENLESS_OPENCODE_CONFIG_DIR`, then `OPENCODE_CONFIG_DIR`, then `XDG_CONFIG_HOME/opencode`, and finally `~/.config/opencode`. Installation creates only `plugins/tokenless.js` as a managed symlink and refuses to replace an unrelated file at that path.

### Qwen Code

The extension loads in a new Qwen Code session. Restart and run one tool call to verify it.

### QwenPaw

The adapter is a QwenPaw plugin: `anolisa adapter enable tokenless qwenpaw` and the bundled install script both run `qwenpaw plugin install <bundle> --force`, so QwenPaw copies the plugin into `<working dir>/plugins/tokenless/` and installs its `requirements.txt` into QwenPaw's own Python environment. That requirement is the `anolisa_tokenless` wheel from the matching GitHub Release, so the first install needs network access. QwenPaw only runs pip when `anolisa_tokenless` is missing from its interpreter's package metadata, so on an offline host `pip install` the wheel into QwenPaw's Python environment first; the same rule means an already installed older wheel is never upgraded by `plugin install`. The install script therefore checks, through the interpreter behind the `qwenpaw` command, that `anolisa_tokenless` imports and carries the SDK surface the plugin needs, and fails when no wheel matched the platform (`requirements.txt` lists Linux x86_64, Linux aarch64, and macOS arm64). The plugin itself refuses to register against an older wheel and logs the required release instead of failing at the first model call. The plugin requires the recovery entry points introduced in Tokenless 0.8.0. Install the SDK wheel matching the plugin release into QwenPaw's Python environment; the 0.7.14 wheel does not provide these APIs. The working directory is resolved like QwenPaw itself: `QWENPAW_WORKING_DIR`, else `COPAW_WORKING_DIR`, else an existing `~/.copaw`, else `~/.qwenpaw`. Without a `qwenpaw` command the install script prints a hint and exits 0 so `make setup` completes on hosts without QwenPaw.

A running QwenPaw hot-loads the plugin; otherwise start QwenPaw. Schema compression and the `tokenless_retrieve` tool apply from the next model call, and command rewriting runs after QwenPaw's approval step, so an approved `execute_shell_command` executes the rewritten command. Only QwenPaw's built-in tools are classified: `execute_shell_command` is command output, `read_file`, `recall_history`, `view_image`, and `view_video` are file content, and the remaining built-ins are API responses; skills, MCP tools, and tools added by later QwenPaw releases pass through untouched. QwenPaw's own tool-result pruning runs after Tokenless and keeps the head of each result (50000 bytes for the two most recent tool results, 3000 bytes for older ones, overflow written to `tool_results/`), so a recovery instruction at the end of a compressed result survives only while the result fits that budget; the omitted content stays retrievable from the stash with `tokenless retrieve`. Records land under `<workspace>/.tokenless` for each QwenPaw workspace; point `tokenless stats list --data-dir` there.

### WorkBuddy

WorkBuddy (Tencent CodeBuddy) shares one hook protocol across its product surfaces: the CodeBuddy Code CLI, WorkBuddy desktop (IDE) and WorkBuddy Enterprise all read a `hooks` key from the user-level `~/.codebuddy/settings.json`, following the Claude Code matcher-group shape. Recent CodeBuddy CLI releases also ship a plugin system, but `settings.json` hooks remain the only integration surface common to all three hosts, so the bundled lifecycle script merges the Tokenless hook groups there:

```bash
# After `make -C src/tokenless install` (or the RPM) staged the adapter resources:
make -C src/tokenless workbuddy-install
# or run the script directly:
bash ~/.local/share/anolisa/adapters/tokenless/workbuddy/scripts/install.sh
# remove again:
bash ~/.local/share/anolisa/adapters/tokenless/workbuddy/scripts/uninstall.sh
```

User-configured hooks and every other settings key are preserved; the uninstall script removes only the Tokenless-owned entries. Both scripts rewrite `settings.json` through a temporary file and never loosen the existing file mode, because the `.codebuddy` home may carry credentials (`settings.json.env` officially supports `CODEBUDDY_API_KEY` and auth tokens); a newly created `settings.json` defaults to `0600`.

The rewrite hook emits WorkBuddy's `modifiedInput` partial field override together with `permissionDecision: "allow"`, which the official PreToolUse contract requires for parameter changes to take effect (the contract's troubleshooting guidance says the tool keeps its original parameters under any other decision). Because `allow` bypasses the host permission prompt, the hook only emits it for attested rewrites. Protocol v2 moved the rtk run into the Tokenless Core, which deliberately reports Allow verdicts (rtk permission rules approved) and Ask/Default verdicts (unattested) identically to hooks, so the hook cannot prove the attestation. The WorkBuddy contract cannot combine `modifiedInput` with a confirmation prompt (`ask` would silently drop the change), so by default the hook passes the original command through unchanged and keeps the host's normal permission flow. Users who accept running rewrites without the host confirmation can set `TOKENLESS_WORKBUDDY_AUTO_ALLOW=1`; the hook then emits the rewrites with `allow`, and the decision reason records the bypass. Response compression replaces the tool result via `updatedToolOutput` when the hook runs under the CodeBuddy Code CLI. The CLI host is recognized through multi-signal classification in which every signal fails safe to the non-CLI path: `CODEBUDDY_FORCE_HEADLESS_BUNDLE` (set by WorkBuddy hosts before spawning the headless bundle since CLI 2.136.0) is positive hosted evidence; `CODEBUDDY_SESSION_KIND=daemon` marks the resident daemon worker (the Daemon Mode reference documents the variable as the worker type, and `daemon start` forks the resident child with `--serve` prepended); a CLI-binary ancestor (`codebuddy` / `codebuddy-code` / `cbc`) carrying a hosted sidecar flag (`--prewarm` / `--prewarm-force` / `--teammate-mode`) is a spawned headless process even in packages predating the marker; a CLI-binary ancestor free of these hosted signals is a standalone CLI. A controlling terminal is deliberately not required — the supported headless shapes (`-p` / `--print` for CI/CD and stdin pipelines, `--acp`, `--bg`, and the user-started `--serve` Web UI) legitimately run without a TTY and the CLI Hooks contract still honors `updatedToolOutput` there. `--serve` is deliberately not hosted evidence: the Web UI reference documents users starting `codebuddy --serve` directly, which is a standalone CLI session; the daemon child with an identical argv shape is separated by the daemon session kind instead. Session kinds other than `daemon` (`interactive` / `bg` / unset) are never treated as hosted evidence because the standalone CLI declares them for its own sessions (`codebuddy --bg` declares `bg`). An absent marker is never treated as proof of a standalone CLI: it only exists from CLI 2.136.0 on, while the declared support range starts at 1.16.0 and the earlier artifacts already ship the hosted modes; a pre-marker sidecar shape carrying none of the hosted signals above is treated as a CLI, which is benign because such a bundle executes the same CLI Hooks contract. `CODEBUDDY_PROJECT_DIR` cannot discriminate either because the IDE Hooks reference documents it for IDE hook scripts too (CLI hooks require CodeBuddy Code v1.16.0 or newer). The host is classified before any compression runs, so non-CLI hosts pay no compression latency and create no compression statistics or stash entries. The IDE and Enterprise surfaces document only the additive `additionalContext` for PostToolUse, which keeps the original tool result; compressing through it would grow the context instead of shrinking it, so on these hosts compression is disabled and only genuinely additive environment attribution is delivered. Restart WorkBuddy/CodeBuddy after installing or removing; the CodeBuddy CLI `/hooks` panel may ask you to review externally added hooks before they take effect.

The lifecycle scripts are staged by `make -C src/tokenless install`, the RPM, the npm postinstall copy, and the raw `anolisa install` component contract, whose `[[adapters]]` entry for `workbuddy` lays this adapter directory beside the other adapters. WorkBuddy still has no built-in anolisa driver and is not registered with `anolisa adapter enable` in this release; run the lifecycle scripts to install or remove the hooks.

`scripts/detect.sh` is read-only and reports adapter state with the same tri-state exit code as the other lifecycle adapters: `0` = WorkBuddy/CodeBuddy present and the Tokenless hooks installed; `1` = present but hooks not installed yet; `2` = prerequisites missing. A missing `~/.codebuddy` means WorkBuddy/CodeBuddy is not installed, so `detect.sh` reports `2`; `install.sh` treats the same condition as a graceful no-op and exits `0`, so lifecycle runs on hosts without WorkBuddy never fail. Missing `tokenless` or `rtk` binaries are also reported as missing prerequisites (exit `2`) in this release.

The RPM uninstall hook (`%preun`) runs as root, so it removes the hooks only from root's `~/.codebuddy/settings.json`. On multi-user hosts, other users must run `uninstall.sh` themselves.

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
