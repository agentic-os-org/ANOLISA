# cosh-ng

[English](README.md)

cosh-ng 是一个以现有 Shell 为基础的 AI 原生终端。`cosh` 默认使用 Enhanced
Assisted 模式，保留隐式自然语言路由、Skills、审批卡片和可恢复 Agent 对话。
如果要求 bash 或 zsh 独占会话，不加载 Cosh Hook、不观察也不提供洞察，可以
在启动时选择 Native 集成。自动化或其他 Agent 集成仍可使用结构化 JSON 和
JSONL 接口。

## 为什么使用 cosh-ng

| 传统终端 | cosh-ng |
|---|---|
| 需要把意图翻译成命令 | 默认 Assisted 模式可混合自然语言和命令 |
| 自动化散落在脚本中 | 用 Skills 封装可复用工作流 |
| AI 上下文绑定在单个聊天窗口 | 按工作空间恢复 Agent 对话 |
| AI 操作难以检查 | 通过审批卡片和审计记录检查工具调用 |
| 不同发行版使用不同系统命令 | 用 `cosh-cli` 获得稳定、结构化的系统操作 |

交互程序、管道、重定向、任务控制、bash/zsh 配置和 `Ctrl+C` 都会在前台终端中
照常工作。

使用 Bash 执行隔离命令时，运行 `cosh --isolated -c '<command>'`。
Bash 无法禁用退出脚本，因此隔离模式下的 Bash login 调用会以状态码 2 拒绝。
详见 [Shell 启动选项](../../docs/user-guide/zh/user-entrypoint/cosh-ng/shell/interactive-mode.md)。

## 安装

在 Alibaba Cloud Linux 4 上，通过 ANOLISA CLI 和 RPM backend 把 cosh-ng
安装到 system 范围。

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
sudo "$HOME/.local/bin/anolisa" --install-mode system install cosh-ng --backend rpm
```

公共安装脚本可以合并上述步骤。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

后续升级或卸载也使用同一个入口。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --install-mode system --upgrade
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --install-mode system --uninstall
```

在 macOS arm64 上改用 user 范围：

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend raw --install-mode user
export PATH="$HOME/.local/bin:$PATH"
```

在 Alibaba Cloud Linux 4 上，也可以直接安装 RPM。

```bash
sudo yum install cosh-ng
```

当前发布的 Linux raw 契约无法覆盖所有已路由的发行版，因此不作为推荐的
Linux 安装路径。raw 包支持 macOS arm64，但依赖 Linux 的软件包和服务操作
不可用。源码构建仅供贡献者使用，请参阅
[开发者入门指南](../../docs/developer-guide/zh/cosh-ng/getting-started.md)。

## 30 秒开始使用

```bash
cd your-project
cosh
```

Enhanced Assisted 是默认模式。提示符上方的 `◇` 状态行表示 Cosh 可能在前台 Shell 执行前
分类并路由本次输入。

```text
◇
user@host:~/project$ git status
◇
user@host:~/project$ 分析这个服务为什么反复重启
```

在空提示符按 `Shift+Tab` 可切换到 Enhanced Shell-only。`◌` 状态行表示普通
输入交给 Shell，但仍可获得命令执行后的洞察。再次按下即可返回 Assisted。

如果会话要求完全不加载 Cosh Hook、不观察也不提供洞察，可显式启动 Native。

```bash
COSH_SHELL_INTEGRATION=native cosh
```

Enhanced 不会向子进程保留已导出的 Bash `PROMPT_COMMAND` 值。
各版本的属性限制及 Native 替代方式见
[Bash prompt 兼容边界](../../docs/user-guide/zh/user-entrypoint/cosh-ng/shell/overview.md#bash-prompt-兼容边界)。

```text
$ hello
bash: hello: command not found
```

用 `/auth` 选择 provider，用 `/help` 查看当前版本支持的命令。如果希望每次 Agent
调用工具前都等待确认，运行 `/mode approval recommend`。Shell 和 Core 的审批设置
统一使用 `recommend`、`auto` 或 `trust`。增强集成使用 cosh-core runtime 时，`/agent`
会打开一次性 Composer，可在开头指定 `/skill:<name>`，并添加经过验证的工作空间内
`@路径`引用。在 Composer 中输入 `/` 可浏览 slash 命令，用上/下键选择、Enter 执行。
需要填写参数时先按 Tab 补全；带参数或多行的草稿由 Enter 按原文提交。
普通 Shell prompt 保留原生路径补全。

`type = "aliyun"` 时，SysOM 自动优先使用可达的 VPC 端点。
设置 `ai.providers.<id>.sysom_endpoint` 可固定端点，`COSH_SYSOM_ENDPOINT` 优先级更高，
`base_url` 不参与 SysOM 选路。仅自动选中的 VPC 路由绕过系统代理，显式覆盖与公网回退
仍保留代理设置。详见 [provider 配置](../../docs/user-guide/zh/user-entrypoint/cosh-ng/core/providers.md)。

如果要在不进入交互式 Shell 的情况下运行本机已安装的 ACP Adapter，可以先检查
Adapter，再通过 stdin 发送 prompt。

下面的命令使用 ANOLISA 或 RPM 安装的 `cosh agent` launcher。源码构建或 unified build
只安装 Gateway binary，此时请使用 `cosh-gateway doctor`、`cosh-gateway run` 或
`cosh-gateway task`，其余参数保持不变。

```bash
cosh agent doctor --profile codex --workspace "$PWD"
printf '%s\n' 'summarize the current changes' | \
  cosh agent run --profile codex --workspace "$PWD"
