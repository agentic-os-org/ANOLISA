# Tokenless Troubleshooting

[中文版](../../../zh/token-saving/tokenless/troubleshooting.md)

First identify the failing layer: component installation, adapter integration, compression, statistics storage, or Stash retrieval. Do not begin by deleting configuration or reinstalling everything.

## Quick diagnostics

Run these in order:

```bash
tokenless --version
anolisa status tokenless
anolisa doctor tokenless
anolisa adapter status tokenless
tokenless stats status
tokenless env-check --all --json
```

When one command fails, resolve that layer before continuing. Preview the install plan without modifying the system:

```bash
anolisa --dry-run install tokenless
anolisa --dry-run --verbose install tokenless
```

Run adapter diagnostics as the user who owns the target Agent configuration and
adapter receipt. That user can inspect both user state and readable system
state:

```bash
anolisa doctor tokenless
```

## `tokenless: command not found`

A normal user install usually places the command in `~/.local/bin`. Check:

```bash
command -v tokenless
printf '%s\n' "$PATH"
ls -l ~/.local/bin/tokenless
```

If `~/.local/bin` is absent from `PATH`, add it according to the shell's startup-file rules and open a new terminal. Do not repeat a system install merely to solve a PATH problem.

npm users should also check:

```bash
npm prefix -g
npm list -g --depth=0 anolisa-tokenless
```

If npm logs say that optional dependencies were skipped, reinstall with:

```bash
npm install -g --include=optional anolisa-tokenless
```

Linux npm binaries support glibc only. musl systems such as Alpine require a Linux source build.

## Input and JSON errors

| Error | Cause | Resolution |
|-------|-------|------------|
| `No input provided` | No `--file` and stdin is a terminal | Use `-f <path>` or a pipe |
| `Input exceeds 64 MiB limit` | One input exceeds the cap | Split the input; do not bypass it by raising system memory limits |
| `JSON parse error` | Invalid JSON | Run `jq . < input.json` first |
| `Expected a JSON array for --batch mode` | `--batch` input is not an array | Remove `--batch` or fix the input structure |
| Output is still the original | Compression had no estimated saving | Normal behavior; inspect the stderr notice |

## No statistics appear after enabling the adapter

### 1. Verify the standalone CLI

```bash
printf '%s\n' \
  '{"status":"ok","debug":{"trace":"verbose"},"metadata":null,"data":{"items":[1,2,3]}}' \
  | tokenless compress-response

tokenless stats list --limit 5
```

If this also creates no record, check:

```bash
tokenless stats status
ls -ld ~/.tokenless
ls -l ~/.tokenless/stats.db
```

No record is written when compression has no savings. Use test input with removable or truncatable content.

### 2. Verify the adapter

```bash
anolisa adapter scan
anolisa adapter status tokenless
```

Confirm that:

- The target framework is detected.
- The Tokenless adapter is enabled.
- Adapter commands run as the user who owns the target framework configuration
  and adapter receipt.
- The agent CLI or IDE was restarted after enabling.

### 3. Verify the agent task

Run a task that actually passes through a hook, such as a shell command with visible output. Pure conversation, short responses, or a framework without the required hook may not create a record.

### 4. Check environment overrides

```bash
env | grep '^TOKENLESS_'
```

Confirm that `TOKENLESS_STATS_ENABLED=0` is not set unexpectedly and that any
custom database path remains under the real user home or selected data
directory.

## Schema compression produces no statistics

How schema compression plugs in depends on the host:

- **cosh and Cosh-NG** run it on the `BeforeModel` hook before every model call; the warnings in this section come from that hook.
- **OpenCode** runs it per tool definition through its `tool.definition` plugin hook, not through `BeforeModel`. MCP tools do not pass through that hook, so an MCP-only tool set produces no records there, and the `BeforeModel` warnings below never apply.
- **Qwen Code** ships a `BeforeModel` hook entry in the extension manifest, but current Qwen Code releases do not implement that hook event: the hook registry skips unknown event names, so only the other hook groups are registered and the schema hook never runs. Zero `compress-schema` records on Qwen Code are expected; this section cannot diagnose them.

