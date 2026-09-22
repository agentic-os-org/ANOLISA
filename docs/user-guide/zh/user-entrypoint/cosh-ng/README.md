# cosh-ng 用户手册

[English](../../../en/user-entrypoint/cosh-ng/README.md)

cosh-ng 是一个 AI 原生 Linux 终端，默认使用 Enhanced Assisted，也提供显式的
无 Hook Native 集成。先阅读快速开始，再按下面的任务导航查找所需功能或命令。

## 从这里开始

- [快速开始](QUICKSTART.md)：安装 cosh-ng 并完成第一个任务。
- [模型提供商](core/providers.md)：配置认证并选择模型提供商。
- [配置](configuration.md)：了解配置文件、设置项和优先级。
- [支持的平台](supported-distros.md)：确认软件包和服务后端。
- [Windows WSL2 预览](wsl-preview.md)：在 Windows 的 Git Bash 中启动 cosh-ng（社区预览）。

## 在终端工作

| 目标 | 继续阅读 |
|---|---|
| 在同一会话中使用 Shell 命令和自然语言任务 | [交互式终端](shell/overview.md) |
| 选择 Agent 工具调用何时需要确认 | [工具审批](shell/approval.md) |
| 恢复或压缩会话 | [会话恢复](shell/session-recovery.md) |
| 了解斜杠命令和按键行为 | [交互行为](shell/interactive-mode.md) |

## 添加可复用能力

| 目标 | 继续阅读 |
|---|---|
| 在项目或团队之间共享操作说明 | [Skills](core/skills.md) |
| 接入本地进程或远程服务提供的工具 | [接入 MCP 服务](mcp.md) |
| 打包 Skills、Hooks、设置和工具 | [Extensions](core/extensions.md) |
| 在 Agent 生命周期事件前后运行检查 | [Hooks](core/hooks.md) |

## 管理系统操作

先运行只读命令。对支持的包管理或服务变更先加 `--dry-run` 预览；这类操作通常需要 root 权限。

| 目标 | 继续阅读 |
|---|---|
| 查找、安装或删除软件包 | [软件包管理](cli/package-management.md) |
| 查看或修改 systemd 服务 | [服务管理](cli/service-management.md) |
| 使用现有的 `cosh-cli` 工作区快照命令 | [工作区快照](cli/checkpoint.md) |
| 查看策略决策和审计事件 | [安全审计](cli/audit.md) |

工作区快照页面描述 direct `cosh-cli` system-operations 路径。托管 Task 可以从已配置的
`ws-ckpt` provider 请求 Runtime 启动前 workspace baseline，以及每个获批 Runtime
permission effect 前的持久 barrier。`/task` 只列出持久归属于该 Task 的 checkpoint，
运行时即可 read-only preview、diff；recovery-protected switch 要求 Task terminal。

已确认创建的 recovery snapshot 会持续保留在列表中，即使后续切换失败或结果不确定。
大量差异的预览只返回有大小限制的前缀，并通过 `changes_omitted` 报告省略条数；
确认用的 digest 仍覆盖完整差异。

## 集成与自动化

`cosh agent` launcher 由 ANOLISA 与 RPM package 安装。源码构建与 unified build 只安装
Gateway binary；此时请替换为 `cosh-gateway doctor`、`cosh-gateway run` 或
`cosh-gateway task`，其余参数保持不变。

### 从源码构建快速启动托管 Task

在使用 systemd 的 Linux 上，贡献者和测试者可以从 cosh-ng 源码根目录准备独立的
development Gateway，再进入已连接该 Gateway 的 Shell。

```bash
./scripts/managed-task-dev.sh setup
./scripts/managed-task-dev.sh shell
```

命令接口如下。

```text
managed-task-dev.sh setup [--no-build] [--workspace ABSOLUTE_DIR] [--codex auto|off|required] [--environment inherit|off] [--checkpoint-socket PATH] [--stop-production] [--dry-run]
managed-task-dev.sh shell [--dry-run]
managed-task-dev.sh status [--dry-run]
managed-task-dev.sh down [--dry-run]
managed-task-dev.sh uninstall [--purge-state] [--dry-run]
```

