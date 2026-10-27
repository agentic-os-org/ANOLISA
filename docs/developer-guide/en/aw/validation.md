# Validate native tool behavior

[中文版](../../zh/aw/validation.md)

Use allow, block and failure cases to verify that native Agents adopt AW policy results.

## Use one configuration for four frameworks

The [shared YAML](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/aw.shared.yaml)
and [boolean script](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/check-tool.py)
give all four targets the same before-block and after-observe policy. Follow the
[walkthrough](integration.md) to copy them, replace
absolute paths and verify the Provider. The commands here use its `AW_BIN`,
`AW_CONFIG` and `AW_DEMO` variables. Installed-package users can use the same
files with `AW_BIN` pointing to the installed binary; Python 3 is the sample
script's dependency.

Prepare each Agent's supported version and native model configuration first.
Only the selected target must be installed to run it. Use separate native
profiles, choose their tool working directory explicitly and start from
`$AW_DEMO/workspace`. The shared YAML changes neither model credentials nor
native permissions. Test one target at a time initially.

| Target | Native prerequisite | Launch detail |
| --- | --- | --- |
| Qoder | CLI >=1.1.64 with native command Hooks, completed login | Interactive or print; optional `--native-settings` |
| OpenClaw | 2026.9.6, configured JSON and existing state directory; stop any Gateway using that profile | AW starts a new `gateway run`; pass both native paths |
| QwenPaw | 2.2.2.post1 / AgentScope 2.0.10, initialized App working directory and model | Set `QWENPAW_WORKING_DIR`; use App/API, not ACP/TUI |
| Hermes | Clean official revision `952c941e`, installed Python console script and initialized local chat profile | Install the AW plugin once into that profile; pass the same profile when running |

Launch Qoder:

```bash
cd "$AW_DEMO/workspace"
"$AW_BIN" run --config "$AW_CONFIG" --agent qoder
```

Launch OpenClaw with your existing absolute native paths:

```bash
"$AW_BIN" run --config "$AW_CONFIG" --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

Launch QwenPaw with your initialized directory:

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home "$AW_BIN" run \
  --config "$AW_CONFIG" --agent qwenpaw -- --host 127.0.0.1 --port 8096
```

Install and launch Hermes after reading its [profile requirements](agents/hermes.md):

```bash
"$AW_BIN" install --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
"$AW_BIN" run --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
```

For OpenClaw and QwenPaw, keep the server terminal open and wait for AW's native
Hook readiness message before sending requests through that Gateway or App.
Use OpenClaw's connected client, not `agent --local`; use QwenPaw's App UI/API,
not another CLI entrypoint. Qoder and Hermes use the launched native chat.
Each [Agent guide](native-agents.md) explains coexistence and lifecycle scope.

### Verify native behavior

In another terminal, restore `AW_BIN`, `AW_CONFIG` and `AW_DEMO` from the
walkthrough. Set `AW_TARGET` to the target being tested. Start with absent marker
files; for a repeat, use a fresh name. Print the following two prompts and send
them as separate turns through that target's native interface:

```bash
export AW_TARGET=qoder
printf 'Use your shell tool exactly once to run `printf AW_OK > "%s/%s.allowed"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
printf 'Use your shell tool exactly once to run `printf AW_DENY_TEST > "%s/%s.blocked"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
```

These commands only write demo markers if executed. The second input contains
the policy's deny marker. After the turns, check:

```bash
cat "$AW_DEMO/workspace/$AW_TARGET.allowed"
test ! -e "$AW_DEMO/workspace/$AW_TARGET.blocked"
"$AW_BIN" status --config "$AW_CONFIG"
ls "$AW_DEMO/state/journal"
```

Expect `AW_OK`, an AW policy denial for the second turn, and no blocked marker.
Read the relevant journal through AW: select a filename from the listed
directory and use its 64-character name without `.jsonl` as `AW_AUDIT_KEY`.

```bash
export AW_AUDIT_KEY=REPLACE_WITH_EVENT_KEY
printf '{"method":"audit","key":"%s"}\n' "$AW_AUDIT_KEY" | \
  "$AW_BIN" request --socket "$AW_DEMO/state/aw.sock"
```

Files include preparation and event journals. For an event, inspect
`result.records[0].record.plan`: `kind` is `event`, and its instance and binding
must match the `AW instance` reported at launch.
The before-denial call should have `policy_block: true`; the completed after
step should name Provider `check-tool` and have `status: ok`. Audit stores
metadata, not raw inputs/results or effect reason text. A closed event has
`terminal: true`. The following checks complete the acceptance record:

| Check | Evidence |
| --- | --- |
| Allowed tool | Tool ran, allowed marker exists, before and supported after steps succeeded |
| Policy block | AW returned a denial, blocked marker absent, before audit records a policy block |
| Provider failure | After stopping the old sessions/service, launch a copied config with an absent before-script path; the invocation fails, `on_error: block` prevents the marker, and audit reports failure rather than a policy hit |
| Coexistence | Keep one existing native Hook/plugin enabled; confirm its expected behavior still occurs alongside AW |
| Restart | Exit Agents, stop AW, relaunch with the original config; new tools are checked and old audit keys remain readable |

Record framework version, AW commit, actual commands and audit keys. Synthetic
Host tests and startup readiness alone do not complete this table. Model
refusal, missing tool invocation or native permission denial is not an AW block.
After-event coverage differs by framework: for example, Qoder reports successful
tools only. Do not require a fabricated after event for every rejected call.

### Stop and clean up

Exit Qoder/Hermes and use Ctrl-C in the owned OpenClaw/QwenPaw server terminals.
Wait for those processes to exit, then stop the shared AW service:

```bash
"$AW_BIN" stop --config "$AW_CONFIG"
```

The stop response acknowledges the request; wait for the recorded daemon PID
to exit and its `aw.sock` to disappear. QwenPaw removes its owned temporary
plugin on normal exit. Hermes keeps its installed plugin: ordinary Hermes
launches without an AW binding do not activate AW callbacks. To undo installation,
stop Hermes and native config writers, inspect the reported `backup` and
`displaced` files, move `plugins/aw-native-hooks` outside the profile's `plugins`
directory and restore the chosen pre-install configuration. Preserve any later
intentional native changes instead of blindly overwriting them. AW has no
native-plugin uninstall command.

Retain audit evidence before deleting the demo directory, following the
[walkthrough's cleanup](integration.md#4-clean-up).
Framework profiles remain separate. Package removal is covered by the
[package reference](reference/packages.md).