When there are no `compress-schema` records on a host that actually runs the hook, check the following in order:

### 1. Confirm there is something to compress

Statistics only record invocations that save tokens; a result that is not smaller than the original is not recorded. Built-in tool descriptions are usually short (below the 256-character function and 160-character parameter truncation thresholds, with no `title` or `examples` to remove), so compression yields no savings and zero records are expected. Verify directly with the current tool declarations — replace the sample array below with your real declarations (a valid JSON array; do not keep any placeholder text, angle brackets, or surrounding quotes):

```bash
echo '[{"name":"example_tool","description":"A deliberately long example tool description that exceeds the 256-character truncation threshold so schema compression has something to remove. A deliberately long example tool description that exceeds the 256-character truncation threshold so schema compression has something to remove."}]' | tokenless compress-schema --batch
```

If stderr shows `did not reduce size`, the current tool set has nothing to compress; tool sets with long descriptions (for example some MCP tools) record normally.

### 2. Confirm the BeforeModel hook actually fires

On cosh and Cosh-NG, when a BeforeModel event carries nothing schema compression can work on, the hook emits one of the following warnings (each at most once per session) and passes the request through unchanged:

```text
[tokenless] WARNING: BeforeModel payload is not a JSON object ...
[tokenless] WARNING: BeforeModel payload carries no llm_request object ...
[tokenless] WARNING: BeforeModel event carries no tool declarations ...
```

The first warning means the hook received a payload that is not a JSON object; the second means the payload carries no `llm_request` object; the third means the host fires BeforeModel but its event format carries no tool declarations (`llm_request.config.tools` or `llm_request.tools`) — check or upgrade the host's hook protocol version. With neither a warning nor any records, BeforeModel is not firing at all:

- The extension or plugin is installed and enabled (`anolisa adapter status tokenless`).
- Hooks are not disabled in the host configuration.
- The host version supports the BeforeModel event.

