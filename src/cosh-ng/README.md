# cosh-ng

[中文版](README_zh.md)

cosh-ng is an AI-native terminal built around the shell you already use.
`cosh` starts in Enhanced Assisted mode by default, preserving implicit
natural-language routing, Skills, approval cards, and resumable Agent
conversations in the same terminal. Choose Native integration at startup when
bash or zsh must own the session without Cosh hooks, observation, or insights.
Structured JSON and JSONL interfaces remain available for automation and
Agent integration.

By default a login shell (for example `cosh --login`) launches Bash with a real
login identity on Bash 4+; set `shell.login_identity = false` to keep the previous
non-login (`--rcfile`) startup. Cosh falls back automatically when the bounded
login-identity capability probe fails, on Bash 3.2 (such as the macOS system
bash), or when the
parent exports a function name that POSIX startup cannot import.

## Why cosh-ng

| In a conventional terminal | In cosh-ng |
|---|---|
| You translate intent into commands | Mix natural language and commands in the default Assisted mode |
| Automation is scattered across scripts | Package repeatable workflows as Skills |
| AI context is tied to one chat window | Resume workspace-scoped Agent conversations |
| AI actions are hard to inspect | Review tool calls in approval cards and audit records |
| Every distro has different system commands | Use `cosh-cli` for stable, structured OS operations |

Interactive programs, pipes, redirects, job control, bash/zsh configuration,
and `Ctrl+C` continue to work in the foreground terminal.

For isolated command execution, use `cosh --isolated -c '<command>'` with Bash.
Isolated Bash login invocations are rejected with status 2 because Bash cannot
suppress logout files. See [shell invocation options](../../docs/user-guide/en/user-entrypoint/cosh-ng/shell/interactive-mode.md).

## Install

On Alibaba Cloud Linux 4, install cosh-ng from the RPM backend in system scope
with the ANOLISA CLI:

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
sudo "$HOME/.local/bin/anolisa" --install-mode system install cosh-ng --backend rpm
```

The public installer can combine those steps:

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

Use the same entry point for later updates or removal:

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --install-mode system --upgrade
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --install-mode system --uninstall
```

On macOS 11+ (arm64 / x86_64), use user scope instead:

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend raw --install-mode user
export PATH="$HOME/.local/bin:$PATH"
```

On Alibaba Cloud Linux 4, the RPM is also available directly:

```bash
sudo yum install cosh-ng
```

The published Linux raw contract is not currently portable across all routed
distributions, so it is not the recommended Linux installation path. The raw
package supports macOS 11+ (arm64 / x86_64), where Linux-only package and service operations
remain unavailable. Source builds are for contributors; follow the
[developer setup](../../docs/developer-guide/en/cosh-ng/getting-started.md).

The release build matrix also produces a macOS x86_64 (Intel) raw archive,
with the same macOS 11.0 minimum as arm64. Intel artifacts are available from
GitHub Releases once a version containing this build support is published;
the public installer above also requires an Intel ANOLISA CLI release and
separate enablement of the website installer.

## Start in 30 seconds

```bash
cd your-project
cosh
```

Enhanced Assisted is the default. Cosh keeps the shell's native prompt
appearance, and may classify and route submitted input before the foreground
Shell executes it:

```text
user@host:~/project$ git status
user@host:~/project$ explain why this service keeps restarting
```

At an empty prompt, press `Shift+Tab` to switch to Enhanced Shell-only, where
ordinary input goes straight to the Shell while post-command insights remain
available. Press `Shift+Tab` again to return to Assisted mode.

Status symbols are optional: set `shell.status_symbols = true` in `config.toml`
(or `COSH_SHELL_STATUS_SYMBOLS=1` for one session) to show a `◇` (Assisted) or
`◌` (Shell-only) status line above each prompt. They are off by default.

Start Native explicitly when the session must have no Cosh hooks, observation,
or insights:

```bash
COSH_SHELL_INTEGRATION=native cosh
```

Enhanced does not preserve exported Bash `PROMPT_COMMAND` values in child
processes. See [Bash prompt compatibility](../../docs/user-guide/en/user-entrypoint/cosh-ng/shell/overview.md#bash-prompt-compatibility)
for the version-specific attribute limits and Native alternative.

```text
$ hello
bash: hello: command not found
```

Use `/auth` to choose a supported provider plan, `/help` to list current slash
commands, and `/mode approval recommend` when every Agent tool call should wait
for confirmation. Approval settings use `recommend`, `auto`, or `trust` across
the shell and Core. In enhanced integration, the cosh-core runtime makes
`/agent` open a one-shot
Composer that accepts a leading `/skill:<name>` and validated workspace-local
`@path` references. Type `/` in the Composer to browse slash commands, use
Up/Down to select, and press Enter to execute the selected command. Use Tab to
complete a command before adding arguments; Enter submits drafts with arguments
or multiple lines as written.
Ordinary shell prompts keep native path completion.

In `cosh-core`, configured Hooks run by default without `hooks.enabled = true`.
Explicit `hooks.enabled = false` keeps config Hooks disabled even when Extensions
are installed. See [Hooks](../../docs/user-guide/en/user-entrypoint/cosh-ng/core/hooks.md)
for disable scope and fail-closed `PreToolUse` behavior.

For `type = "aliyun"`, SysOM automatically prefers a reachable VPC endpoint.
Set `ai.providers.<id>.sysom_endpoint` to pin an endpoint; `COSH_SYSOM_ENDPOINT`
takes precedence, and `base_url` does not route SysOM. Only automatic VPC routes
bypass system proxies; explicit overrides and public fallback retain proxy
settings. See [provider configuration](../../docs/user-guide/en/user-entrypoint/cosh-ng/core/providers.md).

To run one locally installed ACP adapter without entering the interactive
Shell, verify it first and then pipe the prompt through stdin:

The commands below use the `cosh agent` launcher installed by ANOLISA or the
RPM. A source or unified build installs the bare Gateway binary instead; use
`cosh-gateway doctor`, `cosh-gateway run`, or `cosh-gateway task` with the same
remaining arguments.

```bash
cosh agent doctor --profile codex --workspace "$PWD"
printf '%s\n' 'summarize the current changes' | \
  cosh agent run --profile codex --workspace "$PWD"
