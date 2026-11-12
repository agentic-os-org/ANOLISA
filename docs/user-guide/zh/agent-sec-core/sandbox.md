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

## Cedar 输入

同一个 `--permissions` 参数也接受使用 `seccore.system` schema revision 1 的原始 Cedar
文本，或现有 `{"language":"cedar","content":{"schema":{"schemaId":"seccore.system",
"revision":1},"text":"..."}}` 源码封装。格式按内容识别，不依赖文件名。原有 OS permission
JSON 行为不变；使用 Cedar 不会启用 PDP 调用或启动 daemon。

例如，创建 `/tmp/asc-demo` 后，将以下内容保存为 `permissions.cedar`：

```cedar
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/tmp/asc-demo" || resource.path like "/tmp/asc-demo/*" };
```

```bash
agent-sec-cli sandbox run --permissions permissions.cedar -- /bin/sh -c 'printf "sandbox\n" > /tmp/asc-demo/report.txt'
```

此示例允许广泛读取宿主文件，禁止 IP 联网。启动 Cosh 时使用相同参数，后接 `-- cosh`；
需要明确包含其可写状态目录，模型提供方需要联网时还须允许 `network.use`。不会自动补充运行依赖。

转换结果是粗粒度权限申请，不代表所有 Cedar 条件均得到等价执行：

- 文件读写支持字面路径、目录前缀 `/*` 模式及布尔组合。完整目录树须同时包含目录自身的
  等值条件及其后代模式。精确文件授权会检查本地对象；仅目录自身或仅目录后代的授权被拒绝。
- 任一获准写操作（`write`、`create`、`delete`、`rename`、`setattr`）映射为 `write`，
  包括读取与删除。因此，在仍有其他获准修改操作时，仅禁止 delete 不会移除 write 申请。
- 文件的 `context.input.program` 等值或不等值条件按满足条件的程序投影，所得访问权限
  适用于整个进程树。路径排除与无条件禁止仍然生效。
- `network.use` 映射为全网络 `full` 或 `off`。只有 forbid 时，不推导任何正向授权。
- 存在调用规则时，`file.exec` 和 `process.spawn` 检查初始命令及参数，不逐次限制后代
  程序启动，也不生成 noexec 规则。需要显式 noexec 时继续使用 OS permission JSON。
  在 PDP 流程中，这些输入检查发生在发送授权请求之前；PDP 通过 `MODIFY` 或 `STEP_UP`
  改写命令后，不再用输入 Cedar 重查改写结果。最终命令由 PDP 授权，`STEP_UP` 还须批准返回的决定。

主体特定规则、advice、不支持的文件条件（例如特定 chmod mode）、任意通配符和不可表示的
路径区域会被拒绝。输入上限为 4 MiB、1,024 条策略；区域求值预算同时计入条件大小。
收集路径和程序时即检查不同值的数量上限，先检查预算，再选择代表值和求值。
Cedar 文件读写条件中的路径和程序字面量最多为 4,095 个 UTF-8 字节；捕获路径及其祖先
另有 4 MiB 字符串总预算，每个不同路径均在复制前检查。意图候选封装不是权限文件：
须先处理 unresolved 要求及候选有效性，再选取其中的 Cedar 文本。

Cedar 文件读写条件中的路径字面量必须已经存在。准备时保留全部路径（包括排除项），解析文件、目录及
中间路径分量中的符号链接，再按 canonical 路径求值并合并权限。经别名的 allow 不会丢弃
对其目标的 forbid。可读别名保留给命令使用；若别名存在可写祖先则拒绝，因为祖先目录可被用于
在安装后重定向链接。后端仍拒绝无法安全安装的重叠别名。Cedar 显式路径指向多硬链接文件时，
会在合并权限前拒绝，因为 canonical 路径无法识别这些其他名称。完整子树的排除即使位于允许范围
之外也保留显式 deny；PDP 保留这些申请排除项，PEP 拒绝丢弃它们的决定，因此后端执行前仍会
检查并拒绝排除目录内的多硬链接文件。“除 `/work` 外全部禁止”这类补集仍依靠允许范围之外的
默认拒绝，不生成会同时阻止 `/work` 的根 deny。初始程序与工作目录必须和捕获的路径
一致；执行前再次核验已捕获的对象身份。

准备 PDP 请求的应用可用 `asc_pep_runtime::PermissionInput::parse(text)` 解析当次输入，
作为现有 `PermissionProvider` 传给 `PepRuntime::authorize` 或 `run`。JSON 与 Cedar 均
生成 `requested_permissions`；daemon 策略仍决定最终 `effective_permissions`。输入不会
作为授权策略上传到 PAP。`AGENT_SEC_COSH_SANDBOX_PERMISSIONS` 接受相同格式，每次 Hook
准备请求时重新读取文件。输入变化影响下一次请求，不改变已准备的决定或运行中的沙箱。
两个运行入口均通过 `PermissionProvider::prepare` 先绑定路径，再执行或请求授权。
`PermissionInput::resolve` 仅用于词法预览，不生成绑定对象的执行计划。

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

## Docker 后端（源码 Preview）

可选的 `sandbox run --backend docker --image <pinned-image>` 模式要求 Docker Engine；
默认 Linux 后端和上述前置条件不变。镜像提供可信基础文件系统，仅挂载明确获准的宿主
文件或目录；工作目录必须被授权覆盖。deny、noexec 和运行时保留路径会被拒绝，不做近似映射。
Docker 仅接受 OS permission JSON；Cedar 原文和 Cedar source envelope 必须使用 Linux 后端，
在调用 Docker 前即被拒绝。

Docker 挂载要求宿主 root、`CAP_SYS_ADMIN` 和内核 `open_tree`/`move_mount` 支持；
不自动提权，缺少能力即拒绝（包括通常的 rootless 部署）。源路径不能含符号链接。
后端逐级打开源对象，在持有的 FD 上核验授权身份，再挂载到 root 管理的
`/run/asc-sandbox-docker` 私有 staging，保留实时读写；即使启动前原路径被替换，
仍使用原来获准的对象。staging 禁止向工作负载暴露，且隔离后续挂载传播。

Docker 两种网络模式都阻断外部 Unix socket，包括授权目录内的 pathname socket 和宿主
网络命名空间中的 abstract socket。本地 stream/seqpacket socketpair 保留；datagram pair
和兼容 ABI 被阻断。Docker seccomp 仅支持原生 x86_64 和 aarch64。拒绝 socket、设备、FIFO
作为 bind 根，以及 `/run`、`/var/run`、`/var/lib/docker` 目标。只读挂载不等于获准调用 socket
API。这些限制不消除现有共享文件和硬链接边界。Cosh 授权 Hook 不选择 Docker 模式。

## 接入策略授权

需要授权的应用通过 `asc-pep-runtime::PepRuntime` 和 `DaemonAuthorizationClient` 接入，
再使用同一执行后端。[`authorized_sandbox` 示例](../../../../src/agent-sec-core/v2/crates/asc-pep-runtime/examples/authorized_sandbox.rs)
展示请求准备、执行或 ask 前的策略警告及回执。公开执行命令仍为 `sandbox run`；
授权接入通过库调用和 daemon RPC 完成，再使用同一本地执行后端。

通过现有 `policy create/update` 命令管理 Cedar 策略，策略文件为 Cedar 原文，见
[Policy 指南](policy-cli.md#cedar-policy-源码)。更新成功后，下一次授权请求使用新版本。

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