默认情况下，`setup` 使用 debug profile 构建必需的源码 binary，并只接纳
`$PWD` 的 canonical form 作为 workspace。使用 `--no-build` 可复用现有 debug
artifact，使用 `--workspace ABSOLUTE_DIR` 可选择其他绝对 workspace。Setup 成功后
只会执行 Gateway capabilities smoke check，不会提交 Task。

Core 始终会被配置。默认的 `--codex auto` 只会在找到已安装的 pinned
`codex-acp` Adapter 时加入 Codex。Setup 绝不会调用 `npx`、下载 Adapter 或修改
已安装的 bundle。它复用调用用户的有效 `CODEX_HOME`。使用 `--codex off`
可只启用 Core，使用 `--codex required` 可在 pinned Adapter 未就绪时让 setup 失败。
复用 `CODEX_HOME` 也意味着复用其中的登录状态与配置。

默认的 `--environment inherit` 只复制 allowlist 中当前已设置的 variable，并保留
每个 variable 的当前值。Core-only setup 只复制 8 种大小写 proxy form：`HTTP_PROXY`、
`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`、`http_proxy`、`https_proxy`、
`all_proxy` 与 `no_proxy`。

启用 Codex Adapter 后，setup 还会复制以下当前已设置的 Codex 文档变量及
pinned Adapter 支持的变量：
`CODEX_SQLITE_HOME`、`CODEX_API_KEY`、`CODEX_ACCESS_TOKEN`、`OPENAI_API_KEY`、
`OPENAI_FEDERATION_RULE_ID`、`OPENAI_IDENTITY_TOKEN_FILE`、
`OPENAI_WORKLOAD_IDENTITY_CONTEXT`、`CODEX_CA_CERTIFICATE`、`SSL_CERT_FILE` 与
`RUST_LOG`。它会读取 `CODEX_HOME/config.toml`，并复制每个
`model_providers.*.env_key` 和 `model_providers.*.env_http_headers` 值指定且当前已设置的
variable。

Setup 不会复制整个用户 environment，不会通配继承所有 `CODEX_*` variable，也不会
自动继承 installer control、`LD_*`、`DYLD_*` 或 SSH variable。大小写 proxy form
会分别保留自己的当前值，含 userinfo 的 proxy URL 也会保留。Setup 和 status
只显示继承的 variable name，绝不显示值。复制含 userinfo 的 proxy、API/access token、
workload identity value 或 provider 声明的 variable 时，setup 会警告凭据已被快照到
root-owned mode `0600` Gateway/Adapter environment，并可能被同 UID process 读取。使用
`--environment off` 可以完全关闭快照。应当把生成的 environment 作为 private
configuration 保护。Proxy 或凭据值改变后请重新运行 `setup`，正在运行的 service
不会继承后续的 Shell 变化。

默认不配置 Checkpoint support。只在需要公开现有 `ws-ckpt` provider 时，才通过
`--checkpoint-socket PATH` 传入绝对路径且已存在的 Unix socket；否则 development
catalog 中没有 checkpoint provider。`Auto` 只针对这个已知 unavailable 状态记录明确的
持久 downgrade 并继续，`Off` 会跳过两个 checkpoint stage。没有 provider 时 Shell form
不提供 `On`；API 中请求 `On` 会 fail closed。Checkpoint error 与 uncertain outcome
绝不能授权 launch 或 effect。

该 development profile 使用持久 `allow_all` policy 进行本地源码测试。托管 Core 只提供
`ask_user_question` 与需要 approval 的 `write_file`；pinned workspace 会拒绝 traversal、
workspace 外 absolute path 与 symlink escape。关联的 Codex permission callback 会得到
单次允许，但 Codex 使用 service user authority，不受 workspace filesystem sandbox 限制。
提交前请检查 goal 与 canonical workspace，不要对不可信的 repository 或 prompt 使用该
profile。

