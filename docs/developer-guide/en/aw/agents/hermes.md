# Hermes

[中文版](../../../zh/aw/agents/hermes.md)

## Prepare

The verified baseline is commit `952c941e741e922a9be8fc403c8944c6e96318bb`. AW records
the actual checkout revision and accepts other revisions with the native profile,
plugin and shell-hook interfaces. Keep tracked files clean and retain the checked
installation and loader environment. Install the Python console script with an absolute Python shebang; the entrypoint is local `chat`.
Build AW with the [tutorial](../integration.md), copy the [Hermes configuration](../../../../../src/aw/crates/aw-service/examples/aw.hermes.yaml), and set argv to the installation's absolute path.
Initialize the profile and model credentials with Hermes first.

## Install the plugin and launch

```bash
cd src/aw
target/debug/aw agent connect --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

connect backs up configuration, installs the AW plugin and reports `backup` and `displaced` paths. Retain both files.
Use the same profile for launch. Wait for Hook readiness, then [validate native behavior](../validation.md).

## Deployment constraints

| Item | Requirement |
| --- | --- |
| Entrypoint | Python console script with local chat; no shell wrapper, `python -m`, TUI, Gateway, ACP or session/profile/worktree switching |
| Paths | Working directory, profile, venv, checkout and AW paths must be UTF-8; profile and `.env` must be current-user-owned and protected from other writers |
| Git environment | Unset `GIT_DIR`, `GIT_COMMON_DIR`; use absolute HOME/XDG and Git configuration paths; filter and fsmonitor helpers are rejected |
| Native options | Safe mode and chat help flags are rejected before integration; use `hermes chat --help` directly |
| Tool arguments | AW before routes reject Hook/middleware input rewriting, including calls inside `execute_code` |
| Budget | At most 55000 ms per event, within the native callback deadline |

Other native callbacks may still run after before denial; after can observe failed or blocked attempts.
Forced plugin reload during tool execution is rejected with AW callbacks configured. Empty policy still requires plugin installation and registers zero tool callbacks.

## Upgrade and cleanup

For plugin mismatch, stop Hermes and configuration writers, move `<profile>/plugins/aw-native-hooks` outside plugins as a backup, then run connect again.
To roll back, inspect `backup` and `displaced`, restore selected configuration and retain intentional later edits. AW has no Hermes plugin uninstall command.
Stop AW after exiting Hermes. Inspect reported paths on publication or cleanup failure; preserve unknown files.
See the [Hermes contract](../../../../../src/aw/docs/design/en/adapters/hermes-adapter.md) for probes, atomic publication and conflicts.

## Skill trust

Enable `check_skill` on `tool.before` with selector `native:hermes:skill_view`,
using the [shared security Provider](../providers/sec-core.md#validate-skills-before-loading).
AW resolves the native registry and local directory selection, checks the canonical
Skill directory, then supplies the checked manifest to native rendering. Native
disabled, platform and quarantine decisions remain effective.

This supports the main `SKILL.md` of directory-based `skill_view` calls. Plugin-qualified
sources, legacy flat files and `file_path` references return resolution errors;
`on_error: block` denies their loading. Slash commands, preload, direct file reads
and inline Skill commands need their own policy boundaries.

## Check user submissions

Use the [submission configuration](../../../../../src/aw/crates/aw-service/examples/aw.hermes-submission.yaml), set the Hermes, sec-core Provider, CLI and socket paths, then install the plugin with the same profile. It runs `scan_prompt` and `scan_user_input` before the quiet single-query CLI constructs its Agent:

```bash
aw run --config /absolute/aw.hermes-submission.yaml --agent hermes \
  --native-profile /absolute/hermes-profile -- --query 'Summarize the current task.'
```

The configured argv includes `chat --oneshot --quiet`. `--query-file /absolute/prompt.txt` can replace `--query`. A policy denial exits the CLI with code 2 and zero model calls; the checked text never enters the native conversation. Callback faults follow `on_error`, and pending scanners are closed before dispatch or rejection. Inputs must be nonblank UTF-8 text and fit within 256 KiB of encoded callback data.

Enabling `input.submit` requires a quiet query and rejects interactive chat or `--image`. This boundary checks the text after native query-file parsing and before image routing; later native context or Skill expansion retains its separate contract. Native API request plugins remain responsible for their own rewrites. Without submission steps, the existing native chat options remain available. Python factories, gateway sessions and interactive `/steer` are outside this CLI entrypoint.

## Prepare native startup and update policy

After connecting the plugin, install the startup module in the explicitly
selected, dedicated Hermes virtual environment. The example uses Python 3.14;
select the existing `site-packages` path for that environment. AW creates three
new files and refuses to replace existing startup assets.

```bash
aw instance setup --config /absolute/aw.yaml --agent hermes \
  --directory /absolute/hermes-venv/lib/python3.14/site-packages \
  --native-profile /absolute/hermes-profile > hermes-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("hermes-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
/absolute/hermes-venv/bin/hermes chat --cli
```

Use the same native arguments in the configured target. Argument mismatches
abort before native dispatch. Restart once after preparation. The startup module retains native main and reuses AW's registration,
argument-integrity and completion guards in that process. Version and management
commands remain native. Preparation failure prevents chat dispatch.

```bash
aw instance update --config /absolute/aw.yaml --agent hermes
```

Updates activate between confirmed completed chat turns. The native request
includes model work, tool execution and callback cleanup. Cancellation returning
no reply, exceptions or unfinished callbacks retain the busy gate; restart the
process after resolving the failure. Budgets for this update path are at most
12,000 ms, and the native plugin callback timeout must be at least 17 seconds
(or explicitly unlimited).

Tool observation, denial and result replacement use stable routes. Quiet
single-query text mode also reserves admission before `input.submit` and keeps
that admission through the chat turn; interactive targets reject submission
policy additions. Skill checking is currently rejected on the update path;
use the ordinary `aw run` Skill integration when that capability is required.

To remove native-start preparation, stop the dedicated environment's Hermes
processes, then remove exactly the three files listed by `setup`:
`aw-native-bootstrap.pth`, `aw_native_startup.py` and
`aw-native-bootstrap.json`. Keep other environment and plugin files. Plugin
rollback follows the existing [cleanup procedure](#upgrade-and-cleanup).
