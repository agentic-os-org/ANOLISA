# cosh-ng configuration

[中文版](../../../zh/user-entrypoint/cosh-ng/configuration.md)

Start with the defaults. Use `/auth`, `/mode`, and `/config language` for
interactive changes; edit TOML when settings must persist or be shared.

## Files and authority

| File | Read by | Scope |
|---|---|---|
| `/etc/copilot-shell/config.toml` | `cosh-core` and audit | Administrator defaults |
| `~/.copilot-shell/config.toml` | `cosh-core` and `cosh-shell` | User settings |
| `<workspace>/.copilot-shell/config.toml` | `cosh-core` | Project runtime preferences |

Core layers files in system → user → project order. Project config may set
Agent, Hook, Skill, session, `active_model`, and output-language preferences,
but `active_provider`, provider definitions, MCP servers, and project audit
settings are ignored. Project Hooks still require `/hooks trust-project` in the
interactive shell. `cosh-shell` reads the user file, not the system or project
file.

## Minimal user configuration

```toml
[ai]
active_provider = "dashscope"
active_model = "qwen3.7-plus"
output_language = "en"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "${DASHSCOPE_API_KEY}"
model = "qwen3.7-plus"

[agent]
approval_mode = "recommend"
max_turns = 50
max_tool_calls_per_turn = 10

[skills]
custom_paths = ["~/team-skills"]

[session]
auto_persist = true
persist_dir = "~/.copilot-shell/cosh-core/sessions"

[logging]
level = "warn"

[ui]
language = "auto"
log_level = "warn"

[shell]
default = "auto"
integration = "enhanced"
login_identity = true
adapter_default = "cosh-core"
analysis_mode = "smart"
approval_mode = "auto"
status_symbols = false
```

Use environment expansion or `/auth` instead of writing a raw secret into
TOML. See [Providers](core/providers.md) for provider choices.

## Approval and turn budgets

Core approval modes apply to direct integrations:

| Mode | ReadOnly | FileEdit | Shell, network, MCP, external |
|---|---|---|---|
| `trust` | Run | Run | Run |
| `auto` | Run | Run | Ask |
| `recommend` | Run | Ask | Ask |

Core and the shell use the same canonical names: `recommend`, `auto`, and
`trust`. Existing `balanced`, `suggest`, and `strict` values are read as
`recommend`; invalid configuration values also fall back to `recommend`.
`agent.max_turns` limits one Agent request (default `50`), while
`max_tool_calls_per_turn` defaults to `10`. A new prompt starts a fresh turn
budget.

## Sessions and compaction

```toml
[session]
auto_persist = true
persist_dir = "~/.copilot-shell/cosh-core/sessions"

[session.compaction]
enabled = true
auto = true
trigger_ratio = 0.70
emergency_ratio = 0.90
target_ratio = 0.30
preserve_recent_runs = 2
# auto_compact_token_limit = 89600
# model_context_window = 128000
# model_max_output_tokens = 8192
```

Keep `target_ratio <= trigger_ratio <= emergency_ratio`. Compaction changes
only the model-visible history; the persisted transcript remains complete. Set
`auto_persist = false` to disable resumability for the process.

## MCP and other optional sections

Define MCP clients only in system or user config. Each server uses one of
`command` (stdio) or `url` (Streamable HTTP); `allowed_tools` omitted means all,
while `[]` means none. Follow [Connect an MCP server](mcp.md) for examples,
OAuth, and lifecycle commands.

For shell recommendations and health checks, add only what you need:

```toml
[shell.recommendations]
enabled = true
bash_history = false

[health]
enabled = true
role = "web-server"
critical_mounts = ["/", "/var"]

[[health.services]]
name = "nginx"
expected = "active"
```

`integration` accepts `native` or `enhanced`. Enhanced is the default and starts
in Assisted mode, with marker-based Agent routing and command events.
At an empty prompt, `Shift+Tab` switches between Assisted and Shell-only;
Shell-only keeps command events and post-command insights but sends
ordinary input to bash or zsh. Native leaves input, Shell options, traps, and
startup files under bash or zsh ownership and provides no Cosh observation or
insights. The integration value is read when `cosh` starts, so changing it
requires a new session. Invalid values reject startup with a visible error.