```

The first release accepts only the built-in `codex` and `claude-code`
profiles. Install the corresponding `codex-acp` or `claude-agent-acp`
executable separately; COSH never invokes `npx` or downloads an adapter at
runtime. A permission callback prompts only on the local controlling terminal;
without one, or with `--permission deny`, COSH cancels it. Once-only decisions
are recorded as redacted evidence under the private local state directory.

### Run persistent managed Tasks

Linux contributors can build and start an isolated development instance from
the cosh-ng source root, then enter a Shell already connected to it:

```bash
./scripts/managed-task-dev.sh setup
./scripts/managed-task-dev.sh shell
```

The default setup builds debug binaries, admits the current canonical
directory, always enables Core, and adds Codex only when it detects the already
installed pinned Adapter. It never downloads the Adapter, reuses the effective
`CODEX_HOME`, and snapshots allowlisted environment variables without printing
their values. Core-only setup inherits the eight proxy variable forms. With
Codex enabled, setup also includes documented Codex variables, variables
supported by the pinned Adapter, and currently set provider variables declared
by `CODEX_HOME/config.toml`.
Credential-bearing variables trigger a warning because the root-owned mode
`0600` Gateway/Adapter environment may be readable by same-UID processes. No
checkpoint provider is configured by default. The profile uses `allow_all`
for durable per-effect decisions. Managed Core exposes only approval-gated
`write_file` inside its pinned workspace; Codex retains service-user authority
inside the packaged containment and is not a workspace filesystem sandbox.

Development uses a separate transient `cosh-gateway-dev@` unit, socket, state,
and environment file without overwriting package files. Rerun `setup` after a
host reboot. An active production Gateway is rejected unless you explicitly
pass `--stop-production`. See the [user guide](../../docs/user-guide/en/user-entrypoint/cosh-ng/README.md)
for status, shutdown, cleanup, overrides, and the full security boundary.

The package installs one account-named `cosh-gateway@.service`. Core is always
configured. Codex ACP and the checkpoint provider used for pre-Runtime
baselines and pre-effect barriers are optional inputs to that same daemon,
socket, and SQLite database; do not start the retired `cosh-gateway-acp@` unit
beside it.

The service requires a root-managed environment file with the exact canonical
workspace. Keeping Task workspaces outside the private StateDirectory prevents
Runtime access from being widened to Gateway databases and audit state. To
bind the Gateway to the current project, create the file and start the service:

```bash
sudo install -d -m 0755 /etc/cosh
printf 'COSH_GATEWAY_WORKSPACE=%s\n' "$(pwd -P)" | \
  sudo tee "/etc/cosh/gateway-$USER.env" >/dev/null
