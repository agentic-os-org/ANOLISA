# 按明确权限运行沙箱命令

[English](../../en/agent-sec-core/sandbox.md)

`agent-sec-cli sandbox run` 根据调用方提供的文件和网络权限限制目标命令及其子进程，
在本地执行，无需 `agent-sec-daemon`。调用方选择权限，sandbox 在启动命令前安装这些限制。

## 运行要求

按[组件指南](index.md#安装)安装 Rust V2 CLI。默认 Linux 后端要求 Linux、非特权 user namespace，
以及支持 `--bind-fd`、`--ro-bind-fd`、`--cap-add` 和 `--unshare-user` 的非 setuid 系统
bubblewrap；支持发行版回移这些功能的版本。静态链接 `linux-sandbox` 须与 `agent-sec-cli`
安装在同一目录。noexec 规则还要求递归 `mount_setattr` 可用。
缺少前置条件时停止执行，不会退回宿主直接运行。

## 执行命令

使用支持 Landlock 的源码构建时，可以在 Linux 后端的 `--` 前加
`--landlock auto` 或 `--landlock required`。默认 `off` 跳过探测。
启用后，内核额外限制可写挂载和沙箱私有设备范围以外的写入。现有严格沙箱继续
实施读取范围、只读子树（如受保护的 Git 元数据）、noexec 和网络隔离。已打开的
文件描述符保留原有权限。

当 ABI 查询报告系统调用不可用、LSM 未启用，或只有 ABI 1–2 时，`auto` 继续用
原严格沙箱执行，并在标准错误输出中说明跳过原因。`required` 在这些情况下会在
命令启动前报错。ABI 达到 3 时，两种模式均要求规则完整安装。其他探测错误、
对象身份变化或安装失败都会阻止启动。

Cosh 的 all-shell 授权 Hook 从已安装的 `permissions.json.template` 同目录下的 `landlock.conf`
读取单个模式名。文件缺失时使用 `off`，沿用旧版 CLI 支持的参数。该文件须为 root 所有的普通文件，其父目录
也须为 root 所有，均不可由组或其他用户写入。固定权限和按命令生成权限两种模式
都会在准备新决定时读取配置。启用前请配套升级 Hook、CLI、daemon 和 helper。

`required` 在请求审批前先确认内核允许叠加一层 Landlock 规则，再在一次性沙箱中
安装本次写入规则。预检以只读方式挂载所有宿主路径，防止准备挂载点时改动工作区，成功后直接退出。
审批通过后，执行阶段在目标沙箱中重新核对路径并安装规则，然后启动命令。

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

已有目录 write 权限覆盖其子树内新文件和多层目录的创建，Cedar 输入中显式列出的缺失路径也适用，
不再额外要求 `create` 权限。

没有上述覆盖时，Cedar 对不存在路径允许 `create`，才申请其**已存在的直接父目录 write 权限**。
这会明确扩大权限，包含兄弟文件和后代的读取及全部修改操作，不提供精确文件创建或仅创建限制。
如果直接父目录缺失，且没有已有可写祖先覆盖目标，准备失败；报错包含目标与父目录路径，以及
`no existing writable ancestor covers the target`。应授权某个已有祖先目录 write，或先创建父目录再准备权限；
转换器不会自动为更高层祖先新增授权。准备阶段不创建文件，只绑定已有目录身份，不为不存在的对象
伪造身份。悬空符号链接及无法表示的排除规则仍会被拒绝。
已有文件仍按精确路径授权，JSON 路径仍须存在。PDP 使用未改变的 PAP 策略评估扩大后的目录申请；
直接 `sandbox run` 不经过 PDP，直接安装这份权限。

准备时保留已有路径（包括排除项），解析文件、目录及
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
作为授权策略上传到 PAP。Cosh 使用下文的系统默认模板；内部准备入口仍支持当次 JSON/Cedar
权限输入，供权限提供层接入。输入变化影响下一次请求，不改变已准备的决定或运行中的沙箱。
两个运行入口均通过 `PermissionProvider::prepare` 先绑定路径，再执行或请求授权。
`PermissionInput::resolve` 仅用于词法预览，不生成绑定对象的执行计划。

## 权限格式

| 字段 | 行为 |
|---|---|
| `version` | 必须为 `1`，包含子树例外的规则也使用此版本。未知版本和字段被拒绝。 |
| `filesystem.default` | 必须为 `deny`。 |
| `filesystem.rules[].effect` | `allow` 或 `deny`；deny 始终优先，不依赖条目顺序，也不能被更具体的 allow 覆盖。 |
| `filesystem.rules[].rights` | 非空且不重复的 `read`、`write` 列表。write 包含读取、创建、删除和重命名；禁止 read 也会禁止 write。 |
| `filesystem.rules[].excluded_paths` | 可选，仅用于 deny：从本条规则范围中排除已经存在的 canonical 后代目录。例外不授予权限，也不取消其他 deny。 |
| `execution.deny_paths` | 对指定文件或目录子树实施挂载级 noexec，包含范围内可见的子挂载。 |
| `network.mode` | `off` 禁止 IP 联网；`full` 使用宿主网络，不过滤目标地址、端口或方向。 |

路径必须已经存在，是规范化的绝对路径，不使用通配符、点路径段或尾部 `/`，根目录 `/` 除外。
文件路径选中单个对象，目录路径选中其子树。文件系统规则和例外路径合计最多 1,024 个边界，
`execution.deny_paths` 最多接受 1,024 项。可在获准写入的目录内创建新文件。命令需要的程序、加载器、库、工作目录和
数据都应在权限中声明，运行时不会自动补充授权。`/proc` 和 `/dev` 保留给 sandbox 基础设施，
不能作为明确的规则目标。

隔离会话时使用 `version: 1`，声明运行依赖的读取权限，并为当前会话显式授予读写权限。
再添加包含当前会话例外的 deny，例如以下文件系统规则：

```json
{"effect":"deny","path":"/sessions","rights":["read"],"excluded_paths":["/sessions/current"]}
```

准备前创建这两个目录，以 `/sessions/current` 为 cwd 运行。当前会话保持可写；合成父目录
仅允许路径穿越，不可枚举、不可修改；已经存在和运行期间新增的兄弟会话均不可见。
到达更深层例外所需的中间目录也仅可穿越。例外内的独立 deny 继续生效。
例外必须是互不重叠的严格后代目录；symlink 边界和不存在的目录会被拒绝。

当挂载计划需要在隐藏父目录下创建仅可穿越的合成目录时，所有被挂载的规则路径、
工作目录和解析后的可执行文件都必须使用不含符号链接分量的 canonical 路径。
否则会在挂载前拒绝准备；请在权限输入中使用解析后的路径。
该检查也适用于不含 `excluded_paths` 的输入。helper 在创建合成 tmpfs 时直接设置仅可穿越权限，
启动命令前核验实际挂载目标。这类计划要求内核支持 `openat2` 及 `statx` 的挂载根属性。
目标发生变化或无法核验时拒绝启动。

Cedar 用 `unless` 从匹配的 forbid 中排除当前子树，再由 permit 授权；更具体的 permit
不能覆盖无条件 forbid。输入投影和 PDP 决策都会保留这些范围。格式版本保持为 1，
不含例外的规则保持 deny 优先语义。使用例外需要支持 `excluded_paths` 的 CLI 和 helper；
PDP 授权还需要兼容的 daemon。旧组件会拒绝未知字段，不会忽略例外后继续执行。
降级前先停止使用包含例外的策略；普通子目录 allow 不能替代例外。

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

目标程序必须使用容器内的绝对路径；后端显式覆盖镜像 ENTRYPOINT，参数仅使用请求值，
空参数列表也不会继承镜像 CMD。启动前核验实际 executable、args、工作目录和安全选项，
拒绝额外挂载（包括镜像声明产生的匿名卷）；清理时一并删除该容器的匿名卷。
执行身份仍由可信镜像的 USER 决定，不自动映射宿主 UID。结束时要求容器已停止，
并交叉核验 attach 与容器退出码。

创建时启用 `--interactive`，启动前要求 `OpenStdin=true`；stdin/stdout/stderr 由前台
attach 转发，支持管道数据、空输入和终端行输入。EOF 会传给目标程序，不分配容器 TTY。
例如，在获准工作目录内，将 `IMAGE` 设为已缓存且包含 `/bin/cat` 的固定镜像引用，
使用仅含 allow 规则、无 `/` 根挂载且 `execution.deny_paths` 为空的 `permissions.json`：

```bash
printf 'hello sandbox\n' | agent-sec-cli sandbox run --backend docker --image "$IMAGE" --permissions permissions.json -- /bin/cat
```

应原样输出 `hello sandbox` 后退出；不要直接复用上文含 `/` 和 noexec 的 Linux 示例权限。

Docker 挂载要求宿主 root、`CAP_SYS_ADMIN` 和内核 `open_tree`/`move_mount` 支持；
不自动提权，缺少能力即拒绝（包括通常的 rootless 部署）。源路径不能含符号链接。
后端逐级打开源对象，在持有的 FD 上核验授权身份，再挂载到 root 管理的
`/run/asc-sandbox-docker` 私有 staging，保留实时读写；即使启动前原路径被替换，
仍使用原来获准的对象。staging 禁止向工作负载暴露，且隔离后续挂载传播。
只读授权使用 `--mount ... readonly,bind-recursive=readonly`，要求 Docker/runtime
强制递归只读（Linux 5.12+）；启动前核验强制选项，能力不足直接失败，不回退到普通 `:ro`。

`--docker-host` 仅接受本地 `unix:///绝对路径`，默认 `unix:///var/run/docker.sock`。
后端验证 endpoint 所有权、socket 类型及本地 root dockerd 身份，并核对 daemon 可见的
staging 对象；不支持远程 daemon 或未经验证的代理。标准 systemd socket activation
通过 root PID 文件和 dockerd 持有的监听 FD 交叉确认。每次运行使用独立受保护的空
Docker client 配置，所有调用固定同一 endpoint/config，不继承个人 context、proxy 或
registry credentials；镜像必须已在该 daemon 缓存，禁止自动 pull。

自定义 `--docker-cli` 应安装在可写工作区授权范围之外。可执行文件和解析路径时
经过的目录都必须由 root 所有，组和其他用户不能拥有写权限。如果授权的可写目录与 CLI 位于同一文件系统，可执行
文件必须只有一个硬链接。如果可写挂载暴露了可执行文件或解析配置路径时经过的任何目录，
guard 会拒绝启动。这也包括中间符号链接所在的目录，以及通过绑定挂载指向这些对象的别名。
前台连接、状态检查和清理都使用同一个已打开的可执行文件。

构建和安装会同时提供 `asc-sandbox-docker-guard`，由 CLI 自动从同目录启动，不设
setuid、不自动提权；库嵌入方可通过受信任的 `DockerConfig.guard` 指定安装位置。
guard 在独立进程组中持有 staging/config，创建前登记资源，管道 EOF 表示监督进程
失联。create 完成后才清理，避免迟到创建；父进程保留前台 attach 的终端和 stdio。
正常返回也要求 guard 确认容器已删除。清理最多重试 3 次，每次 Docker 调用限时 10 秒；
create 限时 60 秒，超时或删除无法确认时保留 staging/config 和 `recovery.json`，明确报告
未知状态。管理员按记录中的 `recovery_argv` 参数删除该容器，确认创建已结束且容器不存在
后，再卸载记录中的 staging 并清理目录。guard 自身被强杀、宿主故障或 daemon 长期不可达
仍可能留下资源；不能把这些情况视作清理成功。

网络权限 `off` 映射为 Docker `none`，`full` 映射为 `host`，后者共享宿主网络命名空间。
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

daemon 默认启用授权，无需独立授权文件或启动开关。PAP 中适用策略集为空时返回 DENY。
申请权限受 Cedar 限制，共用约束通过 PAP 管理的策略表达。

升级时，从 daemon 启动命令及 systemd drop-in 中移除 `--authorization-config` 和文件路径，
旧参数会被拒绝。旧配置中的 `environment_permissions` 和 `ttl_seconds` 不再读取。
使用新 daemon 前须核对并将必要约束迁移到 Cedar，不会自动转换。修改 unit 后重新加载
systemd 并重启 daemon。daemon 和授权客户端须同步升级：授权请求改用包含 principal、action、
resource 和 context 的公共 PARC 信封，旧 OS 载荷会被拒绝。

挂载 noexec 仍由请求中的 `execution.deny_paths` 明确指定。Cedar 的 `file.exec` forbid
约束本次启动，不能代替对子孙进程生效的 noexec 挂载；必要的 noexec 约束须保留在权限请求中。

请求通过 `context.agent.agentType` 提供可选 Agent 分类，不要求 Agent 身份或 UID 登记。
类型由调用方自报，未经认证；本阶段不按身份隔离单个 Agent。

使用 `seccore.system` revision 3 时，PAP 将完整 Cedar 原文交给 PDP。
PDP 按本次请求提供的可选 `principal.agentType` 求值。Sandbox 请求省略
`ownerUid`，因此 `principal has ownerUid` 为假。以下规则允许 Cosh 启动进程。

```cedar
permit(principal is SecCore::Subject,
       action == SecCore::Action::"process.spawn", resource)
when { principal has agentType && principal.agentType == "cosh" };
```

此规则只授权进程启动，还须补充应用所需的文件/网络策略。缺少类型时不会匹配此条件。
revision 1 和 2 沿用已有分流规则。每条语句都有显式 principal header 的源码才进入请求授权。
混用裸 header 和显式约束会被拒绝，原生 JSON 和全部使用裸 header 的旧版策略走下发路径。
使用旧字段名 `agent_type` 的客户端和 Cedar 策略须改用 `agentType`。

每次 evaluate 拉取一个一致的当前策略快照，摘要覆盖成员、版本、schema 和正文。
未变化时复用编译缓存，变化后整体验证成功才替换。读取、验证或运行兼容性失败时停止授权，
不回退旧策略。创建、删除和 header 资格变化在下一次请求生效；空集替换缓存并拒绝请求。
整个集合中匹配的 forbid 优先于 permit。Scope/Binding 的部署、固定版本和清理独立保持原状。

旧配置项 `baseline_policy`、`task_policy` 不再使用。调整旧请求策略时必须人工核对原来的
交集语义：新集合中的两份 permit 源码会合并授权。额外限制应表达为 forbid，或收紧所有
适用的 permit。已有部署策略不会自动改写。

PEP 须在 60 秒内一次性接收决定；接收后的审批等待、安装和运行不再因该期限失败。
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

安装配套的 Rust V2 运行时与 Cosh extension，启用上述 daemon 授权配置，并在 Cosh NG
中启用扩展。本源码修订新增全 shell 授权；正式发布前须使用包含本次改动的构建。
**不要同时开启 Cosh 的 sandbox 与 tokenless 能力**。若 tokenless 已启用，
先用 `/extensions disable tokenless` 禁用该扩展，再使用 sandbox 授权。tokenless 会改写
部分 shell 命令，Cosh 当前的 Hook 合并方式可能让该改写覆盖 sandbox 的执行交接。
本版本不自动检测或强制执行这一互斥要求。

启用全 shell 授权：

```bash
export AGENT_SEC_COSH_SANDBOX_ALL_SHELL=1
cosh
```

开关默认关闭，接受 `1` / `true` 和 `0` / `false`，忽略大小写与前后空白。
非法值报告配置错误并阻断 shell 执行。关闭时保留原有危险命令筛选、sudo 处理和沙箱路径。
开启后，所有已接入的 `shell` / `run_shell_command` 调用，包括 `python3 skill.py` 和
含 `sudo` 的原始命令，都进入策略授权。明确禁止命令仍可提前阻断；原生文件工具、MCP
及其他入口不在此 Hook 的覆盖范围内。

默认 `fixed` 模式使用 RPM 核心包自动安装的 `/etc/agent-sec/sandbox/permissions.json.template`，无需权限文件环境变量
或手工复制。源码安装遵循已有 `CONFIGDIR` 布局，并同步生成 Hook 使用的路径；标准 RPM
安装使用上述路径。raw 遵循其管理的配置根目录，默认路径为
`/etc/anolisa/sec-core/sandbox/permissions.json.template`，并按同一布局生成 Hook 路径。
生成的路径模块属于安装元数据，不提供用户覆盖入口。模板及父目录必须由 root 所有、不可由组或其他用户写入，路径不得含
符号链接。RPM 使用 `%config(noreplace)`；源码重复安装保留已有配置及权限模式。

默认采用 [Codex workspace-write](https://learn.chatgpt.com/docs/agent-approvals-security#protected-paths-in-writable-roots)
的可写范围及 Git 保护：允许当前运行身份可读的文件，只允许实际工作目录、`/tmp` 和绝对路径
`$TMPDIR` 下写入，禁止 IPv4/IPv6 网络。每个可写目录下已有的 `.git` 文件或目录只读，
包含其后代；`.git` 指针还保护其实际指向的 Git 目录。不会扫描嵌套仓库，也不解析
`commondir`。不按其他目录名称额外排除路径，也不因工作目录是 `/`、HOME 或 HOME 的祖先
目录而拒绝申请。PAP/PDP 可进一步收窄申请权限或拒绝执行。默认配置不隔离敏感文件读取、
继承的环境秘密或共享临时目录。

含符号链接的受保护路径、无效 Git 指针或不可用的绝对路径 `$TMPDIR` 会阻断；
Git 指针路径中的空白原样保留。这些默认值不能替代策略，也不表示两个沙箱后端完全相同。

管理员编辑该模板修改默认申请。`format_version` 为
`asc.sandbox.permissions-template.v1`；`permissions` 使用上述 OS 权限对象，仅允许路径
开头的 `${cwd}` 占位符。
`include_tmpdir` 控制是否读取命令环境中的 `$TMPDIR`（忽略空值和相对路径）；
`read_only_subpaths` 列出每个可写根目录下检查的目录名称，其中 `.git` 也支持指针文件，
缺失路径跳过。不执行 shell 表达式或任意环境变量替换。Rust prepare 每次在内存中展开并校验模板，
再通过现有权限提供层绑定路径。配置修改影响后续请求，不改变已经接受的调用。
固定模式不调用意图识别；两种模式均不向 PAP 发布候选策略。

PDP 仍决定允许、拒绝、修改或要求原生审批，有效权限不能超过申请。批准命令不等于开放
网络或授予宿主 root。不可用的提权能力在沙箱内失败，不剥离 `sudo`、扩大权限或无隔离重试。
请求提供 `principal.agentType: "cosh"`；Cedar 策略须允许该类型及 `/bin/bash -c` 调用。
会话 ID 不建立授权身份。现有 PDP 命令变换仍然支持；Hook 自身不在授权前改写原始命令。

模板缺失、无效或不可信，依赖缺失，daemon 不可用，授权超时及异常响应均报告可见错误并
阻止执行。交付的前置 Hook 显式设置 `fail_open=false`，并将 `require_output_when_env`
绑定全 shell 开关，须保持启用并使用支持此字段的配套 Cosh NG 构建。开关关闭时保留
extension 空输出兼容，开启或非法值时空输出阻断。Hook 在授权前还会检查宿主提供的
`COSH_HOOK_EMPTY_OUTPUT_POLICY=block` 信号；缺失或值不匹配时阻断并提示升级。这是当次
调用的内部元数据，不是用户设置，不应通过手工导出来使旧宿主看似兼容。本源码版本同时
包含检查的两端；相同包版本号的不同 Preview 构建不能仅靠版本字符串区分。
受管理的扩展安装会检查能力指纹；从系统目录直接加载的扩展在升级后不一定要求重新同意，
该机制不能替代宿主兼容要求。Cosh NG 必须在
PreToolUse 启动失败、崩溃、非零退出、空或无效输出、超时时阻断；忽略此类失败的旧宿主
不能提供该保证。prepare 子进程期限为 55 秒，外层 Hook 为 60 秒；finish 为 10 秒，外层
为 15 秒。外层超时终止进程组。原生 ask 在 prepare 之后，不占用其期限。运行时权限错误
作为执行失败返回；回执或 finish 失败报告命令可能已经执行，绝不重跑。其他 Hook 行为不变。

使用 `agent-sec-cli capabilities --agent cosh --capability sandbox --output json`
查看继承的环境配置；模式为 `all-shell`、`dangerous` 或非法开关对应的 `blocked`。
该视图不读取模板，也不证明 daemon 就绪、Hook 加载或宿主兼容。重启 Cosh 前将开关设为 `0` 可恢复
原有筛选。原权限文件覆盖变量已经删除；管理员默认权限改为维护安装的模板。

### 按当前命令生成权限

使用支持 `intent.propose_command` 的配套 CLI/daemon，并按[策略指南](policy-cli.md)
配置 daemon 的意图模型。启动 Cosh 前选择动态来源及明确的 Cedar 运行预设：

```bash
export AGENT_SEC_COSH_SANDBOX_ALL_SHELL=1
export AGENT_SEC_COSH_SANDBOX_PERMISSION_SOURCE=command
export AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS=/absolute/path/runtime.cedar
cosh
```

来源仅接受 `fixed`（未设置时的默认值）和 `command`，非法值阻断授权。这些设置仅在
全 shell 授权开启时生效，本身不会开启授权。已删除的
`AGENT_SEC_COSH_SANDBOX_PERMISSIONS` 在两种模式下均无效。

动态模式使用明确的运行预设，**替代**安装的固定 JSON 模板，不继承模板的 cwd/tmp
写权限或 Git 保护。所需限制须写入 Cedar 预设或适用的 PAP 策略。预设必须为原始 Cedar
或源码信封，与候选使用相同 schema，并提供该工作负载已知的 shell/加载器/库访问及
初始 `/bin/bash -c` 调用许可。目录授权包含后代；整个项目的授权即使在空候选时也已
申请其内容。不要因脚本内部行为未知而补充推测的写入或网络权限。OS permission JSON
不能作为动态运行预设，以免合并时丢失 Cedar 调用与网络限制。

Hook 在 sandbox 包装之前，将包含 `sudo` 的原始命令及规范化 cwd 交给
`intent propose-command --format json`，不传原始用户任务。上游须提前在 PAP 准备任务
限制，并确认其实际进入上述当前请求策略集。daemon 不再读取独立授权配置及其中旧的
`task_policy` 选择键。符合条件的策略会合并 permit，任务边界须使用显式 forbid 或约束
所有适用的 permit；仅创建未进入该集合的策略不会限制执行。

Hook 按 JSON 的 `outcome` 分支，不能仅凭退出码放行：

- `candidate`：展示 `caveats`，将 Cedar 与运行预设合并，复用转换、初始调用校验及
  路径绑定。任一来源的 forbid 均保留。
- 空候选不申请业务权限，仅由明确的运行预设提供许可；PDP 仍可拒绝或收窄。
- `needs_clarification`：说明缺失信息，并在授权前停止。
- 接口不可用、异常响应、转换失败及超时均阻断，不回退固定权限或无隔离执行。
  脚本内部行为未知只作为 caveat，不自动扩大权限。

Hook 响应完整保留 caveats；当前 Cosh 原生审批面板可能截短较长的 caveat 行，
本接入不保证在该面板内可以读到完整文本。

候选只是申请，不写入 PAP。PDP 根据当前策略集决定 `effective_permissions`，结果受申请权限约束。
仍受上述 Cedar 映射限制约束：候选与运行预设合并后，已有目录 write 覆盖缺失后代；没有覆盖时，
对不存在文件允许 `create` 才申请其已存在直接父目录的 write 权限。PAP 须授权该目录范围；
PAP 中的精确文件许可不会被扩大。

意图生成 RPC 期限 38 秒、子进程 40 秒；生成与 prepare 共用 55 秒，外层 Hook 60 秒。
每次授权 RPC 最多 5 秒，且不超过剩余预算扣除 1 秒预留后的三分之一。
finish RPC 为 8 秒、子进程 10 秒、外层 Hook 15 秒。原生 ask 与目标执行沿用原生命周期。

每次调用独享 mode 0600 的候选临时文件；prepare 结束、转换失败或子进程超时后均清理。
daemon 保管已接受的调用，后续审批、执行、finish 不重读候选文件；动态模式的 finish
无需固定模板，回执失败不重跑目标。将来源设回 `fixed`、确认安装模板有效并重启 Cosh，
即可恢复固定授权。本功能不采集任务、不自动发布任务策略、不路由多任务，也不接入
AgentSight/Gateway。

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
同一次调用只由 sandbox Hook 改写 shell 命令；使用 sandbox 能力时须保持 tokenless 禁用。
Cosh 不会自动强制执行该限制。
若其他前置 Hook 阻断，Cosh NG 不会通知 sandbox，命令也不会启动。名额在同一轮下次申请 sandbox 时释放；若本轮就此结束，名额会保留到 daemon 重启。等待用户审批不设时限。
工具超时且执行器已领取决定却未提交回执时，daemon 记为“执行结果未知”并释放名额。
内存中的交接在 daemon 重启后失效；异常退出可能留下未完成交接，受现有每 UID 容量限制。
已执行目标不会因回执失败而重跑。该接入不覆盖 Gateway/ACP 的其他执行路径。
