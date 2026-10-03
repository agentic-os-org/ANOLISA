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

## QwenPaw install reports an unavailable SDK wheel

The QwenPaw bundle installs the native Python SDK from a GitHub Release asset
pinned to the package version, so the wheel and the RPM always carry the same
version. When that asset cannot be downloaded — most often because a version
bump reached `main` before the matching `tokenless/vX.Y.Z` release was
published — the installer stops before handing anything to QwenPaw:

```text
[tokenless] The Tokenless 0.8.2 Python SDK wheel asset is unavailable (HTTP 404):
[tokenless]   https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.2/anolisa_tokenless-0.8.2-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
[tokenless] This package was built from a source tree already at 0.8.2, but that asset
[tokenless] cannot be downloaded. A maintainer must check whether the GitHub Release
[tokenless] `tokenless/v0.8.2` exists:
```

A `404` for that URL proves only that this asset cannot be downloaded, not that
the release is absent: an interrupted asset upload leaves an existing release
without the wheel for this architecture. Request the URL from the message
directly to confirm the `404` — `200` means the wheel is there and the failure
has another cause:

```bash
curl -sIL -o /dev/null -w '%{http_code}\n' "<wheel URL from the message>"
```

- Maintainers, the `tokenless/vX.Y.Z` release does not exist: push the tag and
  approve the `release` environment so the publish workflow uploads the wheel
  assets, then rerun the installer.
- Maintainers, the release exists but this wheel is missing: the upload was
  incomplete. The publish workflow refuses to overwrite an existing release, so
  delete that release and run the workflow again — or upload the missing asset
  to it — then rerun the installer.
- Everyone else: install a Tokenless package whose version already has a
  downloadable wheel.
- Offline or mirrored networks, where the probe cannot reach GitHub but pip
  resolves the wheel from a local mirror: rerun the installer with
  `ANOLISA_SKIP_WHEEL_PREFLIGHT=1`. `ANOLISA_TOKENLESS_PROBE_TIMEOUT` bounds
  each probe in seconds (default 15).

The probe is advisory. Without `curl` or `python3`, when the release host is
unreachable, when a probe outlives `ANOLISA_TOKENLESS_PROBE_TIMEOUT`, or when
the host answers anything other than `404`, the installer leaves the verdict to
pip and reports whatever pip reports.

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

`TOKENLESS_COMPRESSION_ENABLED=0` does not disable rewriting. Rewriting is disabled by default. Set `TOKENLESS_RTK_ENABLED=0` in the host environment to override any explicit SDK/plugin opt-in and preserve the original shell input.

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

The package postinstall makes that confirmation for you: `~/.local/share/anolisa/adapters/tokenless` is shared with the anolisa CLI, so when it already belongs to a managed component install the postinstall keeps it unchanged and prints where the resources inside the package are instead of replacing a tree that a component record and framework registrations still point at. Pass `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1` to take the directory over anyway, in which case re-run `anolisa adapter scan` afterwards so the component record matches what is on disk.

Ownership there has to be proven rather than assumed. The postinstall only refreshes a tree carrying the marker it or the standalone installer wrote (`.tokenless-owner`); a tree left by a release from before the marker existed, or one somebody copied by hand, carries none and is kept as well — nothing about its content says who put it there, and the framework registrations pointing into it would dangle otherwise. The one-time cost is that upgrading from such a version keeps the older resources until the directory is removed or the override is used.

The uninstaller also stops short rather than half-finishing. When a framework registration cannot be removed, the adapter resources *and* the receipt are both kept and the script exits non-zero, so no registration is left pointing at a deleted directory and the script the warning names still exists; fix the framework and re-run to complete the removal. The installer is fail-closed the same way: it moves the previous install aside before replacing it, and if that copy cannot be made — no staging directory, or a recorded file it cannot read — it stops before writing anything.

### curl standalone installation

The standalone installer records every path it created in a receipt at `~/.local/share/tokenless/install-receipt`: the method it ended up taking (npm or source build), the version, the install directory, the npm prefix, the adapter directory, the rc file it appended a PATH line to, and each installed file together with its sha256.

Upgrade by re-running the installer. It overwrites the recorded paths and rewrites the receipt, so the record stays accurate:

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

Switching method on the same machine — for example re-running with `TOKENLESS_FORCE_BUILD=1` after an npm install — retires what the previous method created, so no `rtk` launcher, npm global package or adapter tree survives that the new receipt no longer mentions. Nothing is retired until the replacement has been verified: the previous install is moved aside first and put back if the new one fails, so a missing tag, a failing build or an unwritable directory leaves the working CLI and its receipt exactly as they were. A recorded path whose recorded identity no longer matches was taken over by another installer and is left alone.

