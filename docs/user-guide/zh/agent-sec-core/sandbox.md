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
作为授权策略上传到 PAP。Cosh 使用下文的系统默认模板；内部准备入口仍支持当次 JSON/Cedar
权限输入，供权限提供层接入。输入变化影响下一次请求，不改变已准备的决定或运行中的沙箱。
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

请求通过 `context.agent.agent_type` 提供可选 Agent 分类，不要求 Agent 身份或 UID 登记。
类型由调用方自报，未经认证；本阶段不按身份隔离单个 Agent。

PAP 从全部当前策略推导请求策略集，按精确的
`schema: {schemaId: "seccore.system", revision: 1}` 引用校验。
纳入的源码中每条语句都必须有显式 principal header。当前请求使用
`principal is SecCore::Subject`，在条件中匹配 `agent_type`：

```cedar
permit(principal is SecCore::Subject,
       action == SecCore::Action::"process.spawn", resource)
when { principal has agent_type && principal.agent_type == "cosh" };
```

此规则只授权进程启动，还须补充应用所需的文件/网络策略。缺少类型时不会匹配此条件。
原生 JSON 和全部使用裸 `principal` header 的源码不进入此链路；仅在 `when` 中引用
principal 不算加入。单份存储策略混用裸 header 与显式约束会报错。
需要共同约束请求的每条规则，包括 permit 和 forbid，都必须使用显式 principal header。
单纯拆分不能保留共同约束语义：拆出的裸 principal forbid 不会进入请求授权集。
只有确实面向不同处理路径的规则才适合拆分。这是 ANOLISA 的加载约定，不是 Cedar 的启用字段。

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
请求提供 `principal.agent_type: "cosh"`；Cedar 策略须允许该类型及 `/bin/bash -c` 调用。
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
仍受上述 Cedar 映射限制约束：不存在的文件字面量会导致权限准备失败，不会隐式转为
父目录写权限。

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