该 helper 不会覆盖已安装的 package。它使用 `/run/systemd/system` 下的 transient
`cosh-gateway-dev@.service` template、`/run/cosh-gateway-dev-$USER.env` environment file、
`/run/cosh-gateway-dev-$USER/gateway.sock` socket、
`/usr/local/libexec/cosh-ng-dev/$USER` 下的 staged binary，以及
`/var/lib/cosh-gateway-dev-$USER` 下的持久 Task state。Unit 与 environment 不会跨开机保留，
所以 host 重启后请重新运行 `setup`。如果 package 安装的 production 或 legacy Gateway
正在为同一账号运行，setup 会拒绝且不会修改它。
只有在确实想停止 production 并把该账号切换到 development instance 时，才使用
`--stop-production`。`--dry-run` 可预览对应的 setup、Shell、status、shutdown 或
uninstall operation。

使用以下命令管理生命周期。

```bash
./scripts/managed-task-dev.sh status
./scripts/managed-task-dev.sh down
./scripts/managed-task-dev.sh uninstall
./scripts/managed-task-dev.sh uninstall --purge-state
```

`down` 会停止 transient instance，但保留它的 integration 与数据。`uninstall` 会移除
development integration，但保留持久 Task state，供后续 setup 复用。增加
`--purge-state` 还会删除该 development state。两种形式都不会卸载 cosh-ng，
也不会删除 production Gateway state。

- 运行 `cosh agent doctor --profile codex --workspace "$PWD"` 检查单独安装的
  `codex-acp`，也可以选择 `claude-code` profile 检查 `claude-agent-acp`。把有界 UTF-8
  prompt 通过管道传给 `cosh agent run` 即可执行一轮任务；增加 `--output jsonl` 可以获得
  稳定的流式事件。COSH 不运行 `npx`、不下载 package，也不接受任意 Adapter command。
  Permission request 使用 `/dev/tty`，stdin 只传递 prompt。默认的
  `--permission prompt` 只提供 `allow_once` 与 `reject_once`；没有 TTY、只有不支持的
  choice、遇到 EOF 或使用 `--permission deny` 时都取消且不授权。脱敏 append-only
  evidence 默认写入 `$XDG_STATE_HOME/cosh/gateway/permission-evidence.jsonl`，没有设置
  `XDG_STATE_HOME` 时使用
  `$HOME/.local/state/cosh/gateway/permission-evidence.jsonl`。可以用绝对路径
  `--permission-evidence PATH` 覆盖。COSH 只存储 digest 与 decision class，不保存 raw
  prompt、tool argument、option label、session identifier 或 workspace path。Evidence
  持久化失败时，callback 会被取消且本轮运行失败。这两个 direct ACP command 不受 durable
  Gateway Task Plane 治理，适合本地 interoperability。
- 对持久托管 Task，启动 package 唯一提供的 system-scope
  `cosh-gateway@.service`。必需的 root 管理 environment file 选择准确 canonical
  workspace。请将 workspace 保持在 service private StateDirectory
  `/var/lib/cosh-gateway-$USER` 之外，避免 Runtime 访问范围扩大到 Gateway database 与
  audit state。

  ```bash
  sudo install -d -m 0755 /etc/cosh
  printf 'COSH_GATEWAY_WORKSPACE=%s\n' "$(pwd -P)" | \
    sudo tee "/etc/cosh/gateway-$USER.env" >/dev/null
  sudo chmod 0600 "/etc/cosh/gateway-$USER.env"
  sudo systemctl enable --now "cosh-gateway@$USER.service"
  gateway_socket="/run/cosh-gateway-$USER/gateway.sock"
  ```

  Unit 把 Core `HOME` 固定为 `/var/lib/cosh-gateway-$USER/core-home`。User-level
  provider config 放在
  `/var/lib/cosh-gateway-$USER/core-home/.copilot-shell/config.toml`，也可以使用
  `/etc/copilot-shell/config.toml` system configuration。

  Service 始终传入 package Core executable。独立的可选 argument variable 可以把 Codex
  与 checkpoint 加入同一 daemon、socket、database 与 canonical workspace。空 variable
  会展开为零个 argument，所以省略可选参数不会阻塞 Core-only 启动。不要启动已经退役的
  `cosh-gateway-acp@` unit；unified unit 会与其冲突，避免两个 daemon 争用同一 state。