sudo chmod 0600 "/etc/cosh/gateway-$USER.env"
sudo systemctl enable --now "cosh-gateway@$USER.service"
```

The unit fixes Core `HOME` at
`/var/lib/cosh-gateway-$USER/core-home`. Put the user-level provider config at
`/var/lib/cosh-gateway-$USER/core-home/.copilot-shell/config.toml`, or use
`/etc/copilot-shell/config.toml` for system configuration.

To make Codex selectable, install the pinned Adapter and add one complete
optional argument. The required environment file is trusted operator configuration, so
keep it root-owned and quote paths containing spaces as one systemd word.

```bash
adapter_root="$HOME/.local/lib/cosh/acp-adapters"
install -d -m 0700 "$(dirname "$adapter_root")"
./scripts/install-acp-adapters.sh --prefix "$adapter_root"
node_bin="$(dirname "$(command -v node)")"
sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
COSH_GATEWAY_ACP_ARG='--acp-adapter=$adapter_root/node_modules/.bin/codex-acp'
PATH=$node_bin:/usr/bin:/bin
EOF
sudo systemctl restart "cosh-gateway@$USER.service"
```

To expose checkpoint choices, configure the absolute `ws-ckpt` socket. The
security audit path is optional and is valid only with the checkpoint socket.
The unit deliberately has no dependency on `ws-ckpt`, so an absent optional
configuration never prevents Core-only startup.

```bash
sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
COSH_GATEWAY_CHECKPOINT_ARG=--checkpoint-socket=/run/ws-ckpt/ws-ckpt.sock
COSH_GATEWAY_SECURITY_AUDIT_ARG=--security-audit=/var/lib/cosh-gateway-$USER/security-audit.jsonl
EOF
sudo systemctl restart "cosh-gateway@$USER.service"
```

Inside `cosh`, `/task` opens a form for the goal, Runtime (`Core (cosh-core)`
or `Codex (ACP)`), and checkpoint policy (`Auto`, `On`, or `Off`). `/task
<goal>` opens the same form with the goal prefilled. The confirmation shows the
canonical workspace and the durable default `allow_all` approval policy.
Unavailable Runtimes are not selectable, and `On` is not offered when the
checkpoint provider is unavailable.

```text
/task upgrade the dependencies, update the code, and run the tests
/task
/task list
/task show
/task show <tsk_UUID>
```

Submission returns a durable Task ID immediately. The system service owns the
Gateway and Runtime, so exiting the Shell or disconnecting SSH does not cancel
the Task; reconnect and use `/task list` or `/task show [task-id]` to read
durable progress and results. A Gateway restart still cannot resume an ACP
session: that Run is suspended or lost and requires an explicit retry instead
of silently replaying the prompt.

The checkpoint policy applies both before Runtime launch and before an approved
Runtime-native effect. `Auto` records a durable downgrade only when the provider
explicitly reports unavailable or known-no-effect; an error or uncertain result
does not authorize the effect. `On` fails closed unless exact checkpoint
evidence exists, while `Off` creates neither the baseline nor per-effect
barriers.

Empty workspaces are valid checkpoint state in current ws-ckpt. If an older
daemon skips the initial snapshot, `On` stops the Task before Runtime launch.
Upgrade ws-ckpt or add a file and submit a new Task, or explicitly select `Off`
when checkpoint protection is not needed. The failed Task is terminal;
`retry` does not rebuild a failed baseline.

Managed Core uses the closed `workspace-write-v1` profile. It exposes only
`ask_user_question` and `write_file`; every write is a Runtime-native permission
decision, and Core executes it only after Gateway approval and any required
checkpoint barrier. The existing pinned workspace filesystem rejects parent
traversal, absolute outside paths, and symlink escapes. Shell, edit, read, MCP,
Skills, and Hooks are not admitted by this profile.

Codex permission callbacks correlated to the active Task receive `allow_once`;
COSH never creates provider `allow_always` rules. Codex-native effects are not
Gateway-brokered and run with the service user's authority inside the packaged
systemd containment, not a workspace filesystem sandbox. A pre-effect barrier
covers only a permission effect actually reported by ACP; Codex-native effects
without such a callback are not covered. The unit makes system paths read-only,
uses a private `/tmp`, and hides `/run/user`, so this is not a claim of
unrestricted host authority. Gateway persists bounded Runtime events without
claiming exact side-effect receipts for ACP-native tools.

Use the Task-owned surface to list, preview, or diff proven-created snapshots
while a Task runs. Switching is available only after the Task is terminal and revalidates the
preview and Task revision, creates a pinned recovery snapshot, then asks
`ws-ckpt` to apply the exact full ID. The daemon recomputes the live diff under
the workspace write lock immediately before rollback; a generation or diff
change is rejected before the backend runs. An active Task, foreign snapshot,
stale preview, or occupied workspace fails closed.

Proven recovery snapshots remain listed even if the later switch fails or its
outcome is uncertain. Large previews return a bounded prefix and report
`changes_omitted`; the confirmation digest still covers the complete diff.

```bash
/task snapshots <task-id>
/task snapshot preview <task-id> <snapshot-id>
/task snapshot diff <task-id> <snapshot-id>
/task snapshot switch <task-id> <snapshot-id>
```

For automation, the equivalent submission contract is:

```bash
gateway_socket="/run/cosh-gateway-$USER/gateway.sock"
printf '%s\n' 'inspect the failed service' | \
  cosh agent task --socket "$gateway_socket" submit \
    --runtime core --checkpoint auto --approval-policy allow-all \
    --idempotency-key '<stable-submit-key>'