```

首个版本只接受内置 `codex` 与 `claude-code` profile。对应的 `codex-acp` 或
`claude-agent-acp` executable 需要单独安装。COSH 在 runtime 中不会调用 `npx`，也不会
下载 Adapter。Permission callback 只在本地 controlling terminal 上提示；没有 TTY 或使用
`--permission deny` 时，COSH 会取消请求。Once-only decision 会以脱敏 evidence 形式记录到
private local state directory。

### 运行持久托管 Task

Linux 贡献者可以在 cosh-ng 源码根目录构建并启动独立的 development instance，
再进入已经连接该 instance 的 Shell。

```bash
./scripts/managed-task-dev.sh setup
./scripts/managed-task-dev.sh shell
```

默认 setup 会构建 debug binary，接纳当前目录的 canonical path，始终启用
Core，并且只在检测到已安装的 pinned Adapter 时加入 Codex。它不会下载
Adapter，会复用有效 `CODEX_HOME`，并在不回显值的情况下快照 allowlist 中的
environment variable。Core-only setup 只继承 8 种大小写 proxy variable。启用 Codex
后，setup 还会继承 Codex 文档变量、pinned Adapter 支持的变量，以及
`CODEX_HOME/config.toml` 声明且当前已设置的 provider variable。含凭据的 variable
会触发警告，因为 root-owned mode `0600` Gateway/Adapter environment 可能被同 UID
process 读取。默认不配置 checkpoint provider。该 profile 使用 `allow_all`，具有
持久的逐 effect decision。托管 Core 只提供 pinned workspace 内经过 approval 的
`write_file`；Codex 则在 package containment 内保留 service-user authority，并不受
workspace filesystem sandbox 限制。

Development 使用独立的 transient `cosh-gateway-dev@` unit、socket、state 与
environment file，不会覆盖 package file。Host 重启后需要重新运行 `setup`。
Production Gateway 正在运行时会默认拒绝，除非显式传入
`--stop-production`。状态查询、停止、清理、override 与完整安全边界请阅读
[用户手册](../../docs/user-guide/zh/user-entrypoint/cosh-ng/README.md)。

Package 只安装一个按账号命名的 `cosh-gateway@.service`。Core 始终可配置；Codex ACP
以及用于 Runtime 启动前 baseline 和 effect 前 barrier 的 checkpoint provider，都是同一
daemon、socket 和 SQLite database 的可选输入。不要再并行启动已经退役的
`cosh-gateway-acp@` unit。

Service 要求 root 管理的 environment file 提供准确 canonical workspace。Task workspace
与 private StateDirectory 分离，避免 Runtime 的访问范围扩大到 Gateway database 与 audit
state。要绑定当前项目，请创建该文件并启动 service。

```bash
sudo install -d -m 0755 /etc/cosh
printf 'COSH_GATEWAY_WORKSPACE=%s\n' "$(pwd -P)" | \
  sudo tee "/etc/cosh/gateway-$USER.env" >/dev/null
