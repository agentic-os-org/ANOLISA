# cosh-ng 配置

[English](../../en/cosh-ng/configuration.md)

先使用默认值启动。交互式设置使用 `/auth`、`/mode` 和 `/config language`；需要持久化或共享设置时再编辑 TOML。

## 文件与权限

| 文件 | 读取者 | 作用域 |
|---|---|---|
| `/etc/copilot-shell/config.toml` | `cosh-core` 和审计 | 管理员默认值 |
| `~/.copilot-shell/config.toml` | `cosh-core` 和 `cosh-shell` | 用户设置 |
| `<workspace>/.copilot-shell/config.toml` | `cosh-core` | 项目运行偏好 |

Core 按系统 → 用户 → 项目的顺序合并配置。项目配置可以设置 Agent、Skill、会话、`active_model` 和回答语言偏好，但会忽略 `active_provider`、Provider 定义、MCP server、Hook 定义、`agent.approval_mode`、`agent.allowed_tools` 和项目审计设置。`cosh-shell` 只读取用户文件，不读取系统或项目文件。

普通用户运行 Cosh 时，使用 `/config language` 等交互式 Shell 命令保存设置，
需要由当前账号拥有配置文件或符号链接指向的目标文件。仅有组写权限无法完成保存。
Cosh 还需要能够打开配置锁，并在目标目录中替换配置文件。

如果用户配置指向共享文件或管理员所有的文件，请向管理员获取适用于当前账号的设置。
把这些设置另存为当前账号所有的私有副本，权限设为 `0600`，再将副本放到账号可写的
`~/.copilot-shell/` 目录中，文件名为 `config.toml`。如果原路径是符号链接，
用副本替换链接本身，并保留共享文件。然后重新运行 `/config language` 保存偏好。

ECS 免配接入（`aliyun` 且 `auth_source = "ecs_identity"`）始终使用固定模型
`auto`，`active_model` 和 Provider 的 `model` 均不能修改；选择该接入方式不会把
项目模型保存为用户配置。需要自选模型时，可用 `/auth` 配置其它 Provider；
默认 ECS 免配接入不需要先运行 `/auth`。

## 最小用户配置

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
level = "info"

[ui]
language = "auto"

[shell]
default = "auto"
integration = "enhanced"
login_identity = true
adapter_default = "cosh-core"
analysis_mode = "smart"
approval_mode = "auto"
status_symbols = false
```

优先使用环境变量或 `/auth`，不要把原始 secret 写入 TOML。Provider 选择见 [Providers](core/providers.md)。

使用 `skills.custom_paths` 添加优先采用的共享 Skill，或使用 `skills.system_paths`
以系统优先级添加已安装的技能集合。目录示例见 [Skills](core/skills.md#添加共享目录)。

## 审批与轮次预算

Core 审批模式适用于直接集成：

| 模式 | ReadOnly | FileEdit | Shell、network、MCP、external |
|---|---|---|---|
| `trust` | 执行 | 执行 | 执行 |
| `auto` | 执行 | 执行 | 询问 |
| `recommend` | 执行 | 询问 | 询问 |

Core 和 Shell 统一使用 `recommend`、`auto` 和 `trust`。已有的 `balanced`、
`suggest` 和 `strict` 会按 `recommend` 读取；非法配置值也会回退到
`recommend`。`agent.max_turns` 限制一次 Agent 请求（默认 `50`），
`max_tool_calls_per_turn` 默认 `10`。每次新 prompt 都会重新开始轮次预算。

## 会话与压缩

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

保持 `target_ratio <= trigger_ratio <= emergency_ratio`。压缩只改变模型可见的历史，持久化的完整 transcript 不变。设置 `auto_persist = false` 可关闭该进程的会话恢复。

## MCP 与其他可选部分

MCP client 只能定义在系统或用户配置中。每个 server 使用 `command`（stdio）或 `url`（Streamable HTTP）之一；省略 `allowed_tools` 表示全部工具，`[]` 表示不暴露工具。示例、OAuth 和生命周期命令见[接入 MCP server](mcp.md)。

只添加实际需要的 Shell 建议和健康检查：

```toml
[shell.recommendations]
enabled = true
bash_history = false