cosh agent task --socket "$gateway_socket" list --limit 20
cosh agent task --socket "$gateway_socket" get '<tsk_UUID>'
cosh agent task --socket "$gateway_socket" events '<tsk_UUID>' --after 0 --limit 64
```

The Task API also supports `append`, `cancel`, `retry`, and
`resolve-approval`. `doctor` and `run` remain separate, uncontained one-shot ACP
interoperability commands. The real Codex provider, SSH-disconnect flow, and
systemd service have not been rerun for this increment; current acceptance is
based on deterministic local coverage.

The experimental `cosh agent web` command is Linux-only. Before opening an HTTP
listener, it queries the selected daemon's capabilities and verifies the
canonical workspace identity and Runtime authority. It rejects the current
Core/Codex catalog because both entries attest local-user authority, which can
read a same-user token even outside the workspace. Use `/task` or
`cosh agent task` for continuation with this release. The former
`--capability-profile` declaration has been removed; it never restricted the
daemon. See the [Web command reference](../../docs/user-guide/en/user-entrypoint/cosh-ng/README.md)
for the admission boundary and platform limits.

`SIGINT` and `SIGTERM` trigger bounded scheduler and Runtime shutdown before the
daemon exits. The daemon remains Unix-only and does not open a remote listener.

The repository includes fake-adapter conformance coverage for the direct ACP
path. Run the separate real Codex/Claude adapter checks and manual Terminal
acceptance before treating a particular ACP installation as production-validated.

`cosh-cli checkpoint recover --workspace <path>` supports recovery of interrupted
initialization through the ws-ckpt daemon. Successful JSON includes `meta.warning`
when retained data needs inspection. See
[workspace checkpoints](../../docs/user-guide/en/user-entrypoint/cosh-ng/cli/checkpoint.md).

## Documentation

- [User guide](../../docs/user-guide/en/user-entrypoint/cosh-ng/README.md)
- [Connect an MCP server](../../docs/user-guide/en/user-entrypoint/cosh-ng/mcp.md)
- [Interactive terminal](../../docs/user-guide/en/user-entrypoint/cosh-ng/shell/overview.md)
- [Configuration](../../docs/user-guide/en/user-entrypoint/cosh-ng/configuration.md)
- [Manage system operations](../../docs/user-guide/en/user-entrypoint/cosh-ng/cli/overview.md)
- [Headless integration](../../docs/user-guide/en/user-entrypoint/cosh-ng/core/headless-mode.md)
- [Developer getting started](../../docs/developer-guide/en/cosh-ng/getting-started.md)
- [Architecture](../../docs/developer-guide/en/cosh-ng/architecture.md)
- [Contributing](CONTRIBUTING.md)

## Data Collection

cosh-ng collects anonymous operational metrics to improve service quality.
This includes tool call counts, token usage, approval statistics,
OS type/architecture, and a persistent installation UUID for
cross-session correlation. **No user prompts, code content, or
conversation content is collected.**

To disable telemetry for the current user:

```bash
mkdir -p ~/.copilot-shell
touch ~/.copilot-shell/telemetry_disabled
```

A system administrator can also disable telemetry for all users on the
machine by creating the system-level sentinel:

```bash
sudo mkdir -p /etc/anolisa
sudo touch /etc/anolisa/.telemetry_disabled
```

## Contribute

Source builds are a contributor workflow. Start with the
[developer guide](../../docs/developer-guide/en/cosh-ng/getting-started.md).

## License

Apache-2.0
