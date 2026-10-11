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
替换为 FIFO，也不会阻塞 shebang 解析。
版本探测最长允许 60 秒，因为该 Hermes 版本会在 `--version` 中执行自身更新
检查。AW 不修改这项原生设置，也不重试失败的探测。AW 根据输出的安装目录
核对完整 Git 提交，不使用可能随 `origin/main` 变化的 banner 作为版本依据。
安装和启动随后要求 `git status --porcelain --untracked-files=no` 输出为空，拒绝
已跟踪文件的已暂存及未暂存修改。该探测有超时并支持取消，允许未跟踪的环境与
构建文件；它不承担包完整性审计。

## 安装与启动

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
`plugins.enabled`，并移除 `plugins.disabled` 中对应项。未知配置字段与已有
插件选择保持原值。更新配置会将原始字节完整保存到私有的
`config.yaml.aw-backup-<id>`。官方 `hermes config set` 只在 AW 独占的私有暂存
目录中修改需要调整的插件选择字段，其 YAML 1.1 写入器保留未知值、注释与引号，
并在替换暂存候选之前刷新、同步每次写入的新文件。
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
符号链接文件、非法插件列表或已占用的安装锁都会返回明确错误。profile 配置与插件
必须是当前用户所有的普通文件，且组或其他用户不可写；接受 0600 和 0644。
在任何原生探测前，同一个不跟随符号链接的读取器还会检查可选的 profile `.env`，
因为导入版本 CLI 就会加载它。悬空链接及非普通文件会被拒绝，不视为文件缺失。
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
放行；结构化 AW `ask` 尚未支持。原生 `modify` 继续交由 Hermes 自身解析，
AW 不据此定义跨框架的参数改写合同。Post 回调不能撤销工具或替换结果。

AW 事件期限与 Hermes 回调期限共同生效。原生命令期限及清理余量超过
`plugins.hook_callback_timeout` 时，启动会拒绝该配置。有界探测使用 console script
解释器与固定版本原生 CLI 的 profile/dotenv 加载器，查询原生 timeout resolver，
因此 YAML 1.1 标量、环境展开、默认值和 600 秒上限均遵循 Hermes。启动探测共用
启动器的取消标志；SIGINT/SIGTERM 会及时回收探测进程组，并保留信号退出状态。
安装与启动准备共用中断错误报告，在恢复信号前输出包括进程组清理失败在内的诊断。
插件完成注册后写入
私有就绪凭据，启动器据此显示原生 Hook 已就绪。临时启动 guard 使用 console
script 的 Python 解释器，先将字节码导入缓存指向私有的启动目录，再在同一进程
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
也不会跳过启动 guard 对真实原生注册表的核验。

## 验证

Rust 测试覆盖 profile 安装、原字节备份、幂等、未知字段保留、同名与符号链接
拒绝、事件身份及入口限制。可选原生测试在固定的官方 Hermes Python 环境中
使用临时 profile。

原生测试还覆盖成功注册后 `/voice status` 使用的 STT 可用性探测以及显式强制
重新发现。两次重载均须保留阻断、after 事件审计回调和已有配置 Hook，且不得
重复注册，即使原生 dotenv 加载已覆盖启动变量或该变量已被删除；同时覆盖空策略
重复注册及无效凭据。

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
测试插件在第一次工具的 post 事件后触发原生 STT 重新发现，第二次工具尝试
仍须被阻断。真实 AW journal 必须包含两次尝试各自匹配的 before/after 事件
和成功执行的策略调用。

## 回退

先停止会话与配置写入者，再恢复 `aw install` 返回的文件。检查并合并
`displaced` 中的并发或迟到修改；`backup` 保存最初的原始字节。将选定文件
恢复到同一 profile 的 `config.yaml`，删除 AW 创建的 `plugins/aw-native-hooks`
和 `.aw-install.lock`。两个恢复文件按用户的保留策略处理。普通 Hermes 启动也可以
保留插件，因为没有 AW 启动绑定时插件不会生效。
