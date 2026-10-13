# Hermes 原生 Hook 适配器

[English](hermes-adapter.md)

Hermes 适配器通过框架原生 Plugin 和 shell Hook API，将已有本地 profile
接入 AW 服务。本期支持 Linux 上官方 `NousResearch/hermes-agent` 提交
`952c941e741e922a9be8fc403c8944c6e96318bb` 的 Python `chat` 入口。
Gateway、ACP、Desktop 和独立 TUI 入口尚未支持，使用 `.container-mode` 将 CLI
转入托管容器的 profile 也会被拒绝。
AW 加入原生 `--cli`，防止环境或配置中的 TUI 偏好切换入口。只接受完整的受支持
chat 参数，拒绝 profile 切换、worktree、恢复会话、禁用插件与原生长选项缩写。
显式执行文件路径与 PATH 候选共用普通文件和有效执行权限检查。选中的 console
script 以非阻塞方式打开，再通过描述符核验类型并限量读取 64 KiB；即使路径被
替换为 FIFO，也不会阻塞 shebang 解析。执行探测前，AW 要求脚本、解释器及路径
组成项属于调用者或 root，除 sticky 目录外均不可被组或其他用户写入；符号链接
目标也会校验。AW 固定解析后的脚本路径，保留 venv 解释器路径，不修改原生文件权限。
来源发现使用 console script 的 venv 解释器，以 `-I -S` 和标准库发行包元数据
读取本地 editable `hermes-agent` 安装的 `direct_url.json`。venv 和 editable
checkout 路径须为 UTF-8；不支持非 UTF-8 安装路径，发现阶段会在原生导入或发布
之前失败。不导入 Hermes、不处理
`.pth`，也不加载项目或环境配置的导入路径。安装缺失或有歧义时拒绝继续；后续原生
命令仍保留配置的环境。venv 的 `pyvenv.cfg` 也必须受保护，且不能启用 system site
packages。任何 `-P` 探测前，AW 遍历发现的 venv site 目录，包含 `.pth`、finder、
package 和 cache，对文件及链接应用同样的归属/权限检查。site 目录即使设置
sticky bit，也不能允许 group/other 写入，因为仍可能被新增导入文件。遍历可取消，最多 30 秒。
目录发现同时覆盖原生 site 路径与显式 venv scheme，包括 `-S` 会改变 prefix
初始化行为的 Python 版本。所有原生 Python 命令在解释器启动前传入
`-X pycache_prefix=...`，覆盖 site 初始化阶段。预检探测与安装 writer 共用私有
0700 临时缓存，使用前按原生路径规则校验缓存及全部祖先目录的归属/权限，拒绝自定义
非 sticky 可写 `TMPDIR` 父目录；成功、失败或取消后清理。不读取或改写
checkout/profile 的字节码缓存，原生命令仍保留配置的环境。Git 辅助程序复用 console script 的可执行文件解析及
归属/权限检查。分别从启动 cwd 和安装 checkout 解析，匹配原生 Git 调用，保留安全
的相对 PATH 项。AW 使用从 checkout 解析的绝对辅助程序路径。
所有跳过的 PATH 候选也必须受保护：已有文件须通过路径权限检查，缺失名称的最近已存在
父目录须禁止他人写入，包括 sticky 目录，防止插入优先级更高的可执行程序。
AW 在检查时将
`GIT_WORK_TREE` 固定为发现的源码目录，防止 `core.worktree` 将校验转向另一棵干净树；
这些覆盖不改写最终原生环境。checkout 与启动仓库共用 `.git` 定位文件、派生 Git 目录中可选的
`commondir` 及本地配置名称校验，在读取有效配置前执行。自动触发的
`post-index-change` Hook 路径由 Git 解析（含 `core.hooksPath`），并接受相同的
文件及名称保护检查。原生版本/status 探测可能在 AW 回调就绪前刷新真实索引；
受保护的 Hook 保留原生行为；显式 `core.hooksPath=/dev/null` 继续禁用 Hook。
祖先仓库查找在 dotenv 加载前后均遵守 `GIT_CEILING_DIRECTORIES`；当前目录和
上限以内的仓库仍接受相同的检查。
配置来源及 include 的相对路径按选中的仓库解析，与 Git 从子目录启动时的行为一致。
启动 `.git` 名称缺失时
父目录须受保护；已有的启动 gitfile 本身也须受保护。继承或 dotenv 环境中的
`GIT_DIR`、`GIT_COMMON_DIR` 不受支持，重试前须取消这些仓库重定向。
检查仅在私有命令环境中移除 `GIT_CONFIG`：该变量只为 `git config` 选择文件，
status 使用正常配置作用域，因此过滤器和 fsmonitor 检查须与 status 使用相同配置。
最终原生环境保持不变。索引/状态探测传入
`-c core.fsmonitor=false`，覆盖仓库及其 include 配置而不修改文件，
避免索引和状态检查调用该 Hook。执行索引/状态检查及原生导入前，AW 还读取 Git
实际加载的配置来源，对每个文件（含 include）应用同样的归属和权限检查。
显式 include 路径由 Git 展开，相对路径以其配置来源所在目录为基准；缺失目标
（含条件 include 引用）的最近已存在祖先必须受保护，禁止他人在检查后创建配置。
原生版本和超时探测也会执行 Git，因此必须在导入前拒绝可写配置。
原生 Git 同时从启动 cwd 和 source 执行，因此不支持相对的外部作用域选择。每次
checkout 检查前，非空 `GIT_CONFIG_SYSTEM`、`GIT_CONFIG_GLOBAL`、
`XDG_CONFIG_HOME` 及已设置的 `HOME` 均须为绝对路径；重试前改用绝对路径或取消
变量。空系统/全局/XDG 值保留原生禁用或默认语义。
作用域路径即使缺失也会校验：显式系统/全局覆盖、由 Git 展开 HOME/XDG 的全局路径，
以及 `rev-parse --git-path` 报告的仓库和已启用 worktree 路径。仍支持已禁用作用域和
`/dev/null`。未显式指定系统路径时，Git 必须通过 `var GIT_CONFIG_SYSTEM` 报告编译路径；
旧 Git 需要显式 `GIT_CONFIG_SYSTEM` 路径或禁用系统作用域。AW 不猜测自定义 Git 构建
的系统配置目录，也不改写环境。
有效的 `core.fsmonitor` 必须未设置或能被 Git 解析为布尔值。即使配置文件受保护，
也不支持外部命令 Hook；启动前需禁用该 Hook，避免把 shell 命令误当作单个可信
可执行路径。配置中的外部 Git `filter.*.clean`、`.process` 和 `.smudge` 命令也不支持，
在索引/状态检查前拒绝。这些限制在 dotenv 加载前后都生效。
先用父进程环境校验 checkout，再以有界 Python `-P` 探测调用固定版本的
`load_hermes_dotenv`，禁用本次探测的外部密钥获取，只返回 Git 相关变量、`LD_*`
加载器变量以及 `PATH`、`HOME`、`XDG_CONFIG_HOME`。非空 `LD_*` 在原生子进程启动前
检查，并在 dotenv 加载后、第二次 Git 探测前再次拒绝；允许空值或未设置。在 CLI 版本/超时导入前，使用该有效环境重新
执行同一套执行文件、配置来源和 checkout 检查。dotenv 优先级和插值由原生 loader
负责，探测不重写最终 chat/Provider 环境；原生启动仍正常加载 dotenv 和外部密钥。
在该启用 site 的探测前，项目 dotenv 检查拒绝组或其他用户可写的 checkout 根，
包括 sticky 目录，且无论 `.env` 是否存在。这同时保护 editable `.pth` 激活的
固定 checkout 根，阻止他人新增导入名称。