- 如需选择 Codex，请安装 pinned Adapter，并追加绝对 executable argument 与 Node path。

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

  Bundle 将 `@agentclientprotocol/codex-acp` 精确 pin 到 `1.6.2`，Gateway 会拒绝
  Adapter 上报的其他 identity 或版本。路径包含空格时，必须在可信 environment file 中把
  整个参数引为一个 systemd word。
- 如需启用 Runtime 启动前 baseline 与 permission-effect barrier，请追加绝对 `ws-ckpt`
  socket。Security audit argument
  可选，但不能脱离 socket 单独配置。

  ```bash
  sudo tee -a "/etc/cosh/gateway-$USER.env" >/dev/null <<EOF
  COSH_GATEWAY_CHECKPOINT_ARG=--checkpoint-socket=/run/ws-ckpt/ws-ckpt.sock
  COSH_GATEWAY_SECURITY_AUDIT_ARG=--security-audit=/var/lib/cosh-gateway-$USER/security-audit.jsonl
  EOF
  sudo systemctl restart "cosh-gateway@$USER.service"
  ```

  Gateway unit 不依赖 `ws-ckpt` service。它通过已配置的 admission 报告 checkpoint readiness，
  不会阻塞 Core-only 启动。
- 在 `cosh` 中，`/task` 与 `/task <目标>` 都打开 managed Task form，后者会预填 goal。
  Form 从 Gateway 获取 sealed launch catalog，只提供 ready Runtime，再选择 checkpoint
  policy。确认页会显示 goal、Runtime、canonical workspace、checkpoint 与持久默认审批策略
  `allow_all`。

  ```text
  /task 升级依赖、修改代码并运行测试
  /task
  /task list
  /task show
  /task show <tsk_UUID>
  ```

  提交会立即返回持久 Task ID。Service 持有 Gateway 与 Runtime child，所以关闭 Shell 或
  SSH 不会取消 Task。重新连接后使用 `/task list` 或 `/task show [task-id]` 查看持久进度
  与结果。Gateway restart 仍不能恢复 ACP session，对应 Run 会 suspended 或 lost，必须显式
  retry，不能重放 prompt。

  Policy 作用于 Runtime 启动前，以及每个获批 Runtime permission effect 前。只有 provider
  明确报告 unavailable 或 known-no-effect 时，`Auto` 才记录持久 downgrade；error 与
  uncertain outcome 会 fail closed。`On` 要求准确 checkpoint evidence，`Off` 既不创建
  baseline，也不建立逐 effect barrier。Workspace checkpoint 不保护 host、credential、
  network、cloud 或其他 external effect。

  托管 Core 使用封闭的 `workspace-write-v1` profile，只提供 `ask_user_question` 与
  `write_file`。每次写入都必须先经过 Runtime-native permission decision、适用的持久
  checkpoint barrier 与 Gateway approval，之后 Core 才执行。Pinned workspace 会拒绝
  traversal、workspace 外 absolute path 与 symlink escape。Shell、edit、read、MCP、Skills
  和 Hooks 都不准入。

  持久 `allow_all` policy 不会创建 provider `allow_always` rule。准确关联的 Codex callback
  收到 `allow_once`。逐 effect checkpoint barrier 只覆盖 ACP 确实上报的 permission effect；
  没有 callback 的 native effect 不在覆盖范围内。ACP native effect 在 systemd containment
  内使用 service user authority，
  不受 workspace filesystem sandbox 限制。Unit 仍会把 system path 设为只读，使用 private
  `/tmp` 并隐藏 `/run/user`，所以“local-user authority”不表示 unrestricted host authority。
  Gateway 持久化有界 reported event，但不声称 ACP native effect 的准确 receipt。

  Task 运行时可以检查 Task-owned snapshot；切换要求 Task terminal。

  ```bash
  /task snapshots <task-id>
  /task snapshot preview <task-id> <snapshot-id>
  /task snapshot diff <task-id> <snapshot-id>
  /task snapshot switch <task-id> <snapshot-id>
  ```

  Switch confirmation 默认选中取消。Task active、foreign 或 abbreviated ID、stale preview
  和 occupied workspace 都会被 Gateway 拒绝。切换前先把 cosh 与其他 shell process 移到
  workspace 外。daemon 会在 workspace write lock 内、紧邻 rollback 前重新计算 live diff，
  generation 或 diff 漂移会在 backend 产生 effect 前被拒绝。