An npm-to-npm upgrade is covered the same way. It replaces the package payload in place and the launcher links resolve into that payload, so a copy of the previous module directory, its `@anolisa` platform package and the prefix's own bin links is kept until the new CLI has been verified; a broken new binary is put back rather than left behind a launcher that still resolves to it.

The new receipt is what makes the retirement safe, so it is written first: to a temporary in the same directory, moved into place, then flushed. If it cannot be written and the previous receipt cannot be removed either, the run fails and puts the previous install back — a stale receipt describing an install that was already replaced would let a later `scripts/uninstall.sh` delete the new one, because a same-version reinstall reproduces the recorded digests and link targets. When the stale receipt *can* be removed the install still succeeds, with a warning that scripted uninstall is unavailable.

Where the previous npm prefix and the install directory overlap — `npm install -g --prefix ~/.local` puts its bin links in `~/.local/bin`, the installer's default install directory — that package is retired by hand instead of through `npm uninstall --prefix ~/.local`, which would remove the CLI the new install just placed there. An npm attempt that fails part-way is rolled back the same way, so the source-build fallback never inherits an unowned package, launcher link or adapter tree.

Ownership is checked, not just content. A newer anolisa or npm install of the same version reproduces byte-identical binaries and manifests, so the receipt also records this install's id, the link target each launcher resolves to, and an ownership marker (`.tokenless-owner`) inside the adapter tree and the npm module directory. The uninstaller keeps anything whose recorded identity or marker no longer matches — files, adapter resources, framework registrations and the npm package alike.

The source-build fallback is Linux-only. On macOS the installer either takes the npm path or exits with an error; it never runs `cargo`. Intel macOS has no published npm package either, so it currently has no supported route — see the platform table in the [Quick Start](QUICKSTART.md#platform-support).

`~/.local/share/anolisa/adapters/tokenless` is shared with the anolisa CLI and with a direct `npm install -g`. When that directory already belongs to one of them, the npm postinstall leaves it untouched, the installer compares it against the snapshot it took beforehand and only puts the snapshot back if something really did replace it, records no adapter directory, and says so. The uninstaller then leaves those resources and the framework registrations pointing at them alone. A snapshot that cannot be taken at all stops the run before `npm install -g` replaces anything.

Restart the agent afterwards. When the run took the npm path, the plugin registered with a framework may still be an older copy — run that framework's `scripts/install.sh` again as described in [npm installation](#npm-installation).

Uninstall with the matching script, which removes only what the receipt records:

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/uninstall.sh | bash
```

Preview the plan first, or also drop the collected statistics:

```bash
bash src/tokenless/scripts/uninstall.sh --dry-run
bash src/tokenless/scripts/uninstall.sh --purge
```

`--dry-run` prints what would be removed and changes nothing. `--purge` additionally deletes the runtime data directory `~/.tokenless`, which holds `stats.db` and `stash.db`; without it that data is kept. `--receipt <path>` reads a non-default receipt and mirrors `TOKENLESS_RECEIPT`.

The uninstaller stops short rather than half-finishing, and says so by exiting non-zero. A framework registration it could not remove keeps the adapter resources *and* the receipt, because deleting the resources would leave that registration pointing at nothing. A global npm package it could not remove — because npm is not on PATH — keeps the package, the launcher links under the recorded prefix and the receipt: Tokenless still runs from that prefix, and without the receipt there would be no record of the prefix or its owner, so a re-run could not finish the job. Fix the cause and re-run; the receipt is what makes the retry possible.

Re-installing into a different `TOKENLESS_INSTALL_DIR` also retires the PATH entry the installer appended for the previous directory. The receipt names one rc file and one directory, so without that the block for the old directory would survive every uninstall.

What gets removed depends on the recorded method. After an npm path, the script removes the recorded launcher binaries from the recorded install directory — including a custom `TOKENLESS_INSTALL_DIR` — runs `npm uninstall -g anolisa-tokenless` against the recorded prefix, and removes the adapter resource copy only when that npm run created it — running each bundled framework's own `scripts/uninstall.sh` first, so an enabled OpenClaw, Hermes or Qwen Code registration is removed instead of being left pointing at a deleted directory. After a source build it removes only the `tokenless` CLI, because that path installs no `rtk` and no adapter resources. Either way a neighbouring anolisa CLI or manual npm installation that shares the same directory survives.

Do not substitute a fixed `rm -f ~/.local/bin/tokenless ~/.local/bin/rtk` list. It misses a custom `TOKENLESS_INSTALL_DIR` and the npm global package, and after a source-build install it deletes `rtk` and adapter resources that install path never created.

Without a receipt the uninstaller refuses to guess and prints the per-method manual steps instead. Uninstall through the method you actually used: [anolisa installation](#anolisa-installation), [npm installation](#npm-installation), or the YUM/RPM sequence below.

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