安装和启动均在执行 `--version` 之前，校验发现的 checkout 路径、已跟踪文件、
固定 Git 提交、索引标记和已跟踪状态。限时 `git ls-files -v -z` 拒绝
`assume-unchanged` 和 `skip-worktree`，请清除标记后重试。每个 checkout 探测均使用
`--no-replace-objects`，使工作区/索引始终与固定对象比较，即使存在 replacement refs
也不会转向替换对象。AW checkout 探测使用 `--no-optional-locks` 禁止可选索引刷新，
也不修改 replacement refs。状态探测覆盖 `core.trustctime=true` 和
`core.checkStat=default`，并额外对从 HEAD 填充的临时索引进行比较。新索引没有
缓存 stat 信息，因此即使 Git 构建只比较秒级时间，也能发现恢复 mtime 后的等长改动。
写入临时索引时传入 `-c core.hooksPath=/dev/null`，仅在该检查命令中禁用
`post-index-change` 及其他 checkout Hook，包括原配置的 `core.hooksPath` 覆盖。
原有 Hook 和配置保持不变，原生操作继续使用原值。
临时索引目录显式指定 0700 权限，在成功、失败或取消后清理。
这些覆盖只用于 AW 检查，原生 Git 命令保留原有环境与行为。
随后要求 `git status --porcelain --untracked-files=no` 输出为空。这些检查限时且
可取消，允许未跟踪的环境与构建文件，不承担包完整性审计。

