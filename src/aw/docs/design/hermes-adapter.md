# Hermes native Hook adapter

[中文版](hermes-adapter_zh.md)

The Hermes adapter connects an existing local profile to the AW service through
Hermes' native plugin and shell Hook APIs. It targets the official
`NousResearch/hermes-agent` revision
`952c941e741e922a9be8fc403c8944c6e96318bb` and the Python `chat` entrypoint on Linux.
Gateway, ACP, Desktop and the separate TUI entrypoint remain unsupported.
AW inserts native `--cli` so ambient TUI preferences cannot change that entrypoint.
Only complete supported chat options are accepted; profile changes, worktree and
resume switches, plugin-disabling flags and native option abbreviations are rejected.
Profiles that redirect the CLI through `.container-mode` are also rejected.
The version probe allows up to 60 seconds because this Hermes revision performs
its own update check during `--version`. AW does not change that native setting
or retry a failed probe. The reported installation directory is checked against
the full Git commit; a changing `origin/main` banner is not used as the version.

## Installation and launch

Hermes reads `config.yaml` from its profile directory. This revision has no
temporary configuration overlay and only loads enabled plugins. AW therefore
uses an explicit installation step:

```bash
aw install --config aw.yaml --agent hermes --native-profile /absolute/profile
aw run --config aw.yaml --agent hermes --native-profile /absolute/profile -- chat
```

The configured Agent `argv` supplies the installed Hermes executable. Put `chat`
and its arguments either in `argv` or after the launcher's `--`, once. The
[sample configuration](../../crates/aw-service/examples/aw.hermes.yaml) puts
`chat` in `argv`.

Installation writes the bundled plugin to `plugins/aw-native-hooks`, adds that
name to `plugins.enabled`, and removes its matching `plugins.disabled` entry.
Unknown configuration values and existing plugin selections remain intact. The
first configuration update saves the exact original bytes in a private
`config.yaml.aw-backup-<id>` file. The official `hermes config set` command updates
only the required plugin selection fields in an owned private staging directory.
Its native YAML 1.1 writer preserves unknown values, comments and quotes. AW
compares the live file with the original bytes before atomically replacing it;
a failed command or concurrent edit leaves the live configuration untouched.
Staging contains only the configuration candidate, without profile credentials
or history, and is removed after installation. Each writer command has a
30-second deadline. Repeating installation
with unchanged files is a no-op. A different plugin under the same name,
symlinked installation files, malformed plugin lists, or an occupied installation
lock cause a visible failure.

Installation does not invoke `hermes plugins enable`: that command can hot-load
plugins into an existing Gateway or Desktop process. AW only updates local files.
It never copies or rewrites the profile's `.env`, `auth.json`, databases, memory or
sessions. Normal Hermes operations can still update their own state.

Launch requires the matching installed plugin to be enabled. It keeps the
selected `HERMES_HOME`, pins the equivalent native `--profile` selector so a
sticky `active_profile` cannot redirect the process, and retains the launch
working directory. The installed plugin is inert during ordinary Hermes launches
without the AW launch environment variable.

## Hook semantics

Each admitted AW step becomes a separate native shell Hook callback registered
with `PluginContext.register_hook`. Hermes serializes callbacks in registration
order and retains control over parallel or sequential tool batches. Existing
configured Hooks still run; AW does not import or reorder their configuration.
Native Hook configuration has no per-Hook environment object in this pinned
revision.

Raw commands receive the actual callback process environment, including variables
loaded from the profile's `.env` and Hermes' native HOME/TMPDIR settings. Structured
Providers retain the environment captured at launch binding. AW does not put
environment contents into normalized events or audit records.

| Native event | AW event | Available behavior |
| --- | --- | --- |
| `pre_tool_call` | `tool.before` | Structured observe/block; native commands retain native stdout and exit semantics |
| `post_tool_call` | `tool.after` | Observation, including blocked, failed, cancelled and timed-out tool outcomes |

The native payload supplies `session_id`, `extra.api_request_id` and
`extra.tool_call_id`. The normalized call ID hashes the request and tool-call ID
together, so a model can reuse a call ID in a later request. AW rejects
missing identities, preserves arbitrary tool names and object arguments, and
keeps `extra.result` in its native type, including serialized JSON strings. No
generated fallback replaces a missing native identity.

Hermes parses a native `action: block` response or exit code 2 as a veto. A veto
does not prevent later registered before callbacks from running. Native
`action: approve` requests human approval; it never means automatic allow.
Structured AW `ask` remains unsupported. Native `modify` continues to use
Hermes' own response parser; AW does not create a portable rewrite contract.
Post callbacks cannot undo the tool or replace its result.

Both the AW event deadline and Hermes' callback timeout apply. Launch refuses
event budgets whose native command timeout and cleanup allowance exceed
`plugins.hook_callback_timeout`. A private readiness receipt records successful
registration before the launcher reports native Hook readiness. This is
same-user installation evidence, not a final or protected security boundary;
Hermes can continue after plugin-load failures before the launcher observes a
missing receipt.

## Verification

Rust tests cover profile installation, byte-exact backups, idempotence, unknown
configuration values, collision and symlink rejection, event identities and
entrypoint restrictions. The optional native tests use the pinned official
Hermes Python environment with temporary profiles:

```bash
python src/aw/crates/aw-service/tests/fixtures/hermes/native_contract.py \
  --source /absolute/hermes-agent \
  --plugin /absolute/anolisa/src/aw/adapters/hermes
```

`live_cli.py` exercises official `chat --oneshot` with a deterministic local
OpenAI-compatible endpoint, a real AW daemon and the sample Provider. It runs in
the pinned Hermes Python environment with `--source` selecting that checkout. It checks
allow/block adoption, existing relative-path Hooks, before/after commands,
retained profile state, native YAML scalar meanings, callback environment,
instance release and cleanup. It uses no real model key
and does not certify a hosted model or interactive human approval.

## Rollback

Stop sessions using this adapter before restoring the exact backup path returned
by `aw install`. Restore that file over the same profile's `config.yaml`, then
remove only `plugins/aw-native-hooks` and `.aw-install.lock` created by AW. Keep or
delete the private backup according to the operator's retention policy. Ordinary
Hermes launches can also retain the installed plugin because it is inactive
without an AW launch binding.
