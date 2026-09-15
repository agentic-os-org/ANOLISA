# cosh-ng User Guide

[中文版](../../../zh/user-entrypoint/cosh-ng/README.md)

cosh-ng is an AI-native Linux terminal with Enhanced Assisted as its default
and an explicit hook-free Native integration. Start with the quick start, then
use the task-based links below for the feature or command you need.

## Start here

- [Quick start](QUICKSTART.md) — install cosh-ng and run a first task.
- [Model providers](core/providers.md) — configure authentication and select a provider.
- [Configuration](configuration.md) — review files, settings, and precedence.
- [Supported platforms](supported-distros.md) — check package and service backends.
- [Windows WSL2 preview](wsl-preview.md) — start cosh-ng from Git Bash on Windows (community preview).

## Work in the terminal

| Goal | Read next |
|---|---|
| Use Shell commands and natural-language tasks together | [Interactive terminal](shell/overview.md) |
| Choose when Agent tool calls require confirmation | [Tool approval](shell/approval.md) |
| Resume or compact a conversation | [Session recovery](shell/session-recovery.md) |
| Learn slash commands and keyboard behavior | [Interactive behavior](shell/interactive-mode.md) |

## Add capabilities

| Goal | Read next |
|---|---|
| Share instructions across a project or team | [Skills](core/skills.md) |
| Connect tools from a local process or remote service | [Connect an MCP server](mcp.md) |
| Bundle Skills, Hooks, settings, and tools | [Extensions](core/extensions.md) |
| Run checks around Agent lifecycle events | [Hooks](core/hooks.md) |

## Manage system operations

Use read-only commands first. Add `--dry-run` to a supported package or service mutation before making a change; these operations usually need root privileges.

| Goal | Read next |
|---|---|
| Find, install, or remove packages | [Package management](cli/package-management.md) |
| Inspect or change systemd services | [Service management](cli/service-management.md) |
| Use the existing `cosh-cli` workspace checkpoint commands | [Workspace checkpoints](cli/checkpoint.md) |
| Check policy decisions and audit events | [Security audit](cli/audit.md) |

The workspace checkpoint page describes the direct `cosh-cli`
system-operations path. Managed Tasks can request both a pre-Runtime workspace
baseline and a durable barrier before each approved Runtime permission effect
from a configured `ws-ckpt` provider. `/task` lists only checkpoints durably
owned by that Task and provides read-only preview and diff while it runs;
recovery-protected switching requires a terminal Task.

Proven recovery snapshots remain listed even if the later switch fails or its
outcome is uncertain. Large previews return a bounded prefix and report
`changes_omitted`; the confirmation digest still covers the complete diff.

## Integrate and automate

The `cosh agent` launcher is installed by ANOLISA and RPM packages. Source and
unified builds install the bare Gateway binary instead; substitute
`cosh-gateway doctor`, `cosh-gateway run`, or `cosh-gateway task` and keep the
remaining arguments unchanged.

### Start managed Tasks from a source build

On Linux with systemd, contributors and testers can prepare an isolated
development Gateway from the cosh-ng source root and enter its connected Shell:

```bash
./scripts/managed-task-dev.sh setup
./scripts/managed-task-dev.sh shell
```

The command surface is:

```text
managed-task-dev.sh setup [--no-build] [--workspace ABSOLUTE_DIR] [--codex auto|off|required] [--environment inherit|off] [--checkpoint-socket PATH] [--stop-production] [--dry-run]
managed-task-dev.sh shell [--dry-run]
managed-task-dev.sh status [--dry-run]
managed-task-dev.sh down [--dry-run]
managed-task-dev.sh uninstall [--purge-state] [--dry-run]
```

By default, `setup` builds the required source binaries in the debug
profile and admits the canonical form of `$PWD` as the only workspace. Use
`--no-build` to reuse existing debug artifacts or `--workspace ABSOLUTE_DIR`
to select a different absolute workspace. A successful setup ends with a
Gateway capabilities smoke check; it does not submit a Task.