版本探测通过已校验的解释器以 `-P` 运行已检查的脚本，排除脚本目录的隐式导入，
即使该目录带 sticky bit 且可写也不会从中加载同名包。调用者管理的 Python 配置
属于受信任输入，包含显式 `PYTHONPATH` 和受保护的 `.pth`/editable-finder 代码。
AW 保留这些配置，不递归审计它们选择的外部导入根或依赖；安装所有者应信任所配置
的路径。上述检查验证固定框架 checkout 和 AW 自建启动资源，不对已安装的 Python
环境提供沙箱隔离。
后续版本探测最长允许 60 秒，因为该 Hermes 版本会执行自身更新检查；AW 不修改
该设置，也不重试失败。输出的安装目录必须与已校验的元数据来源一致，不使用可能
随 `origin/main` 变化的 banner 作为版本依据。

元数据发现探针复用已有有界数据执行器，JSON 输出限制为 4 MiB，保留 5 秒期限和
取消；版本字符串仍限制为 4 KiB。原生导入前继续检查 source 和 site 路径。

dotenv 探针通过字节数组传递选定环境的名称和值，复用 Python 文件系统编码和
Rust OsString 保留 Linux 环境字节，包括非 UTF-8 的 PATH 项。JSON 使用已有
4 MiB 数据输出限额，保留 5 秒期限、取消和 stderr
限额；版本输出仍限制为 4 KiB。超过 4 KiB 的继承 PATH 与 profile 定义 PATH
均在共享预检及真实 install/chat 入口验证。限额作用于序列化 JSON，包括字节数组
开销；序列化 JSON 超过该限额的环境不属于此有界探针的支持范围。

## 安装与启动

共享启动入口在 Adapter prepare 或服务创建前要求规范工作目录为 UTF-8；安装也在
任何写入前要求规范 profile 为 UTF-8，避免 JSON 路径在发布配置后才报错。

Hermes 从 profile 目录读取 `config.yaml`。该版本没有临时配置覆盖入口，
且只加载已启用的插件，因此 AW 提供显式安装步骤：

```bash
aw install --config aw.yaml --agent hermes --native-profile /absolute/profile
aw run --config aw.yaml --agent hermes --native-profile /absolute/profile -- chat
```

Agent 的 `argv` 指定已安装的 Hermes Python console script，使用绝对 Python
shebang。不支持 shell wrapper 或 `python -m` 入口；请使用固定版本包安装生成的
console script。`chat` 及其参数可以放在
`argv` 中，也可以放在启动命令的 `--` 后，出现一次即可。
[示例配置](../../crates/aw-service/examples/aw.hermes.yaml)已在 `argv` 中写入 `chat`。
安装在探测或修改 profile 前复用启动的 `check_args` 合同校验已配置的原生参数。
只包含执行文件的 `argv` 仍有效，因为启动时可在 `--` 后提供 `chat`。
适配器选择与安装使用同一份已校验的 AW 配置快照；编辑器在分派期间替换配置
文件也不会改变本次安装的配置。

