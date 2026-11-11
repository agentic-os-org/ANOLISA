# 按明确权限运行沙箱命令

[English](../../en/agent-sec-core/sandbox.md)

`agent-sec-cli sandbox run` 根据调用方提供的文件和网络权限限制目标命令及其子进程，
在本地执行，无需 daemon。调用方选择权限，sandbox 在启动命令前安装这些限制。

## 运行要求

按[组件指南](index.md#安装)安装 Rust V2 CLI。此命令要求 Linux、非特权 user namespace，
以及支持 `--bind-fd`、`--ro-bind-fd`、`--cap-add` 和 `--unshare-user` 的非 setuid 系统
bubblewrap；支持发行版回移这些功能的版本。静态链接 `linux-sandbox` 须与 `agent-sec-cli`
安装在同一目录。noexec 规则还要求递归 `mount_setattr` 可用。
缺少前置条件时停止执行，不会退回宿主直接运行。

## 执行命令

下面的示例允许读取宿主文件，只允许写入 `/tmp/asc-demo`，禁止 IP 联网和从该目录直接执行。
为了覆盖运行依赖，示例中的根目录读取授权较宽；限制实际工作负载时，应替换为应用所需的明确读取路径。

在下面命令创建的目录中，将这份 `OsPermissionSet` JSON 保存为 `permissions.json`：

```json
{
  "version": 1,
  "filesystem": {
    "default": "deny",
    "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]},
      {"effect": "allow", "path": "/tmp/asc-demo", "rights": ["read", "write"]}
    ]
  },
  "execution": {"deny_paths": ["/tmp/asc-demo"]},
  "network": {"mode": "off"}
}
```

```bash
mkdir -p /tmp/asc-demo
cd /tmp/asc-demo
# Save the JSON above as permissions.json.
agent-sec-cli sandbox run --permissions permissions.json -- /bin/sh -c 'printf "sandbox\n" > report.txt'
cat report.txt
```

`--permissions` 必填，最多读取 4 MiB。程序与参数放在 `--` 后。
命令使用调用方的工作目录和环境变量；环境变量名称和值必须是有效 UTF-8。

## 权限格式

| 字段 | 行为 |
|---|---|
| `version` | 必须为 `1`；未知字段被拒绝。 |
| `filesystem.default` | 必须为 `deny`。 |
| `filesystem.rules[].effect` | `allow` 或 `deny`；deny 始终优先，不依赖条目顺序，也不能被更具体的 allow 覆盖。 |
| `filesystem.rules[].rights` | 非空且不重复的 `read`、`write` 列表。write 包含读取、创建、删除和重命名；禁止 read 也会禁止 write。 |
| `execution.deny_paths` | 对指定文件或目录子树实施挂载级 noexec，包含范围内可见的子挂载。 |
| `network.mode` | `off` 禁止 IP 联网；`full` 使用宿主网络，不过滤目标地址、端口或方向。 |

路径必须已经存在，是规范化的绝对路径，不使用通配符、点路径段或尾部 `/`，根目录 `/` 除外。
文件路径选中单个对象，目录路径选中其子树。`filesystem.rules` 和 `execution.deny_paths`
各最多接受 1,024 项。可在获准写入的目录内创建新文件。命令需要的程序、加载器、库、工作目录和
数据都应在权限中声明，运行时不会自动补充授权。`/proc` 和 `/dev` 保留给 sandbox 基础设施，
不能作为明确的规则目标。

只读不代表不可执行。noexec 阻止经该挂载直接执行，不阻止获准解释器读取脚本、经其他获准别名执行，
也不阻止执行另一位置的副本。文件系统 deny 路径必须是 canonical 路径；后端拒绝经过 symlink 的
deny 路径、重叠的显式挂载别名，以及显式禁止范围内的硬链接文件。noexec 路径也必须是 canonical 路径。
宿主文件权限仍然生效。

写权限限制的是路径，不是同一文件的全部名称。可写目录中预先存在的硬链接仍可能修改
写权限范围外的文件。为保持启动可用性，后端不扫描可写路径中的硬链接。当 `/` 不可写
且计划包含可写路径时，每次实际启动 strict helper 都向 stderr 输出一条 warning 后继续执行。
即使没有硬链接也会提示；这是隔离边界说明，不是检测结果，也不保证外部文件内容受到保护。
计划预检不会重复输出；纯只读计划和授予 `/` 写权限的计划不输出此提示。

需要内容隔离时，应使用独立的文件副本。其他宿主进程仍可修改共享目录树；此 warning
不会阻止并发创建硬链接或替换目录项。上述显式 deny 检查继续生效，仍可能拒绝执行。

## 执行行为

两种网络模式都保留进程隔离、权限降级和基础 seccomp 限制。socket 类型的 stdio 被拒绝；
调用方提供的终端、管道和普通文件 stdio 保留为数据通道。其他继承 FD 和外部 Unix socket 连接
被阻断；本地 stream、sequenced-packet Unix socket pair 仍可用于子进程 IPC。

CLI 返回目标退出码；收到信号退出报告时，返回 `128 + signal`。它不改变调用方的信号处理，
不设执行时限，也不提供主动取消接口。helper 启动后须在 30 秒内完成隔离安装。
准备或安装失败时目标不会启动，也不会在 sandbox 外重试命令。

主命令结束时清理 sandbox 后代。父进程退出时终止直接 helper 和已建立的隔离进程树。
bubblewrap 初始化期间，父进程被终止可能留下尚未启动目标的安装子进程；这一清理竞态仍是已知限制。
实现与验证边界见[执行设计](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)。

## 接入策略授权

需要授权的应用通过 `asc-pep-runtime::PepRuntime` 和 `DaemonAuthorizationClient` 接入，
再使用同一执行后端。[`authorized_sandbox` 示例](../../../../src/agent-sec-core/v2/crates/asc-pep-runtime/examples/authorized_sandbox.rs)
展示请求准备、执行或 ask 前的策略警告及回执。公开执行命令仍为 `sandbox run`；
授权接入通过库调用和 daemon RPC 完成，再使用同一本地执行后端。

通过现有 `policy create/update` 命令管理 Cedar 策略，策略文件使用
[Policy 指南](policy-cli.md)中的源码封装。更新成功后，下一次授权请求使用新版本。

通过 `agent-sec-daemon --authorization-config /etc/agent-sec-core/authorization.json`
启用授权。此 root 配置只选择 PAP 策略 ID 和运行参数，不保存 Cedar 策略正文，也不接受策略快照文件：

```json
{
  "baseline_policy": "<policy ID returned by PAP>",
  "environment_permissions": {
    "version": 1,
    "filesystem": {"default": "deny", "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]}
    ]},
    "execution": {"deny_paths": []},
    "network": {"mode": "off"}
  },
  "ttl_seconds": 60
}
```

可选 `task_policy` 指定另一份 PAP 策略 ID。两份策略均使用
`schema: {schemaId: "seccore.system", revision: 1}`，分别求值。`environment_permissions` 限制每次 `type: "os"` 请求可获准的权限，
由沙箱执行器约束此次启动的进程树。AgentSight 通过 Scope/Binding 选择要保护的主机进程。
配置及祖先目录须由 root 管理，不可被组或其他用户写入；修改策略选择或运行参数后重启 daemon。

每次 evaluate 从 PAP 一起读取所选策略的当前版本；版本未变时复用已编译结果。
策略缺失、被删除或无法用于授权时停止，不回退旧策略。已有 Scope/Binding 继续使用各自固定版本。

PEP 须在 ttl_seconds 内一次性接收决定；接收后的审批等待、安装和运行不再因该期限失败。
策略更新影响后续请求，已接收决定仍绑定原命令和权限；拒绝或执行失败后不能复用决定。

`authorization.evaluate` 返回 ALLOW、DENY、MODIFY、STEP_UP 或 DEFER，不执行命令。
STEP_UP 在本地 ask，通过后直接执行，不再次 evaluate。事实齐备时，PDP 在同一次求值中完成
修改后命令与权限的授权，PEP 本地核验后执行；权限收窄也返回 MODIFY。DEFER 结束本次申请。
策略中的 warn advice 通过 warnings 返回，不要求审批，也不新增第六种决策。示例在执行或 ask 前显示警告，
并将其放入 JSON 结果；daemon 同时把警告记录到执行回执事件。
`authorization.report` 检查决定引用、调用方 UID 和结果一致性后，将执行结果记录到已有
SQLite/JSONL 安全事件存储。Ack 表示全部必需写入完成，不代表命令成功；部分写入也可能
导致上报失败。上报失败不重跑命令。待上报决定仅保存在进程内，daemon 重启后丢失。
未配置授权接入时这些 RPC 返回 `unavailable`；直接 `sandbox run` 仍然可用。

schema、组件分工和验证范围见[实现设计](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)。

## 在 Cosh NG 中启用授权

先启用上述 daemon 授权配置，并确认 ASC 扩展已在 Cosh 中启用。为目标工作目录准备
完整的 `OsPermissionSet` 文件，然后在启动 Cosh 的环境中设置：

```bash
export AGENT_SEC_COSH_SANDBOX_PERMISSIONS=/absolute/path/permissions.json
cosh
```

这会把现有 guard 命中的 shell 命令切换到 PDP 授权；不扩大命令匹配范围，也不接管原生文件工具。
未设置该变量时保留原行为。设置后，权限文件无效、daemon 不可用或授权失败均阻断，不回退旧沙箱或无隔离执行。
请求使用 `agent_type: "cosh"` 和 `agent_id: "cosh:<session_id>"`，Cedar 策略须允许相应主体及 `/bin/bash -c` 调用。

Hook 通过私有 `sandbox hook prepare` 入口准备权限申请。ALLOW/MODIFY 替换为获准命令的执行入口；
STEP_UP 使用 Cosh 原生 ask，warn 显示为通知。daemon 保管完整请求和决定，包装命令只携带决定 ID。
`authorization.claim` 的 accept 阶段在期限内接收决定；execute 阶段仅向同一 UID 转交一次，
不会再次求值。审批后按原权限执行；拒绝时 abandon 并上报未执行回执。

沙箱审批将最终命令缩进并独立成段，下面只列出需要确认的资源操作及策略原因，不显示工作目录。
操作使用“读”“写”短标签；目录权限包括后代，写权限包括读、新增、修改和删除。文件与目录按本地
路径信息区分，无法确定类型时显示“路径”。排除的子区域仍会列出。提示不展开自动获准权限或独立
告警。仅执行审批只显示命令和原因，不列资源项。批准仍一次性接受完整的固定执行，不能逐项授权。

每项原因取对应 Cedar `escalate` advice 的 `message`，保留原语言；未提供时显示通用确认原因。
机器 `reason` 原因码仍保留在决定中，不作为面向用户的解释。命令改写前遗留的审批要求会标为
“原请求”原因，不会冒充替换后命令申请的资源权限。旧决定缺少逐项来源时，只显示命令与合并原因。
审批模板目前使用中文，Cosh 的 `ui.language` 不会翻译 Hook 消息。路径和参数完整保留，控制字符
会转义。换行和导航由 Cosh 控制，较长或较多的事项仍可能超出其审批面板。

非审批沙箱通知仍保留最终命令和目录，方便其他 Hook 要求审批时核对；Cosh 主预览显示的是内部
交接命令。告警仍保留在决定、回执和非审批通知中。无需审批的策略调整仍显示发生变化的权限类别。

沙箱告警本身不要求审批，其他已启用的 Hook 仍可要求批准。例如，sandbox-guard 带告警放行时，
code-scanner 仍可针对递归删除请求确认。这类审批应当保留。

本授权接入面向 Cosh NG，其 PostToolUse 在工具成功或失败后均会触发。
同一次调用只由 sandbox Hook 改写 shell 命令。原版 Cosh 不保证多个改写之间的顺序。
若其他前置 Hook 阻断，Cosh NG 不会通知 sandbox，命令也不会启动。名额在同一轮下次申请 sandbox 时释放；若本轮就此结束，名额会保留到 daemon 重启。等待用户审批不设时限。
工具超时且执行器已领取决定却未提交回执时，daemon 记为“执行结果未知”并释放名额。
内存中的交接在 daemon 重启后失效；异常退出可能留下未完成交接，受现有每 UID 容量限制。
已执行目标不会因回执失败而重跑。该接入不覆盖 Gateway/ACP 的其他执行路径。