Core is always configured. The default `--codex auto` adds Codex only when it
finds the already installed pinned `codex-acp` Adapter. Setup never invokes
`npx`, downloads an Adapter, or modifies the installed bundle. It reuses the
effective `CODEX_HOME` from the invoking user. Use `--codex off` for Core only,
or `--codex required` to fail setup unless the pinned Adapter is ready. Reusing
`CODEX_HOME` also reuses the login state and configuration stored there.

The default `--environment inherit` copies only allowlisted variables that are
currently set, preserving each current value. Core-only setup copies the eight
uppercase and lowercase proxy forms: `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
`NO_PROXY`, `http_proxy`, `https_proxy`, `all_proxy`, and `no_proxy`.

When the Codex Adapter is enabled, setup also copies these documented Codex
variables and variables supported by the pinned Adapter when set:
`CODEX_SQLITE_HOME`, `CODEX_API_KEY`,
`CODEX_ACCESS_TOKEN`, `OPENAI_API_KEY`, `OPENAI_FEDERATION_RULE_ID`,
`OPENAI_IDENTITY_TOKEN_FILE`, `OPENAI_WORKLOAD_IDENTITY_CONTEXT`,
`CODEX_CA_CERTIFICATE`, `SSL_CERT_FILE`, and `RUST_LOG`. It reads
`CODEX_HOME/config.toml` and also copies currently set variables named by each
`model_providers.*.env_key` and `model_providers.*.env_http_headers` value.

Setup does not copy the whole user environment, wildcard all `CODEX_*`
variables, or automatically inherit installer controls, `LD_*`, `DYLD_*`, or
SSH variables. Uppercase and lowercase proxy forms keep their separate current
values, and proxy URLs containing userinfo are preserved. Setup and status show
only inherited variable names, never values. When userinfo proxies, API/access
tokens, workload identity values, or provider-declared variables are copied,
setup warns that credentials were snapshotted into the root-owned mode `0600`
Gateway/Adapter environment and may be readable by same-UID processes. Use
`--environment off` to disable the snapshot completely. Treat the generated
environment as private configuration, and rerun `setup` after changing proxy
or credential values because the service does not inherit later Shell changes.

Checkpoint support is off by default. Pass `--checkpoint-socket PATH` with an
absolute existing Unix socket only when an existing `ws-ckpt` provider should
be exposed; otherwise the development
catalog has no checkpoint provider. `Auto` then records an explicit durable
downgrade and continues only for that known unavailability, while `Off` skips
both checkpoint stages. The Shell form does not offer `On` without a provider;
an API request for `On` fails closed. Checkpoint errors and uncertain outcomes
never authorize launch or an effect.

This development profile uses the durable `allow_all` policy for local source
testing. Managed Core exposes only `ask_user_question` and approval-gated
`write_file`; its pinned workspace rejects traversal, outside absolute paths,
and symlink escapes. Correlated Codex permission callbacks receive a one-time
allow decision, but Codex executes with the service user's authority and is not
confined by a workspace filesystem sandbox. Review the goal and canonical
workspace before submission, and do not use this profile for untrusted
repositories or prompts.

The helper does not overwrite the installed package. It uses the transient
`cosh-gateway-dev@.service` template under `/run/systemd/system`, an environment
file at `/run/cosh-gateway-dev-$USER.env`, a socket at
`/run/cosh-gateway-dev-$USER/gateway.sock`, staged binaries below
`/usr/local/libexec/cosh-ng-dev/$USER`, and durable Task state under
`/var/lib/cosh-gateway-dev-$USER`. The unit and environment do not survive a
boot, so rerun `setup` after restarting the host. If the packaged production or
legacy Gateway is active for the same account, setup refuses without changing
it. Use `--stop-production` only when you intentionally want setup to stop
production and switch that account to the development instance. `--dry-run`
previews the corresponding setup, Shell, status, shutdown, or uninstall
operation.

Use the lifecycle commands as follows:

```bash
./scripts/managed-task-dev.sh status
./scripts/managed-task-dev.sh down
./scripts/managed-task-dev.sh uninstall
./scripts/managed-task-dev.sh uninstall --purge-state
```

`down` stops the transient instance but retains its integration and data.
`uninstall` removes the development integration while retaining durable Task
state for a later setup. Add `--purge-state` to delete that development state
as well. Neither form uninstalls cosh-ng nor deletes production Gateway state.

- Run `cosh agent doctor --profile codex --workspace "$PWD"` to verify a
  separately installed `codex-acp`, or select `claude-code` for
  `claude-agent-acp`. Run one turn by piping a bounded UTF-8 prompt into
  `cosh agent run`; add `--output jsonl` for stable streamed events. COSH does
  not run `npx`, download packages, or accept arbitrary adapter commands.
  Permission requests use `/dev/tty`, leaving stdin dedicated to the prompt.
  The default `--permission prompt` offers only `allow_once` and `reject_once`;
  no TTY, unsupported choices, EOF, and `--permission deny` all cancel without
  authorization. Redacted append-only evidence defaults to
  `$XDG_STATE_HOME/cosh/gateway/permission-evidence.jsonl`, falling back to
  `$HOME/.local/state/cosh/gateway/permission-evidence.jsonl`. Use an absolute
  `--permission-evidence PATH` to override it. COSH stores hashes and the
  decision class, never raw prompts, tool arguments, option labels, session
  identifiers, or workspace paths. Evidence persistence failure cancels the
  callback and fails the run. These direct ACP commands are ungoverned by the
  durable Gateway Task Plane and are intended for local interoperability.
- For persistent managed Tasks, start the one packaged system-scope
  `cosh-gateway@.service`. A required root-managed environment file selects the
  exact canonical workspace. Keep it outside the service's private
  `/var/lib/cosh-gateway-$USER` StateDirectory so Runtime access does not widen
  to Gateway databases and audit state:

  ```bash
  sudo install -d -m 0755 /etc/cosh
  printf 'COSH_GATEWAY_WORKSPACE=%s\n' "$(pwd -P)" | \
    sudo tee "/etc/cosh/gateway-$USER.env" >/dev/null
  sudo chmod 0600 "/etc/cosh/gateway-$USER.env"
  sudo systemctl enable --now "cosh-gateway@$USER.service"
  gateway_socket="/run/cosh-gateway-$USER/gateway.sock"
  ```

  The unit fixes Core `HOME` at
  `/var/lib/cosh-gateway-$USER/core-home`. Store its user-level provider config
  at `/var/lib/cosh-gateway-$USER/core-home/.copilot-shell/config.toml`, or use
  `/etc/copilot-shell/config.toml` for system configuration.

  The service always passes the packaged Core executable. Optional standalone
  argument variables add Codex and checkpoint support to the same daemon,
  socket, database, and canonical workspace. Empty variables expand to no
  argument, so omitted optional arguments do not block Core-only start.
  Do not start the retired `cosh-gateway-acp@` unit; the unified unit conflicts
  with it to prevent two daemons from contending for the same state.
- To make Codex selectable, install the pinned Adapter and append its absolute
  executable argument and Node path:

  ```bash
  adapter_root="$HOME/.local/lib/cosh/acp-adapters"
  install -d -m 0700 "$(dirname "$adapter_root")"
  ./src/cosh-ng/scripts/install-acp-adapters.sh --prefix "$adapter_root"
  node_bin="$(dirname "$(command -v node)")"
  sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
  COSH_GATEWAY_ACP_ARG='--acp-adapter=$adapter_root/node_modules/.bin/codex-acp'
  PATH=$node_bin:/usr/bin:/bin
  EOF
  sudo systemctl restart "cosh-gateway@$USER.service"
  ```

  The bundle pins `@agentclientprotocol/codex-acp` exactly to `1.6.2` and
  Gateway rejects a different reported identity or version. Paths with spaces
  must be quoted as one systemd word in the trusted environment file.
- To enable pre-Runtime baselines and permission-effect barriers, append the
  absolute `ws-ckpt` socket. The
  security audit argument is optional but cannot be used without the socket:

  ```bash
  sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
  COSH_GATEWAY_CHECKPOINT_ARG=--checkpoint-socket=/run/ws-ckpt/ws-ckpt.sock
  COSH_GATEWAY_SECURITY_AUDIT_ARG=--security-audit=/var/lib/cosh-gateway-$USER/security-audit.jsonl
  EOF
  sudo systemctl restart "cosh-gateway@$USER.service"
  ```

  The Gateway unit has no `ws-ckpt` service dependency. It reports checkpoint
  readiness from configured admission instead of blocking Core-only startup.
- Inside `cosh`, both `/task` and `/task <goal>` open the managed Task form;
  the latter prefills the goal. The form obtains the sealed launch catalog
  from Gateway, offers only ready Runtimes, and selects a checkpoint policy.
  Its confirmation page shows goal, Runtime, canonical workspace, checkpoint,
  and the durable default approval policy `allow_all`:

  ```text
  /task upgrade the dependencies, update the code, and run the tests
  /task
  /task list
  /task show
  /task show <tsk_UUID>
  ```

  Submission returns a durable Task ID immediately. The service owns Gateway
  and its Runtime children, so closing Shell or SSH does not cancel the Task.
  Reconnect and use `/task list` or `/task show [task-id]` for durable progress
  and results. A Gateway restart still cannot resume an ACP session; the Run is
  suspended or lost and requires explicit retry rather than prompt replay.

  The policy applies before Runtime launch and before each approved Runtime
  permission effect. `Auto` records a durable downgrade only when the provider
  explicitly reports unavailable or known-no-effect; errors and uncertain
  outcomes fail closed. `On` requires exact checkpoint evidence, and `Off`
  creates neither the baseline nor per-effect barriers. Workspace checkpoints
  do not protect host, credential, network, cloud, or other external effects.

  Managed Core uses the closed `workspace-write-v1` profile. It exposes only
  `ask_user_question` and `write_file`; every write requires a Runtime-native
  permission decision, the applicable durable checkpoint barrier, and Gateway
  approval before Core executes it. Its pinned workspace rejects traversal,
  outside absolute paths, and symlink escapes. Shell, edit, read, MCP, Skills,
  and Hooks are not admitted.

  The durable `allow_all` policy does not create provider `allow_always` rules.
  Correlated Codex callbacks receive `allow_once`. A per-effect checkpoint
  barrier covers only permission effects that ACP actually reports; native
  effects without a callback are not covered. ACP-native effects run with
  the service user's authority inside systemd containment and are not confined
  by a workspace filesystem sandbox. The unit still makes system paths
  read-only, uses private `/tmp`, and hides `/run/user`; “local-user authority”
  does not mean unrestricted host authority. Gateway persists bounded reported
  events without claiming exact receipts for ACP-native effects.

  Inspect Task-owned snapshots while the Task runs; switch only after it is terminal:

  ```bash
  /task snapshots <task-id>
  /task snapshot preview <task-id> <snapshot-id>
  /task snapshot diff <task-id> <snapshot-id>
  /task snapshot switch <task-id> <snapshot-id>
  ```

  Switch confirmation defaults to cancel. Gateway rejects active Tasks,
  foreign or abbreviated IDs, stale previews, and occupied workspaces. Move
  cosh and other shell processes outside the workspace before switching. The
  daemon recomputes the live diff under the workspace write lock immediately
  before rollback and rejects generation or diff drift before backend effects.
- For automation, pipe intent into the same Task API:

  ```bash
  printf '%s\n' 'inspect the failed service' | \
    cosh agent task --socket "$gateway_socket" submit \
      --runtime core --checkpoint auto --approval-policy allow-all \
      --idempotency-key '<stable-submit-key>'
  cosh agent task --socket "$gateway_socket" list --limit 20
  cosh agent task --socket "$gateway_socket" get '<tsk_UUID>'
  cosh agent task --socket "$gateway_socket" events '<tsk_UUID>' --after 0 --limit 64
  printf '%s\n' 'answer to the question' | \
    cosh agent task --socket "$gateway_socket" append '<tsk_UUID>' \
      --input-request-id '<inp_UUID>' --idempotency-key '<stable-input-key>'
  cosh agent task --socket "$gateway_socket" cancel '<tsk_UUID>' --run-id '<run_UUID>' \
    --idempotency-key '<stable-cancel-key>'
  cosh agent task --socket "$gateway_socket" retry '<tsk_UUID>' \
    --previous-run-id '<run_UUID>' --idempotency-key '<stable-retry-key>'
  ```

  The API supports `capabilities`, `submit`, `list`, `get`, `events`, `append`,
  `cancel`, `retry`, and `resolve-approval`. Idempotency keys make retries safe
  after uncertain client I/O. Current deterministic tests cover launch
  selection and baseline policy; real Codex, SSH-disconnect, and packaged
  systemd execution remain installation-specific unaccepted gates.
- The experimental Web continuation command is **Linux-only** and currently
  **refuses the built-in Core/Codex catalog**. Continue Tasks through `/task` or
  `cosh agent task`. macOS builds omit the Web command and its Linux credential
  tests; they do not attempt to resolve credentials through `/proc/self/fd`.

  On Linux, `cosh agent web --help` describes these options:

  | Option | Contract |
  |--------|----------|
  | `--socket PATH` | Absolute local Gateway socket override; otherwise uses `COSH_GATEWAY_SOCKET`, an active packaged instance, then the user runtime directory. |
  | `--workspace PATH` | Existing absolute directory whose canonical path, device, and inode must match the daemon's admitted workspace identity. |
  | `--token-file PATH` | Absolute, single-link regular file, mode `0600`, owned by root or the current user, with trusted ancestors and 32–256 printable ASCII token bytes. The opened file must be outside the admitted workspace. |
  | `--bind ADDRESS` | IPv4/IPv6 loopback only; default `127.0.0.1:8765`. |
  | `--output human\|jsonl` | Startup and error presentation. |

  Admission queries the authenticated local daemon's capabilities **before**
  binding HTTP. A missing daemon, unknown launch schema, workspace mismatch,
  incomplete Runtime catalog, delegated local-user authority, or unbrokered
  effects causes a visible `web_failed` error. Unavailable Runtime entries are
  checked too: disabling new launches does not remove historical Task authority.
  `--capability-profile` has been removed; a caller declaration cannot constrain
  the daemon's authority.

  Both current Runtime entries attest local-user authority. A workspace-external
  token does not isolate it from those Runtimes, so this release has no admitted
  production Web configuration. A restricted Runtime boundary and corresponding
  daemon attestation require separate implementation and validation before Web
  can be enabled. This restriction does not change terminal, Task CLI, or direct
  ACP operation.

  The retained presentation adapter implements Task lists, cursor-based events,
  questions, Task-bound approvals, cancellation, and retry. Its HTTP contract
  requires a Bearer token in the Authorization header and fresh mutation
  idempotency keys; cookies and query tokens are rejected. It has no TLS, OIDC,
  multi-user roles, or public listener. These routes are unavailable until
  startup admission succeeds.
- [Structured OS CLI](cli/overview.md) — command domains and safe automation patterns.
- [Output format](output-format.md) — the `CoshResponse<T>` success and error envelope.
- [Headless mode](core/headless-mode.md) — JSONL integration for other frontends.
- [Agent tools](core/tools.md) — tool boundaries and approval behavior.