安装向 `plugins/aw-native-hooks` 写入随 AW 提供的插件，将名称添加到
`plugins.enabled`，并移除 `plugins.disabled` 中对应项。列表包含 `${...}` 时，
调用固定版本原生展开函数，在真实 profile 环境中解析插件选择；指向 AW 的项
规范化为启用列表中的名称或从禁用列表移除，其他模板保持原值。原生探针只返回
指向 AW 的索引，模板展开增长不再占用 JSON 输出预算，也不传回无关的展开值。展开探针和 YAML
writer 均通过有界 stdin 接收 JSON 列表，沿用 4 MiB 配置数据限额，避免操作系统
单个 argv 的长度限制。原始安装文件和序列化 JSON 输入各自限制为 4 MiB；
转义或 YAML 展开后的数据仍须满足传输限额，超过限额会在原生写入和 profile 发布
之前失败。未知配置字段与已有
插件选择保持原值。更新配置会将原始字节完整保存到私有的
`config.yaml.aw-backup-<id>`。仅在 AW 独占的私有暂存目录修改需要调整的插件选择
字段。读取插件有效列表前先展开 YAML 合并来源，支持嵌套合并、序列优先级及显式
覆盖；候选验证和已安装配置读取共用该逻辑。固定版本原生 round-trip YAML 编辑器
先复制目标插件 mapping，再替换列表，保留共享锚点的原值。其 YAML 1.1 resolver
为 `on`、`no`、`null` 等名称加引号，使运行时读取器继续将其视为字符串。
写入器保留未知值、注释与引号，并在替换暂存候选之前刷新、同步每次写入的新文件。
写入前初始化所选原生 profile 及其 dotenv，并保留原生受管安装及单字段编辑限制。
仅显式 YAML 路径指向暂存目录；`HERMES_HOME` 和 profile 选择器仍指向真实 profile。
写入器与回调超时探测使用 Python `-P`，避免项目目录隐式覆盖原生模块的导入；
显式设置的解释器环境保持不变。
备份只在候选校验及最后一次真实文件比较通过后创建；暂存失败不留下备份副本，
发布错误会报告确切的备份路径。
AW 核对真实配置仍等于原始字节后，再与候选原子交换。原生命令失败或发布前
检测到并发编辑时，真实配置保持不变。被换出的 inode 保留在
`config.yaml.aw-backup-<id>.displaced`，与 `backup` 一起通过 `displaced` 字段返回。
最终比较与交换之间的编辑会返回明确冲突：候选已成为真实配置，并发版本保留
供恢复。AW 不会自动回退覆盖后续编辑。其他写入者关闭文件描述符之前应保留
displaced 文件，对旧 inode 的迟到写入仍保存在其中。原生写入者不遵守 AW 锁，
因此这里保证保留其数据，不承诺按内容条件替换。文件系统不支持交换时直接失败，
不会退回无条件 rename。暂存目录只有配置候选，不复制认证与历史。配置发布后，
安装显式删除暂存目录并同步 profile，完成后才报告成功。清理或同步失败会报告
已发布状态及两份恢复文件路径，不自动回退。Drop 在错误路径上继续尽力清理。
每次原生写入命令的期限为 30 秒。安装复用启动信号处理与取消标志；中断时先
回收原生探测和写入进程、清理暂存目录，再保留信号退出状态。进入安装、创建插件
资源及每次原子发布之前都检查调用方的取消标志，无需原生写入器的路径也遵循此
约束。取消会移除尚未发布的暂存目录与 displaced 链接；已创建的初始备份会保留
并报告路径。已发布的插件与已交换的配置不会自动回退，已完成的发布会继续同步
目录并核验恢复文件。安装锁文件及空 `plugins` 上级目录可以保留，供后续重试。
文件未变化时重复安装不产生修改。同名非 AW 插件、
符号链接文件、非法插件列表或已占用的安装锁都会返回明确错误。安装锁使用作用域
guard，在关闭描述符前显式解锁，避免并发 fork/exec 延长已完成安装的持锁时间。
profile 配置与插件
必须是当前用户所有的普通文件，且组或其他用户不可写；接受 0600 和 0644。
在原生 dotenv 或 CLI 导入前，同一个不跟随符号链接的读取器检查可选的 profile
和 checkout `.env`。其父目录即使带 sticky bit，也必须禁止组或其他用户写入，
以保护尚不存在的 dotenv 名称。悬空链接及非普通文件会被拒绝，不视为文件缺失。
任何探测或安装写入前，规范化后的 profile 路径必须是 UTF-8。
AW 不会修改不安全文件的权限。规范化后的 profile 各级上级目录必须属于当前用户
或 root；组或其他用户不可写，带 sticky bit 保护子目录的情况除外。这会拒绝其他
用户能在原生导入前替换的路径，同时允许 `/tmp` 下的私有 profile。
插件目录发布使用 `RENAME_NOREPLACE`；并发原生安装器创建的空目录也会保留，
并报告路径冲突。备份创建之前检测到的配置冲突只报告真实文件冲突，不指向
不存在的恢复文件。
新建 `plugins` 目录后还会同步 profile 目录；已有配置已启用 AW、无需修改配置的
路径也执行该同步。启动与安装共用随附文件校验，不匹配、不可读或缺少文件时
报告相同的显式备份与重装步骤。诊断包含具体文件及底层读取错误，不自动覆盖
这类插件目录。