- Automation 可以把 intent 传给同一 Task API。

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

  API 支持 `capabilities`、`submit`、`list`、`get`、`events`、`append`、`cancel`、
  `retry` 与 `resolve-approval`。Idempotency key 让 I/O 不确定后的重试保持安全。当前
  deterministic test 覆盖 launch selection 与 baseline policy；真实 Codex、SSH 断开和 package
  systemd execution 仍是与安装相关且尚未验收的 gate。
- 实验性的 Web continuation 命令**仅在 Linux 上提供**，目前**会拒绝内置 Core/Codex
  catalog**。请通过 `/task` 或 `cosh agent task` 继续 Task。macOS build 不包含 Web
  命令及其 Linux credential test，不会通过 `/proc/self/fd` 解析 credential。

  在 Linux 上，`cosh agent web --help` 展示以下选项：

  | 选项 | 契约 |
  |------|------|
  | `--socket PATH` | 覆盖 Gateway 本机 socket 的绝对路径；否则依次使用 `COSH_GATEWAY_SOCKET`、运行中的 package instance、用户 runtime directory。 |
  | `--workspace PATH` | 已存在目录的绝对路径，其 canonical path、device 与 inode 必须匹配 Daemon 准入的 workspace 身份。 |
  | `--token-file PATH` | 绝对路径、single-link regular file、权限 `0600`、由 root 或当前用户拥有，祖先目录可信，包含 32–256 个 printable ASCII token byte。打开的文件必须位于准入的 workspace 之外。 |
  | `--bind ADDRESS` | 仅支持 IPv4/IPv6 loopback；默认 `127.0.0.1:8765`。 |
  | `--output human\|jsonl` | 启动信息与错误的展示格式。 |

  准入检查在绑定 HTTP **之前**查询经本机身份验证的 Daemon capabilities。Daemon 不可用、
  launch schema 未知、workspace 不匹配、Runtime catalog 不完整、存在本地用户权限委派或
  非 brokered effect，都会产生可见的 `web_failed` 错误。Unavailable Runtime 条目同样接受
  检查：关闭新任务准入并不会移除历史 Task 的权限。`--capability-profile` 已移除，调用者的
  声明不能约束 Daemon 的权限。

  当前两个 Runtime 条目都声明拥有本地用户权限。把 token 放到工作区外无法与这些 Runtime
  隔离，因此这个版本没有可通过准入的生产 Web 配置。启用 Web 前，需要另行实现和验证受限
  Runtime 边界及对应的 Daemon attestation。这个限制不改变 Terminal、Task CLI 或 direct
  ACP 的流程。

  保留的 presentation adapter 实现了 Task list、基于 cursor 的 event、question、绑定 Task
  的 approval、cancel 与 retry。HTTP 契约要求通过 Authorization header 传递 Bearer token，
  mutation 使用新的 idempotency key；cookie 与 query token 会被拒绝。它不提供 TLS、OIDC、
  multi-user role 或 public listener。这些 route 只有通过启动准入后才可访问。
- [结构化 OS CLI](cli/overview.md)：命令域和安全的自动化方式。
- [输出格式](output-format.md)：`CoshResponse<T>` 成功和失败响应封装。
- [无界面模式](core/headless-mode.md)：供其他前端使用的 JSONL 集成。
- [Agent 工具](core/tools.md)：工具边界和审批行为。