[health]
enabled = true
role = "web-server"
critical_mounts = ["/", "/var"]
expected_writable_mounts = ["/data"]
pressure_notices = false
network_capacity_notices = false
fs_event_notices = false
host_event_notices = false

[[health.services]]
name = "nginx"
expected = "active"
```

`integration` 支持 `native` 和 `enhanced`。默认的 Enhanced 从 Assisted
模式启动，提供基于 marker 的 Agent 路由和命令事件。在空提示符按
`Shift+Tab` 可在 Assisted 与 Shell-only 之间切换。Shell-only 保留
命令事件和执行后洞察，但把普通输入交给 bash 或 zsh。Native 把输入、Shell
选项、trap 和启动文件完全交给 bash 或 zsh，不进行 Cosh 观察，也不提供洞察。
`cosh` 在启动时读取集成值，修改后需要新建会话；非法值会显示错误并拒绝启动。

`status_symbols` 默认为 `false`。设为 `true`（或单次会话使用
`COSH_SHELL_STATUS_SYMBOLS=1`）后，每个 Enhanced 提示符上方会显示
`◇ `（Assisted）或 `◌ `（Shell-only）状态行。Native 会话不显示状态符号。
该值在启动时读取；非法值回退为 `false`。

`analysis_mode` 支持 `smart`、`auto`、`manual`；Shell 审批支持 `recommend`、
`auto`、`trust`。`health.services.expected` 支持 `active` 或 `inactive`。

`[health]` 的提示开关默认关闭，只控制是否在登录横幅弹出对应提示，不影响
诊断详情（`cosh doctor`）里的事实采集：

- `expected_writable_mounts`（默认 `[]`）：声明期望可写的挂载点。只有这些
  挂载点出现只读偏离时才弹提示；普通只读挂载仅进诊断详情。
- `pressure_notices`（默认 `false`）：开启后，当前 cgroup 内存额度吃紧且同组
  PSI 持续停顿时弹提示（J19），主机或当前组的内存/IO/CPU PSI 双窗持续偏高时
  弹提示（J20/J21/J22）。关闭时这些事实仍可在诊断详情查看。
- `network_capacity_notices`（默认 `false`）：开启后，conntrack 连接跟踪容量
  接近上限时弹提示（J23）；普通 90% 水位仅进诊断详情。
- `fs_event_notices`（默认 `false`）：控制文件系统事件提示（J24–J29：ext4
  错误/强制只读、journal abort/flush、XFS shutdown、块 I/O）。属渐进放量，
  默认关闭；文件系统事件的放量另需设备生命期证据，仅打开此开关并不授权
  默认曝光。
- `host_event_notices`（默认 `false`）：控制内核主机事件提示（J30 hung task、
  J31 soft lockup）。同属渐进放量默认关闭；开启是逐规则试点决定，不是默认开启。
- `[health.checks]`：按检查 ID 开关单项采集，例如 `"filesystem.mounts" = true`
  启用全部声明挂载的卷级统计（默认只查根挂载）。
- `[health.maintenance]`：声明维护/演练窗口，`enabled = true` 开启，
  `reason` 写明原因，`until_epoch_seconds` 可选（Unix 秒，省略则长期有效）。
  窗口内只抑制登录横幅的提示卡，事实采集与状态恢复照常进行；到期自动恢复。

文件句柄水位（R07）、pids 任务额度（R06）、内核硬件事件/进程 segfault/网卡
超时（E08–E10）首批仅进诊断详情，不弹提示。ext4/JBD2/XFS/块 I/O 故障链
（E02–E05，J24–J29）与 hung task/soft lockup 的近期完整记录（J30/J31）分别
由 `fs_event_notices` 和 `host_event_notices` 控制：两者默认关闭，未显式开启
前不弹提示，且文件系统事件的放量另需设备生命期证据。所有提示沿用 2 小时冷却、
恢复静默与 10 分钟复发门，登录健康路径总预算 300ms 不变。

`login_identity`（默认 `true`）让交互式 Enhanced 登录 shell 拥有真实的 login
身份：内层 bash 以登录 shell 启动（`shopt -q login_shell` == yes），像 `bash
-l` 一样读取 `/etc/profile` 和 `~/.bash_profile`。设 `login_identity = false`
可回退到旧的非 login 启动（`--rcfile`）。Bash 4 及以上版本在有界的真实启动能力
探测通过时走真实 login 身份路径；Bash 3.2（包括 macOS 系统 Bash）、探测失败，或父环境导出了
POSIX 启动无法导入的函数名时都会自动回退。fallback marker 仍只读取一次 login profile。

Enhanced Bash login 会在 login profile 和首轮用户 `PROMPT_COMMAND` hook 执行后、
第一个 Cosh prompt 边界前，一次性记录最终 PATH。该值只传给 Provider 子进程，不会
修改 Cosh 进程的全局 PATH。报告缺失、格式错误、超长、身份伪造或到达过晚时，Provider
继续使用继承 PATH。设置 `COSH_SHELL_BOOTSTRAP_PATH=0` 会同时关闭旧 PATH 探针和
这条 Bash 启动 PATH 交接；zsh 与 Bash non-login 的其他探针行为不变。

## 登录升级提示

登录时，`cosh-shell` 会在后台检查托管安装的 `cosh-ng` 是否有可用更新，并在发现更新时显示提示。该检查默认开启。将 `COSH_SHELL_UPGRADE_CHECK` 设为 `off`、`0`、`false` 或 `no`（不区分大小写），可在下一次会话禁用检查。设置 `COSH_SHELL_UPGRADE_CHECK_CACHE` 可覆盖缓存文件路径；默认路径为 `~/.copilot-shell/cosh/upgrade-check.json`。

如果 `cosh-ng` 由 `anolisa` 以 system 作用域、默认 raw 后端安装，只有 root 会话会显示升级提示。普通用户无法执行 system 作用域的更新检查（需要写入 `/var/cache/anolisa`），因此不会看到提示。如需手动检查，请运行 `sudo anolisa --install-mode system update cosh-ng --dry-run`。RPM 后端安装不受影响。

## 日志

`[logging].level` 同时设置 `cosh-shell` 与 `cosh-core` 的日志过滤级别
（默认 `info`）。解析顺序为：

1. 环境变量 `COSH_LOG`
2. 环境变量 `RUST_LOG`
3. `[logging].level`

`[ui].log_level` 仍被接受，作为存量配置的兼容别名；两者同时设置时
`[logging].level` 优先。级别仍为默认 `info` 时，`[ui].debug = true` 会把
有效级别提升为 `debug`。

取值是 `tracing` `EnvFilter` 表达式，支持按模块控制：例如
`COSH_LOG=cosh_core=debug` 只打开 cosh-core 的 debug 记录。

日志写入 `~/.copilot-shell/logs/` 下的 `cosh-shell.log.<日期>` 与
`cosh-core.log.<日期>`；目录不可写时降级输出到 stderr。保留策略为今天
加前两个 UTC 日期，且两个组件共享每用户 20 MiB 预算；预算耗尽后该进程
停止写日志（stderr 有提示），删除旧日志并重启会话后恢复。

## 审计设置

系统文件包含 `[audit]` 表时，以系统设置为准；否则使用用户设置。项目审计表会被忽略。

```toml
[audit]
mode = "best_effort"         # best_effort | required
retention_days = 30
max_disk_bytes = 1073741824
```

`retention_days` 和 `max_disk_bytes` 必须大于零。存储根目录默认为 `$XDG_STATE_HOME/cosh/audit` 或 `~/.local/state/cosh/audit`；设置绝对路径的 `COSH_AUDIT_DIR` 可以覆盖它。

## 遥测 opt-out

cosh-ng 会采集匿名的运营指标以改进服务质量，包括工具调用次数、Token
用量、审批统计、操作系统类型/架构，以及用于跨会话关联的持久化 installation
UUID。**不会采集用户 prompt、代码内容或对话内容。**

遥测默认开启。要为当前用户关闭遥测，创建用户级哨兵文件：

```bash
mkdir -p ~/.copilot-shell
touch ~/.copilot-shell/telemetry_disabled
```

系统管理员可以通过创建系统级哨兵文件，为整台机器上的所有用户关闭遥测：

```bash
sudo mkdir -p /etc/anolisa
sudo touch /etc/anolisa/.telemetry_disabled
```

任一哨兵文件创建后立即对运行中的进程生效，无需重启。

## 环境变量覆盖

| 变量 | 作用 |
|---|---|
| `COSH_AI_PROVIDER`、`COSH_MODEL`、`COSH_OUTPUT_LANGUAGE` | Core Provider、模型和回答语言 |
| `COSH_APPROVAL_MODE`、`COSH_MAX_TURNS` | Core 审批和单次请求轮次预算 |
| `DASHSCOPE_API_KEY`、`OPENAI_API_KEY`、`OPENAI_BASE_URL` | OpenAI-compatible 凭据和 URL 回退 |
| `ALIBABA_CLOUD_ACCESS_KEY_ID`、`ALIBABA_CLOUD_ACCESS_KEY_SECRET`、`ALIBABA_CLOUD_SECURITY_TOKEN` | Aliyun 凭据回退 |
| `COSH_SHELL_DEFAULT_SHELL`、`COSH_SHELL_ADAPTER`、`COSH_SHELL_ANALYSIS_MODE`、`COSH_SHELL_APPROVAL_MODE` | 交互式 Shell 选择 |
| `COSH_SHELL_INTEGRATION` | 下一次会话使用 `native` 或 `enhanced` Shell 集成 |
| `COSH_SHELL_LOGIN_IDENTITY` | 下一次 Enhanced 登录 shell 是否启用真实 login 身份，`on`/`off`（默认 on） |
| `COSH_SHELL_BOOTSTRAP_PATH` | 设为 `0` 可关闭启动 PATH 探测和 Bash login PATH 交接 |
| `COSH_SHELL_LANG`、`COSH_SHELL_AI`、`COSH_SHELL_INPUT_WAIT_TIMEOUT_SECS` | Shell 语言、AI 开关和输入等待超时 |
| `COSH_SHELL_UPGRADE_CHECK` | 登录升级提示检查；默认开启，设为 `off`、`0`、`false` 或 `no` 可禁用 |
| `COSH_SHELL_UPGRADE_CHECK_CACHE` | 覆盖登录升级提示缓存路径（默认 `~/.copilot-shell/cosh/upgrade-check.json`） |
| `COSH_RECOMMENDATIONS_BASH_HISTORY` | 允许使用 Bash history 生成建议 |
| `COSH_LOG`、`RUST_LOG` | 日志过滤（`COSH_LOG` 优先） |
| `COSH_AUDIT_DIR` | 审计存储根目录 |
| `COSH_CORE_APPROVAL_TIMEOUT_SECS` | Core 审批响应超时（秒），1 到 2592000；非法值告警并回退默认 |
| `COSH_CORE_PATH` | 内置 shell 适配器使用的 `cosh-core` 二进制路径 |
| `COSH_CLAUDE_MAX_BUDGET_USD` | 传给 `claude` 适配器的花费上限（默认 `1.00`） |
| `COSH_AGENT_START_TIMEOUT_SECS`、`COSH_AGENT_IDLE_TIMEOUT_SECS`、`COSH_AGENT_APPROVAL_WAIT_TIMEOUT_SECS`、`COSH_AGENT_HARD_TIMEOUT_SECS`、`COSH_AGENT_CANCEL_GRACE_MS` | Agent 看门狗超时：启动 20 秒、空闲 90 秒、审批等待 600 秒、硬超时 600 秒、取消宽限 2000 毫秒；零或无法解析的值回退默认 |
| `COSH_AGENT_STDERR_TAIL_BYTES` | 失败报告保留的 agent stderr 字节数（默认 4096） |
| `COSH_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS` | 待定 shell 证据关闭前的空闲窗口 |
| `COSH_SHELL_HANDOFF_TIMEOUT_SECS` | Enhanced shell 交接超时（秒） |
| `COSH_SHELL_DEBUG` | Shell 调试开关；当日志级别仍为默认值时提升为 `debug` |
| `COSH_SHELL_RAW_SHELL` | `cosh --raw` 使用的 shell 二进制 |
| `COSH_SHELL_RENDER` | 设为 `plain` 或 `text` 关闭样式化渲染 |
| `COSH_SHELL_STARTUP_BANNER` | 显示启动横幅（`1`/`true`/`yes`/`on`/`always`）；其余值关闭，默认按终端检测 |
| `COSH_SHELL_STARTUP_HOOKS` | 运行启动钩子（`1`/`true`/`yes`/`on`/`builtin`/`built-in`）；默认关闭 |
| `COSH_SHELL_ANIMATION` | 动画指示：`always` 或 `never` |
| `COSH_SHELL_WIDTH` | 渲染宽度覆盖（限制在支持范围内） |
| `COSH_SHELL_HEALTH_SCAN` | 健康扫描模式：`0`/`off`/`false`/`disabled` 关闭，`fixture:<路径>` 加载夹具，其余值执行实时扫描（启动扫描仅 Linux） |
| `COSH_RECOMMENDATIONS_ENABLED` | `1`/`true`/`yes`/`on` 强制开启建议；`0`/`false`/`no`/`off` 强制关闭建议，即使配置已启用 |
| `COSH_SLASH_VIA_SHELL` | 精确斜杠命令经由 Bash history 路由；`0` 保持内置拦截路径 |
| `COSH_AUDIT_LOG` | 审计日志文件路径覆盖 |
| `COSH_AUDIT_POLICY` | 审计策略 TOML 文件覆盖 |
| `COSH_SYSOM_VPC_PROXY_HOST`、`COSH_SYSOM_PROBE_TIMEOUT_MS` | SysOM 端点探测：VPC 代理主机与探测超时（毫秒） |
| `COSH_METADATA_HOST` | 区域检测使用的 ECS metadata 端点（默认 `100.100.100.200:80`） |
| `COSH_SHELL_PROJECT_TRUST_STORE` | 项目信任存储路径覆盖 |
| `COSH_SHELL_HOOK_FEEDBACK_STORE` | 钩子反馈存储路径覆盖 |
| `COSH_SHELL_HEALTH_SUPPRESSION_STORE` | 健康抑制存储路径覆盖 |
| `COSH_GATEWAY_EXECUTABLE` | `/task` 使用的 `cosh-gateway` 二进制绝对路径 |
| `COSH_CLI_BIN` | `/audit` 调用的 `cosh-cli` 二进制 |

相关二进制支持时，环境变量优先于配置文件。日志在 `~/.copilot-shell/logs/` 下按日轮转，保留策略见[日志](#日志)。

面向测试的路径覆盖（`COSH_STATES_DIR`、`COSH_TELEMETRY_DISABLED_PATH`、
`COSH_INSTALLATION_ID_PATH`、`COSH_SLS_LOG_PATH`、`COSH_SLS_TRACK_URL`、
`COSH_METADATA_PROBE_TIMEOUT_SECS`）用于隔离测试中重定位状态、遥测与
metadata 探测；正常安装无需设置。