发布后，AW 先输出安装状态及两个恢复路径，再清理私有 Python 缓存。缓存清理
失败仍返回错误，但不会隐藏已经安装的配置及恢复文件。

安装不会调用 `hermes plugins enable`，因为该命令可能向已有 Gateway 或
Desktop 进程热加载插件。AW 只更新本地文件，不复制或重写 profile 的 `.env`、
`auth.json`、数据库、记忆与会话。Hermes 正常运行时仍可更新自身状态。

启动要求匹配的 AW 插件已经安装并启用。AW 保留选定的 `HERMES_HOME`，显式
指定对应的原生 `--profile`，防止 sticky `active_profile` 改变目标目录，并
保留启动工作目录。普通 Hermes 启动没有 AW 启动器的进程内会话绑定时，
已安装插件不会注册任何回调。

当 `providers` 和 `events` 为空时，AW 注册零个回调并正常启动。已有原生 Hook
仍会执行；AW 记录会话生命周期，不表示执行了每工具策略检查或工具审计。
仍须先显式安装插件。

## Hook 语义

每个准入的 AW step 对应独立的原生 shell Hook 回调，通过
`PluginContext.register_hook` 注册。Hermes 按登记顺序串行执行回调，工具批次
的串行或并行仍由 Hermes 调度。已有配置中的 Hook 继续执行；AW 不导入或
重排其配置。该固定版本的原生 Hook 配置没有逐 Hook 的环境变量对象。

原生命令收到回调进程的实际环境，包括 profile 的 `.env` 变量，以及 Hermes
设置的 HOME/TMPDIR。结构化 Provider 保持启动绑定时固定的环境。AW 不将环境
内容写入归一化事件或审计记录。

| 原生事件 | AW 事件 | 可用行为 |
| --- | --- | --- |
| `pre_tool_call` | `tool.before` | 结构化 observe/block；原生命令保留 stdout 与退出状态语义 |
| `post_tool_call` | `tool.after` | 观察，包括被阻断、失败、取消及超时的工具结果 |

原生载荷提供 `session_id`、`extra.api_request_id` 和 `extra.tool_call_id`。
归一化 call ID 对请求与工具调用 ID 一起取哈希，允许模型在后续请求中复用同一
call ID。AW 拒绝缺少身份的回调，
保留任意工具名和对象参数，保持 `extra.result` 原始类型，包括已序列化的
JSON 字符串。缺少原生 call ID 时不会生成替代值。