Then continue with the generic steps in [No statistics appear after enabling the adapter](#no-statistics-appear-after-enabling-the-adapter).

## Adapter enable fails

Common causes:

- The target Agent product is not installed or detected.
- The framework version does not meet the adapter requirement.
- The adapter command ran as a different user from the one that owns the target
  framework configuration or adapter receipt.
- A directly installed Tokenless RPM has not been adopted into ANOLISA state.
- An npm installation has no anolisa component record, but `anolisa adapter enable` was used.
- OpenClaw security policy rejected the plugin's required unsafe-install override.

Start with:

```bash
anolisa adapter scan
anolisa --verbose adapter enable tokenless <framework>
```

For npm installations, use [Agent integration · Manual integration after npm installation](framework-integration.md#manual-integration-after-npm-installation).

For a directly installed RPM, create the missing state record, then rerun the
adapter command as the target framework user:

```bash
sudo yum install anolisa
sudo anolisa --install-mode system adopt tokenless
```

For an anolisa-managed installation, the first attempt does not bypass OpenClaw's safety scan. If the error specifically recommends it, review the findings and retry with:

```bash
anolisa adapter enable tokenless openclaw \
  --allow-unsafe-plugin-install
```

The npm/manual install script differs in *how* it consents, not *whether*: it adds `--dangerously-force-unsafe-install` automatically whenever the installer still advertises that option as effective, because the plugin launches fixed `tokenless` and `rtk` child processes. Hosts that mark the option a deprecated no-op (OpenClaw 2026.6.5+) never receive it — there the safety scan is decided by `security.installPolicy`, so a rejection must be resolved by the operator relaxing that policy, not by re-running the script. Review the adapter and policy; do not enable it where that override is prohibited.

## QwenPaw install reports an unpublished SDK wheel

The QwenPaw bundle installs the native Python SDK from a GitHub Release asset
pinned to the package version, so the wheel and the RPM always carry the same
version. When a version bump reaches `main` before the matching
`tokenless/vX.Y.Z` release is published, that asset does not exist yet and the
installer stops before handing anything to QwenPaw:

```text
[tokenless] The Tokenless 0.8.2 Python SDK wheel is not published yet (HTTP 404):
[tokenless]   https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.2/anolisa_tokenless-0.8.2-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
[tokenless] Release `tokenless/v0.8.2` that carries the wheel assets does not exist.
```

This is a release-orchestration gap, not a broken host. Confirm it by requesting
the URL from the message directly — `404` means the release is missing, `200`
means the wheel exists and the failure has another cause:

```bash
curl -sIL -o /dev/null -w '%{http_code}\n' "<wheel URL from the message>"
```

- Maintainers: push the `tokenless/vX.Y.Z` tag and approve the `release`
  environment so the publish workflow uploads the wheel assets, then rerun the
  installer.
- Everyone else: install a Tokenless package whose version already has a
  published release.
- Offline or mirrored networks, where the probe cannot reach GitHub but pip
  resolves the wheel from a local mirror: rerun the installer with
  `ANOLISA_SKIP_WHEEL_PREFLIGHT=1`. `ANOLISA_TOKENLESS_PROBE_TIMEOUT` bounds
  each probe in seconds (default 15).

The probe is advisory. Without `curl` or `python3`, when the release host is
unreachable, or when it answers anything other than `404`, the installer leaves
the verdict to pip and reports whatever pip reports.

## A command is not rewritten

RTK does not have a rewrite rule for every command. Test it directly:

```bash
rtk rewrite "ls -la"
```

If `rtk` is missing:

```bash
command -v rtk
```

If RTK works directly but not in the agent, inspect the framework support matrix, adapter status, and whether the session was restarted.

`TOKENLESS_COMPRESSION_ENABLED=0` does not disable rewriting. Disable the adapter, or set OpenClaw's `rtk_enabled=false` when using that plugin, if the original shell input must be preserved.

## Tool Ready still reports `NOT_READY`

The current build hard-disables Tool Ready and cannot emit `NOT_READY` or block a tool. Confirm the active binary:

```bash
tokenless --version
tokenless env-check --tool <name> --json
```

The JSON result should contain `"status":"UNKNOWN"` and `"enabled":false`. A `NOT_READY` result indicates a mixed or stale deployment. Update both the Tokenless binary and shared adapter resources, then restart the agent. Setting the former `TOKENLESS_TOOL_READY_ENABLED` variable has no effect.

## Database errors

### `Failed to open database`

```bash
ls -ld ~/.tokenless
ls -l ~/.tokenless/stats.db*
env | grep -E 'TOKENLESS_(DATA_DIR|STATS_DB|STASH_DB)='
```

Confirm that the current user can write the selected data directory and database. `TOKENLESS_DATA_DIR` may be outside the real home, but it must be an absolute non-root directory without parent traversal. An invalid explicit data directory does not fall back to home. `TOKENLESS_STATS_DB` and `TOKENLESS_STASH_DB` must remain under the real home or selected data directory; the bundled RTK writer applies the same rule.

Do not share one `stats.db` between users. AgentSight and Tokenless should run so that they can access the same user's database.

### No SLS JSONL record

```bash
tokenless stats status
test -e /var/log/anolisa/sls/ops/tokenless.jsonl
```

SLS is enabled by default, but Tokenless does not create the target file. A missing file causes a silent skip. A custom path must be under `/var/log/` or `/tmp/`.

## `retrieve` is empty or fails

Check that:

1. The hash contains all 24 hexadecimal characters.
2. Compression did not use `--no-stash`.
3. Compression was active rather than dry-run.
4. The one-hour default TTL has not passed and the 10,000-entry capacity did not evict it.
5. Compression and retrieval use the same user and database path.
6. Compression stderr did not report a Stash write failure.

```bash
ls -l ~/.tokenless/stash.db*
env | grep '^TOKENLESS_STASH_DB='
```

Retry with the same database explicitly:

```bash
tokenless retrieve <hash> --stash-db ~/.tokenless/stash.db
```

Expired or never-successfully-written content cannot be recovered.

## Statistics exist but the prompt is not smaller

First check the framework's response-delivery path in the
[support matrix](framework-integration.md#agent-adapter-support-matrix). Qoder replaces output
through `updatedToolOutput`. Qwen Code and legacy Copilot Shell pass post-tool output through because
their current contracts provide no replacement field; compressed copies are not injected through
`additionalContext`. Codex also avoids response compression and limits its PostToolUse context to
classified environment failures. Measure Codex savings on RTK-rewritten shell calls.

For Claude Code, response replacement requires version 2.1.121 or later. Older or unrecognized
versions pass the original through. OpenClaw optimizes supported persisted results when
`post_tool_enabled` is on. Tokenless automatically chooses JSON cleanup or TOON when it produces a
smaller valid result.

## Qoder plugin cache issue

Use this section only when an upgrade produces:

```text
python3: can't open file '/rewrite_hook.py'
```

Refresh the adapter:

```bash
anolisa adapter disable tokenless qoder
anolisa adapter enable tokenless qoder
```

Confirm that the cache has no unexpanded placeholder:

```bash
grep -R -n 'QODER_TOKENLESS_HOOKS' \
  ~/.qoder/plugins/cache/local/tokenless*/*/hooks.json 2>/dev/null
```

No output is expected. Fully exit and restart Qoder IDE afterwards.

## anolisa and RPM state disagree

If `dnf remove` or `rpm -e` was run directly:

```bash
sudo yum install anolisa
sudo anolisa --install-mode system repair tokenless
```

Follow the repair plan. Only when the RPM is still present and the output explicitly asks to recreate the record, run:

```bash
sudo anolisa --install-mode system forget tokenless
sudo anolisa --install-mode system adopt tokenless
```

`forget` deletes only anolisa state; it does not uninstall the RPM.

## Upgrade and uninstall

### anolisa installation

Upgrade:

```bash
anolisa update tokenless
anolisa adapter status tokenless
anolisa doctor tokenless
```

For system mode:

```bash
sudo anolisa update tokenless
```

Restart enabled agents after upgrading. Adapters normally do not need to be re-enabled. If status reports inconsistent resources, follow the diagnostic result before disabling and enabling again.

Before uninstalling, list and disable every adapter:

```bash
anolisa adapter status tokenless
anolisa adapter disable tokenless <framework>
anolisa uninstall tokenless
```

Use the same scope for system mode. In the current release, `--purge` only supports plan preview through `anolisa --dry-run uninstall --purge tokenless`; without `--dry-run`, it returns `NotImplemented` and does not uninstall the component or remove configuration, cache, or state. Use `anolisa uninstall tokenless` for an actual uninstall, and see [Clear data](configuration-and-privacy.md#clear-data) for local databases.

### npm installation

Upgrade:

```bash
npm install -g anolisa-tokenless@latest
```

npm refreshes adapter resources, but a plugin registered with a framework may still be an older copy. Run the target framework's `scripts/install.sh` again and restart the framework.

Uninstall in this order:

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/uninstall.sh
npm uninstall -g anolisa-tokenless
```

After confirming that every npm-managed adapter was uninstalled, remove the resource copy from the user data directory:

```bash
rm -rf -- ~/.local/share/anolisa/adapters/tokenless
```

Run this only after confirming that the directory belongs to this Tokenless npm installation. A manually installed cosh Extension must be separately confirmed and removed from `~/.copilot-shell/extensions/tokenless`.

### YUM/RPM installation

Prefer management through the anolisa system scope. If anolisa does not own the installation record, disable adapters first, then run:

```bash
sudo yum update tokenless
sudo yum remove tokenless
```

Upgrade or removal does not automatically clear Tokenless runtime databases under the user home.

## If the issue remains

Before sharing the following output, inspect and remove sensitive content:

```bash
tokenless --version
anolisa --version
anolisa doctor tokenless
anolisa adapter status tokenless
tokenless stats status
tokenless env-check --all --json
```

Do not attach `stats.db`, `stash.db`, or unreviewed `tokenless stats show` output.