`status_symbols` is `false` by default. Set it to `true` (or
`COSH_SHELL_STATUS_SYMBOLS=1` for one session) to show a `◇ ` (Assisted) or
`◌ ` (Shell-only) status line above each Enhanced prompt. Native sessions
never show status symbols. The value is read at startup, and invalid values
fall back to `false`.

`analysis_mode` accepts `smart`, `auto`, or `manual`; shell approval accepts
`recommend`, `auto`, or `trust`. `health.services.expected` accepts `active` or
`inactive`.

`login_identity` (default `true`) gives an interactive Enhanced login shell a
real login identity: the inner bash starts as a login shell (`shopt -q
login_shell` == yes) and reads `/etc/profile` and `~/.bash_profile` like `bash
-l`. Set `login_identity = false` to fall back to the previous non-login startup
(`--rcfile`). Bash 4 and newer use the login-identity path when the bounded
real-launch capability probe succeeds. Bash 3.2 (including the macOS system
Bash), probe failures, and
parent environments that export function names POSIX startup cannot import all
fall back automatically. Zsh is unaffected.

## Audit settings

Audit settings come from the system file when it has an `[audit]` table;
otherwise the user table is used. Project audit tables are ignored.

```toml
[audit]
mode = "best_effort"         # best_effort | required
retention_days = 30
max_disk_bytes = 1073741824
```

`retention_days` and `max_disk_bytes` must be greater than zero. The storage
root is `$XDG_STATE_HOME/cosh/audit` or `~/.local/state/cosh/audit`; set
`COSH_AUDIT_DIR` to an absolute path to override it.

## Telemetry opt-out

cosh-ng collects anonymous operational metrics to improve service quality.
This includes tool call counts, token usage, approval statistics, OS
type/architecture, and a persistent installation UUID for cross-session
correlation. **No user prompts, code content, or conversation content is
collected.**

Telemetry is enabled by default. To disable it for the current user, create
the per-user sentinel file:

```bash
mkdir -p ~/.copilot-shell
touch ~/.copilot-shell/telemetry_disabled
```

A system administrator can disable telemetry for all users on the machine by
creating the system-level sentinel file:

```bash
sudo mkdir -p /etc/anolisa
sudo touch /etc/anolisa/.telemetry_disabled
```

Either sentinel takes effect immediately for running processes; no restart is
required.

## Environment overrides

| Variables | Effect |
|---|---|
| `COSH_AI_PROVIDER`, `COSH_MODEL`, `COSH_OUTPUT_LANGUAGE` | Core provider, model, and response language |
| `COSH_APPROVAL_MODE`, `COSH_MAX_TURNS` | Core approval and per-request turn budget |
| `DASHSCOPE_API_KEY`, `OPENAI_API_KEY`, `OPENAI_BASE_URL` | OpenAI-compatible credentials and URL fallbacks |
| `ALIBABA_CLOUD_ACCESS_KEY_ID`, `ALIBABA_CLOUD_ACCESS_KEY_SECRET`, `ALIBABA_CLOUD_SECURITY_TOKEN` | Aliyun credential fallbacks |
| `COSH_SHELL_DEFAULT_SHELL`, `COSH_SHELL_ADAPTER`, `COSH_SHELL_ANALYSIS_MODE`, `COSH_SHELL_APPROVAL_MODE` | Interactive shell choices |
| `COSH_SHELL_INTEGRATION` | `native` or `enhanced` Shell integration for the next session |
| `COSH_SHELL_LOGIN_IDENTITY` | `on`/`off` real login identity for the next Enhanced login shell (default on) |
| `COSH_SHELL_LANG`, `COSH_SHELL_AI`, `COSH_SHELL_INPUT_WAIT_TIMEOUT_SECS` | Shell language, AI toggle, and input-wait timeout |
| `COSH_RECOMMENDATIONS_BASH_HISTORY` | Opt in to Bash-history recommendations |
| `COSH_LOG`, `RUST_LOG` | Log filtering (`COSH_LOG` wins) |
| `COSH_AUDIT_DIR` | Audit storage root |

Environment values take precedence when the relevant binary supports them.
Logs rotate daily under `~/.copilot-shell/logs/` and old files are kept for
seven days.