Hermes 将原生 `action: block` 或退出码 2 解释为阻断。阻断不会使后续登记的
before 回调停止执行。原生 `action: approve` 表示请求人工确认，不表示自动
放行；结构化 AW `ask` 尚未支持。配置 AW before 回调时，启动器先执行原生
指令归并，再阻断所有生效的参数改写并丢弃修改后的参数。原生回调均检查原始
参数，随后才归并改写；直接执行改写结果会绕过 AW 检查。所有回调仍会执行，
已有原生阻断理由保持不变。该 guard 不随插件重新发现而失效，在原生 main
退出时恢复。同一启动作用域也包装原生 tool-execution middleware：在执行链之前
冻结 JSON 参数，参数变化时返回原生工具错误，阻止真实 dispatch。该边界覆盖
`execute_code` 内部的 `CellAuthority`/RPC 调用，它们的前置检查发生在执行
middleware 之前。值相同的新字典仍可使用；嵌套原地修改及值类型变化会被拒绝。配置 AW before
回调时，仅原生 JSON 值类型和字符串字典键可进入 dispatch；tuple 和整数键不能
通过序列化内容相同绕过检查。
整数必须处于 i64/u64 范围，浮点数必须有限，字符串值及键必须由 Unicode scalar
组成；不支持的输入直接拒绝，不做转换。
配置 AW before 回调时，顶层和嵌套调用均不支持执行 middleware 改写；共用一个
原生执行边界，不重复运行策略回调。原生 after 回调仍观察工具错误。两个 guard
均不随插件重载丢失，在 main 退出时恢复。空策略和仅 after 路由保留原生改写
行为。Post 回调不能撤销工具或替换结果。

AW 事件期限与 Hermes 回调期限共同生效。原生命令期限及清理余量超过
`plugins.hook_callback_timeout` 时，启动会拒绝该配置。有界探测使用 console script
解释器与固定版本原生 CLI 的 profile/dotenv 加载器，查询原生 timeout resolver，
因此 YAML 1.1 标量、环境展开、默认值和 600 秒上限均遵循 Hermes。启动探测共用
启动器的取消标志；SIGINT/SIGTERM 会及时回收探测进程组，并保留信号退出状态。
安装与启动准备共用中断错误报告，在恢复信号前输出包括进程组清理失败在内的诊断。
插件完成注册后写入
私有就绪凭据，启动器据此显示原生 Hook 已就绪。临时启动 guard 使用 console
script 的 Python 解释器，解释器启动时就将字节码导入缓存指向私有的启动目录，再在同一进程
导入原生 CLI 前恢复原始 argv。旧 profile 字节码缓存不会被读取，也不会被改写；
私有缓存随启动目录清理。profile 选择和 dotenv 加载仍由原生实现负责。guard 在
导入原生 CLI 之前保存启动器提供的 `AW_HERMES_LAUNCH`，在插件发现前将路径存入
启动器持有的进程内模块。注册与凭据检查均使用保存的路径；模块不受原生插件
卸载影响，在原生 main 返回时移除，后续 dotenv 重载不能选择其他会话的绑定与回调。
普通 Hermes 进程仅有环境变量也不会激活插件。在共用的 chat 启动入口，guard
拒绝生效的 safe mode，保留原生启动，等待原生插件发现并核对匹配凭据，然后
才允许 chat 调度。guard 还会核验 profile 安装路径下的插件确实加载、启用且无错误，
其原生 ownership ledger 的活跃 Hook 事件数量必须符合预期。被放弃的插件加载
worker 所写凭据不能放行 chat；空策略启动也要求插件保持启用。工作目录与 PID 保持不变；外部就绪期限仅约束启动时间。
这仍是同一用户下的安装证据，不构成 final 或 protected 安全边界。