sudo chmod 0600 "/etc/cosh/gateway-$USER.env"
sudo systemctl enable --now "cosh-gateway@$USER.service"
```

Unit 把 Core `HOME` 固定为 `/var/lib/cosh-gateway-$USER/core-home`。User-level
provider config 放在
`/var/lib/cosh-gateway-$USER/core-home/.copilot-shell/config.toml`，也可以使用
`/etc/copilot-shell/config.toml` system configuration。

如需选择 Codex，请安装 pinned Adapter，并增加一个完整的可选参数。必需的 environment file
属于可信 operator configuration，应保持 root-owned。路径包含空格时，要把整个参数写成一个
systemd word。

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

如需显示 checkpoint 选项，请配置绝对 `ws-ckpt` socket。Security audit path 可选，并且
只能和 checkpoint socket 一起配置。Unit 不依赖 `ws-ckpt`，所以没有这些可选配置时，
Core-only 启动不会被阻塞。

```bash
sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
COSH_GATEWAY_CHECKPOINT_ARG=--checkpoint-socket=/run/ws-ckpt/ws-ckpt.sock
COSH_GATEWAY_SECURITY_AUDIT_ARG=--security-audit=/var/lib/cosh-gateway-$USER/security-audit.jsonl
EOF
sudo systemctl restart "cosh-gateway@$USER.service"
```

在 `cosh` 中，`/task` 会打开表单，依次选择 goal、Runtime（`Core (cosh-core)` 或
`Codex (ACP)`）与 checkpoint policy（`Auto`、`On` 或 `Off`）。`/task <目标>` 打开
同一表单并预填 goal。确认页会显示 canonical workspace 与持久默认审批策略
`allow_all`。不可用的 Runtime 不能选择；checkpoint provider 不可用时也不会提供 `On`。

```text
/task 升级依赖、修改代码并运行测试
/task
/task list
/task show
/task show <tsk_UUID>
```

提交后立即返回持久 Task ID。Gateway 与 Runtime 由 system service 持有，因此退出 Shell
或断开 SSH 不会取消 Task。重新登录后，用 `/task list` 或 `/task show [task-id]` 查看
持久进度与结果。Gateway restart 仍不能恢复 ACP session。对应 Run 会进入 suspended 或
lost，必须显式 retry，Gateway 不会静默重放 prompt。

Checkpoint policy 同时作用于 Runtime 启动前和获批的 Runtime-native effect 之前。
只有 provider 明确报告 unavailable 或 known-no-effect 时，`Auto` 才记录持久 downgrade；
error 或 uncertain result 不能授权 effect。`On` 只有在存在准确 checkpoint evidence 时才
放行，否则 fail closed；`Off` 既不创建 baseline，也不建立逐 effect barrier。

托管 Core 使用封闭的 `workspace-write-v1` profile，只提供 `ask_user_question` 与
`write_file`。每次写入都是 Runtime-native permission decision；只有 Gateway approval
以及所需 checkpoint barrier 完成后，Core 才会执行。现有 pinned workspace filesystem
会拒绝 parent traversal、workspace 外绝对路径与 symlink escape。该 profile 不准入 Shell、
edit、read、MCP、Skills 或 Hooks。

与 active Task 准确关联的 Codex permission callback 会收到 `allow_once`，COSH 不会创建
provider `allow_always` rule。Codex 原生 effect 不经过 Gateway broker，在 package systemd
containment 内使用 service user authority，并没有 workspace filesystem sandbox。Effect
前 barrier 只覆盖 ACP 确实上报 permission callback 的 effect；没有 callback 的 Codex 原生
effect 不在覆盖范围内。Unit 会把 system path 设为只读，使用 private `/tmp` 并隐藏
`/run/user`，因此这不表示不受限制的 host authority。Gateway 只持久化有界 Runtime
event，不会为 ACP 原生工具声称准确 side-effect receipt。

Task 运行时即可通过 Task-owned surface 列出、预览或比较其 proven-created snapshot。
切换只在 Task terminal 后可用，并会重新校验 preview 与 Task revision，先创建 pinned recovery
snapshot，再让 `ws-ckpt` 应用完整 ID。daemon 会在 workspace write lock 内、紧邻 rollback
前重新计算 live diff；generation 或 diff 变化会在 backend 执行前被拒绝。Task active、snapshot
不属于该 Task、preview 已过期或 workspace 被占用时都会 fail closed。

已确认创建的 recovery snapshot 会持续保留在列表中，即使后续切换失败或结果不确定。
大量差异的预览只返回有大小限制的前缀，并通过 `changes_omitted` 报告省略条数；
确认用的 digest 仍覆盖完整差异。

```bash
/task snapshots <task-id>
/task snapshot preview <task-id> <snapshot-id>
/task snapshot diff <task-id> <snapshot-id>
/task snapshot switch <task-id> <snapshot-id>
```

Automation 可以使用等价的 submission contract。

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

Task API 还支持 `append`、`cancel`、`retry` 与 `resolve-approval`。`doctor` 和 `run`
仍是独立且无 containment 的一次性 ACP interoperability command。本增量尚未重跑真实
Codex provider、SSH 断开流程与 systemd service，当前验收依据是 deterministic local
coverage。

实验性的 `cosh agent web` 命令仅在 Linux 上提供。它在打开 HTTP listener 前查询所选
Daemon 的 capabilities，并验证 canonical workspace 身份与 Runtime 权限。当前 Core/Codex
catalog 的两个条目都声明拥有本地用户权限，即使 token 位于工作区外也可读取，因此 Web
会拒绝启动。这个版本请通过 `/task` 或 `cosh agent task` 继续任务。原来的
`--capability-profile` 声明已移除，因为它从未限制 Daemon 的权限。平台限制与准入边界详见
[Web 命令参考](../../docs/user-guide/zh/user-entrypoint/cosh-ng/README.md)。

`SIGINT` 与 `SIGTERM` 会在 Daemon 退出前触发有界的 scheduler 与 Runtime shutdown。Daemon
仍然只监听 Unix socket，不开放 remote listener。

仓库为 direct ACP path 提供 Fake Adapter conformance coverage。具体安装在投入生产前，仍需
另行执行真实 Codex/Claude Adapter 检查与人工 Terminal 验收。

## 文档

- [用户手册](../../docs/user-guide/zh/user-entrypoint/cosh-ng/README.md)
- [接入 MCP server](../../docs/user-guide/zh/user-entrypoint/cosh-ng/mcp.md)
- [交互式终端](../../docs/user-guide/zh/user-entrypoint/cosh-ng/shell/overview.md)
- [配置](../../docs/user-guide/zh/user-entrypoint/cosh-ng/configuration.md)
- [管理系统操作](../../docs/user-guide/zh/user-entrypoint/cosh-ng/cli/overview.md)
- [Headless 集成](../../docs/user-guide/zh/user-entrypoint/cosh-ng/core/headless-mode.md)
- [开发者入门](../../docs/developer-guide/zh/cosh-ng/getting-started.md)
- [架构](../../docs/developer-guide/zh/cosh-ng/architecture.md)
- [贡献指南](CONTRIBUTING_zh.md)

## 数据采集

cosh-ng 会采集匿名的运行指标用于改进服务质量，包括工具调用次数、token 用量、
审批统计、操作系统类型/架构，以及一个持久的安装 UUID 用于跨会话
关联。**不采集用户输入内容、代码内容或对话内容。**

关闭当前用户的遥测：

```bash
mkdir -p ~/.copilot-shell
touch ~/.copilot-shell/telemetry_disabled
```

系统管理员也可以通过创建系统级哨兵文件，为整台机器上的所有用户关闭遥测：

```bash
sudo mkdir -p /etc/anolisa
sudo touch /etc/anolisa/.telemetry_disabled
```

## 参与贡献

源码构建主要面向贡献者，请从[开发者指南](../../docs/developer-guide/zh/cosh-ng/getting-started.md)
开始。

## 许可证

Apache-2.0