原生强制重新发现会卸载旧 Hook 注册并再次调用插件。每次调用都向新的原生
context 注册完整回调集。首次注册原子创建就绪凭据；后续调用只有在已有凭据
全部内容与当前会话匹配（包括 token、PID、Hook 数量）时才复用。共用的有界、
不跟随符号链接读取器要求文件属于当前用户、是普通文件且权限为 0600。
凭据不匹配或不安全时注册失败，不覆盖原文件。复用凭据不会跳过回调注册，
也不会跳过原生注册表核验。启动器在原生 before 归并、工具执行和公共 terminal
post 入口复用同一检查，
先等待原生发现完成，再读取 ownership ledger。重新发现遗漏 AW 或加载失败时，
旧凭据不能放行下一次工具 dispatch：会话在执行前退出。仅 after 路由也要求审计
回调有效；空路由保留原生行为。执行包装在下游 handler 前再次检查，不重放策略
回调。配置 AW 回调时，包装使用原生 discovery lock 维护 before 检查、执行 middleware 和 post dispatch 的
活跃调用数；存在活跃工具时，在卸载前拒绝强制重新发现，保留待执行的 post
回调，覆盖并行线程、handler 和 middleware 内部的重载请求。若 AW 在 veto 返回后、
post 事件之前丢失，terminal emitter 会在原生无监听器判断前退出，避免审计静默
跳过；registry 和 inline 工具共用该入口。锁只保护计数
更新和发现，不跨越回调或工具执行持有。原生 dispatcher 保留错误与
结果审计，不重放回调或添加调度器。工具之间的重载和空路由保留原生行为；
main 退出时恢复 discovery 方法。

## Review 问题归纳与 KISS 取舍

[#4533](https://github.com/agentic-os-org/ANOLISA/pull/4533) 的历次意见主要反复指向
少数共享合同遗漏，实现将这些规则集中在已有边界：

| 反复出现的问题 | 最小共享修正 |
| --- | --- |
| safe mode、超时后的迟到凭据、强制重载使回调失效 | chat 和工具执行前校验原生插件和 ledger；重载完整注册回调，仅复用同一会话的匹配凭据 |
| dotenv 或插件重载改变 AW 启动绑定 | 绑定保存在启动器会话模块中，不随原生插件卸载而丢失 |
| 原生参数改写绕过原始参数检查 | 原生指令归并只执行一次；配置 AW before 回调时阻断生效改写，guard 不随插件卸载而丢失 |
| YAML 标量、merge/alias、模板和 managed key 与 Rust 假设不同 | 复用固定 Hermes 的 YAML 写入、展开与超时解析，只修改插件选择并保留其他原始值 |
| 配置/插件并发发布、中断、未报告备份和持久化 | 私有暂存、已有锁与原子交换、保留 displaced inode；已有有界子命令传递取消 |
| 检查的代码或辅助程序与实际 Python/Git 执行不同 | dotenv 前后共用预检，复用路径检查、私有字节码缓存及原生 Git 查询，覆盖跳过名称和仓库定位文件 |
| PATH、argv/help、非 UTF-8 路径在副作用后才失败 | 在原生探测、写入或服务创建前校验支持入口与序列化输入 |

反复发现问题的主因是检查了一个快照，后续原生操作却使用另一套环境、名称解析或
生命周期状态。因此回归覆盖 dotenv 的有效环境变化、成功启动后的重载、安装/启动
路径上的原文件保留和资源清理；就绪文件本身不能证明策略仍在执行。
后续 [#7101](https://github.com/agentic-os-org/ANOLISA/pull/7101) 也在这些边界
核对原生生效参数及 YAML 1.1 的字符串名称。

KISS 是在共享边界一次修正不变量、复用原生语义，并对不支持的仓库选择器或 shell
辅助命令明确报错。已复现的正确性检查不能省略。这里不增加通用 Git 配置解析器、
Python 外部导入闭包扫描、回调调度器、自动权限修复或重试层。调用者管理的外部
Python 导入保持为受信任配置。动态禁用模板和 sticky checkout 根已有原生启动及
dotenv 检查兜底，未重复添加展开探测或生产 guard；命名 profile 拼接意见也先核对
固定版本实现再判断是否修改。

## 验证

Rust 测试覆盖 profile 安装、原字节备份、幂等、未知字段保留、同名与符号链接
拒绝、事件身份及入口限制。可选原生测试在固定的官方 Hermes Python 环境中
使用临时 profile。

原生测试还覆盖成功注册后 `/voice status` 使用的 STT 可用性探测以及显式强制
重新发现。两次重载均须保留阻断、after 事件审计回调和已有配置 Hook，且不得
重复注册，即使原生 dotenv 加载已覆盖启动变量或该变量已被删除；同时覆盖空策略
重复注册及无效凭据。
参数改写回归覆盖修改 Hook 登记在 AW 前后、重载前后、已有阻断理由，
以及空策略和仅 after 路由的原生改写行为。
真实原生 `CellAuthority.dispatch` 路径另覆盖执行 middleware 的参数替换、嵌套
原地修改、布尔值改为数字及仅观察，包含插件重载前后、after 错误事件和空策略/
仅 after 路由。失败重载回归通过真实发现，在成功启动后遗漏 AW 或使加载超时；
before+after 和仅 after 路由均不能进入最终 handler，空路由保持原生行为。
测试只将最终 registry handler 替换为执行记录器。

```bash
python src/aw/crates/aw-service/tests/fixtures/hermes/native_contract.py \
  --source /absolute/hermes-agent \
  --plugin /absolute/anolisa/src/aw/adapters/hermes
```

`live_cli.py` 使用确定性的本地 OpenAI 兼容服务、真实 AW daemon 与示例
Provider，运行官方 `chat --oneshot`。测试使用固定的 Hermes Python 环境，并以
`--source` 指定该源码目录，检查允许、阻断和空策略是否被采用、原生 dotenv 的
safe mode 优先级、已有相对路径 Hook 共存、
before/after 命令、profile 与原生 YAML 值保留、回调环境、实例释放与清理。测试不使用真实
模型密钥，也不据此声称托管模型或交互式人工确认已经通过验收。
测试插件在第一次工具的 post 事件完成后、下一次 API 请求前触发原生 STT 重新发现，第二次工具尝试
仍须被阻断。真实 AW journal 必须包含两次尝试各自匹配的 before/after 事件
和成功执行的策略调用。
安装回归同时保留 `on`、`off`、`yes`、`no` 等名称的 YAML 1.1 字符串类型和
重复安装幂等性。两个含模板的插件列表分别超过 Linux 单个 argv 的 128 KiB 限额，
仍通过原生展开及写入。非 root 文件系统故障在安装完成后拒绝私有缓存清理；
命令须失败，但仍报告安装状态和可读取、字节级一致的 backup/displaced 路径。
测试恢复权限并清理自建缓存。真实 chat 的改写回归要求原始参数获准时仍阻断共存 Hook 改写，
无工具副作用且 before/after 审计成对保留。

## 回退

先停止会话与配置写入者，再恢复 `aw install` 返回的文件。检查并合并
`displaced` 中的并发或迟到修改；`backup` 保存最初的原始字节。将选定文件
恢复到同一 profile 的 `config.yaml`，删除 AW 创建的 `plugins/aw-native-hooks`
和 `.aw-install.lock`。两个恢复文件按用户的保留策略处理。普通 Hermes 启动也可以
保留插件，因为没有 AW 启动绑定时插件不会生效。

启动 JSON 对非 UTF-8 的 AW 执行文件、binding 和状态路径返回可传播错误。
安装结果使用可失败的输出写入；写入失败时通过 CLI 错误通道报告已发布状态、
backup 和 displaced 路径。

Git 从工作目录与 checkout 解析时须选择同一规范可执行文件；相对 PATH 项
选择不同 Git 构建的情况不受支持。

Checkout 与启动工作目录复用同一个有效 Git 配置检查。各自保留 cwd，使 local
来源与 includeIf 按实际目录生效；仅 checkout 检查固定 worktree。启动目录检查
不运行 status 或更新项目 index。普通项目仓库通过现有 filter/fsmonitor/路径
检查时仍受支持。
