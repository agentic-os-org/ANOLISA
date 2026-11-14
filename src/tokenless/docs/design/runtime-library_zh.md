# Tokenless Runtime 库

[English](runtime-library.md)

## 目标

`anolisa-tokenless` 是框架无关的 Tokenless SDK。BeforeModel、PostTool 和 Retrieve 通过
PyO3 Runtime 在进程内运行；PreTool 在子进程中运行，详见下文。平台 Wheel 同时包含 PyO3 Runtime
和匹配的 `tokenless` 可执行文件，因此 Python 应用不要求 `tokenless` 或 `rtk` 出现在 `PATH` 中。

公开的 `TokenlessSdk` 把宿主框架的四个生命周期映射为 Tokenless 行为：

| 生命周期 | 行为 |
|---|---|
| `before_model` | 可恢复的 Function Calling Schema 压缩，并按需发布恢复工具 |
| `pre_tool` | 对 Adapter 明确指定的命令字段执行 RTK 改写 |
| `post_tool` | 状态路由、响应压缩、TOON 候选选择和环境错误提示 |
| `retrieve` | 受可见 marker 授权的 byte-exact Stash 恢复 |

Tool Ready 在产品范围内硬关闭，不属于该 API。

## 协议与状态

Python Hook 的宿主协议规则放在各自的
`adapters/tokenless/<host>/hook_adapter.py`：输入解析、能力检查和返回信封由该宿主负责。
例如 Cosh-NG 将 Shell 替换结果序列化为文本，Claude Code 保留结构化字段，千问办公将单流
Shell 的 `stdout` 替换为文本并保留非 Shell 的结构化 MCP 字段，
Qoder 保留原生 Bash 的 stdout 对象。`common/hooks` 负责 Protocol v2 通信、归一化结果的
通用转换和生命周期状态。原有命令文件保留为启动入口，宿主选择位于
`adapters/tokenless/hook_dispatch.py`。Cosh 扩展随包携带自己的适配器，独立复制的千问办公
插件加载自身目录中的适配器和公共运行库。OpenClaw、DSH、Hermes 及 Python SDK 集成
继续使用各自现有的宿主入口。压缩器选择和压缩结果的接受策略仍由 Core 负责。

Adapter 把框架对象转换为四组不可变 Request/Response：`BeforeModelRequest`、
`PreToolRequest`、`PostToolRequest` 和 `RetrieveRequest`。`Attribution` 要求 Agent 和
Session 标识；PreTool 与普通 PostTool 还必须提供 Tool Use 标识。工具 Schema 统一为
OpenAI Function Calling JSON，但生命周期操作是 Tokenless 自身契约，不是 OpenAI 请求。

CLI Envelope 保持 Protocol v2。BuildLog 布局压紧与进度缩减共用现有
`AppliedOperation::BuildLogReduction` / `build_log_reduction`，无需 Wire 升版或新增 SDK 枚举。
PyO3 调用不经过 Envelope，沿用 `AppliedOperation.BUILD_LOG_REDUCTION`。
恢复等级仍由独立结果字段表示：仅布局为语义 `lossless`，包含进度省略时为 `retrievable`。

`tokenless-runtime` 统一持有 SQLite Stash 和统计记录器。Schema 与响应压缩共享 Stash，
候选被丢弃时会回滚对应 key。TOON 作为 Rust 库直接链接，不启动进程。RTK 链接在 `tokenless`
可执行文件中；Adapter 提供 `command_field` 时，`TokenlessSdk.pre_tool` 每次调用都启动
`tokenless compress`，因为 RTK 假定一条命令对应一个进程：它在进程生命周期内缓存已信任的过滤规则和
项目文件，并可能无时限地等待 `git rev-parse`。因此每次调用都能看到最新的 RTK 状态。SDK 给子进程
5 秒时限，超时后杀掉其进程组（Windows 上用 `taskkill /T`），任何失败都记录警告并放行原命令。每个
改写后的 wrapper 都以 `tokenless rtk` 形式调用 Wheel 内置的 `tokenless`，并携带本次执行的归属信息。内容检测、阈值、TOON 选择、诊断、授权和 Stash 策略都保留在
Rust Core，而不是 Python 配置中。Shell 工具的 `PostToolRequest` 在 `command` 字段携带实际
执行的命令行；由 Core 而非 Adapter 把只打印本地文件的结果报告为 `file_read`，打印出的
HTML 页面因此保持原样，输出中的数据仍照常压缩。

PreTool 使用与 `find` 相同的直接调用及内置 Wrapper 识别规则，保留已识别的原生
`head`、`tail` 调用，支持嵌套和 Shell 换行续行。所有参数形式均保持原生执行，包括字节
窗口和持续跟踪模式。一旦识别到此类调用，其所在的整条 Pipeline 或复合命令都跳过 RTK
改写。这避免了 RTK `read --max-lines` 路径的结构截断；显式 RTK 调用保持既有行为。
不会读取自定义 `hooks.transparent_prefixes`，因此这些前缀下的调用仍可能被改写。
该规则独立于搜索路径共享开关，保留现有 PostTool 文件读取策略。

同一原生命令识别覆盖 `grep`、`rg` 的所有模式；`git grep`、`xargs grep` 和自定义 Wrapper
前缀不在此识别范围。PostTool 复用该解析器识别 `command_output` 来源的搜索，
与原生 `Grep` 工具一样，只调用 `SearchResultsCompressor`。统一入口识别带路径和行号的
匹配记录及纯路径列表，格式识别和收益判断都保留在压缩器内部。关闭搜索压缩或没有候选时
原样返回，即使设置 `force_json` 也不尝试其他领域压缩器。Qoder 原生 Bash Adapter 只替换
字符串 `stdout`，即使它较短、为空或 `stderr` 更长。`stdout` 缺失或不是字符串时禁用替换；
错误诊断仍可读取 `stderr`。回填结果时保留其他所有字段。

此保护覆盖包含已识别搜索的复合命令的完整输出。例如，`grep -q needle Cargo.toml && cargo build`
也会保留完整构建日志，因为 Hook 不提供各子命令的输出边界。在能保证搜索匹配完整的混合输出
分块处理实现之前，这会有意降低既有构建/测试日志及其他领域的压缩覆盖率。
`rg -l TODO src | xargs cat` 等 Pipeline 也保留输出，尽管最终内容是文件正文而非路径列表。
Tokenless 不增加截断回退，宿主自身的输出限制仍然有效。`max_input_bytes` 限制压缩工作量，
超限时原样返回，不限制返回文本体积。

搜索压缩器开启但没有候选时，使用公共仲裁的 `no_savings`；连接持久化 Store 时，Stash
计数为零。关闭搜索压缩或命中生命周期门槛时仍为透传。API 和文件读取结果上的 `command`
元数据不会开启 Shell 搜索保护；精确工具名 `Grep` 保留其原生搜索契约。

SDK 不保存进程级“当前 Session”。`before_model` 返回精确的可见 Marker 集合。Adapter
声明自己是否已有 Marker 授权恢复路径，并持有面向 Agent 的命令或 Tool 声明。AgentScope
在模型调用之间保持 Retrieve Tool 静态，把 Marker 集合保存在框架 Session 状态中；`retrieve` 只授权该集合
中的 Marker。宿主应用继续保存原始工具结果供 UI 和业务逻辑使用，只把复制
后的最终模型可见文本传给 `post_tool`；Retrieve 输出禁止再次进入 PostTool。

千问办公的 Hook Adapter 在两个生命周期请求之前，将识别到的工作区 Shell MCP 名称统一为
`Bash`。仅对恰好一个普通文本块解包；未知块数组直接透传。工作区事件的绝对 `cwd`
确定 `.tokenless` 状态位置，两个 Hook 和 RTK 命令共用它。原生 Bash 保留原有状态位置，仅将单流 `stdout` 替换进宿主文本槽，对多流、失败或未知
Shell 包装保持透传，并按最终交付文本核验净节省。插件私有二进制也由 Adapter 解析；macOS 上经验证的 PreTool 桥接将
独立恢复命令绑定到该二进制及相同的状态、Stash 选择和归属。桥接不可用时，私有运行时
路径声明 `recovery.kind = none`。桥接和精确恢复识别由千问办公适配器独立负责。

非法输入、内置 `tokenless` 可执行文件缺失、生命周期操作失败、挂载失败和工具重名会快速失败。Passthrough、
No Savings 和 Recoverability Unavailable 等正常 Core Disposition 返回 typed 结果。候选
只有严格更短时才会采用；Schema 和响应截断还必须能够恢复。

OpenClaw 2026.6.6 或更高版本通过已声明且显式启用的工具结果 middleware，在模型消费前
调用 PostTool。Adapter 保留结果信封，只处理单文本结果，并保留逐调用 RTK 归属信息。
静态 `tokenless_retrieve` 工具的工厂收到会话 UUID 后才声明工具恢复能力。Adapter 从
提交 prompt 时的 `llm_input` 重建可见 Marker 集合；历史没有恢复指令时跳过 Core，并加入
刚交付的 Stash Key。`after_compaction` 不提供消息内容，因此撤销此前授权，直到下次提交
prompt 时取得保留的历史。会话结束时清空状态。工具执行时向 Core Retrieve 提供该会话的
Marker 集合，取回内容绕过 PostTool。
缺少工具 API 或 UUID 时仅接受无损压缩；RTK 输出在回执查询前仍声明 `recovery.kind = none`。
不再通过单独的持久化 hook 重复处理结果。Adapter 捕获压缩
失败并保留收到的宿主结果，因为宿主会把 middleware 抛出的异常转为工具失败。
旧版 middleware 预处理可能已裁剪文本或元数据。Tokenless 不会在 API 最低版本之上
额外拒绝这些宿主，也不会重建缺失内容。仅 `applied` 压缩替换文本；工具错误诊断仍可
追加上下文。详见[宿主兼容边界](../../../../docs/user-guide/zh/tokenless/framework-integration.md#openclaw)。

构建 CLI 和 OpenClaw Adapter 后，可在 `src/tokenless` 使用本地宿主镜像运行可选容器回归：

```bash
python3 tests/test-openclaw-gateway.py --image <legacy-image> --host-output-policy legacy
python3 tests/test-openclaw-gateway.py --image <fixed-image> --host-output-policy normalized
```

根据镜像的源码来源选择预期策略，不依赖版本字符串。真实 Gateway 使用本地脚本模型，
不发送云端请求。测试覆盖普通 CSV、150,000 字符纯文本／JSON 结果、失败透传、首次
消费、历史重放、持久化和统计。旧版截断作为预期限制验证；normalized 策略要求完整
原生输入进入 middleware，并无损交付 JSON 候选。此测试验证传输行为，不验证模型
回答质量或计费节省。

Python 生命周期配置在构造时读取总开关、搜索、HTML 和 Diff 环境开关。显式
`TokenlessConfig` 参数优先于环境默认值，解析后的开关传入进程内 Runtime，因此也覆盖
QwenPaw 和 AgentScope；直接 Runtime 构造仍采用显式配置。Diff 默认开启，关闭压缩
总开关只选择 dry-run，不会关闭 PreTool RTK。

## BuildLog 布局与进度

BuildLog 在既有 PostTool 内容路由内处理 pnpm，不解析命令，也不新增 PreTool 归属规则。
`pnpm install` 不加入原生命令豁免列表；RTK-owned 结果沿用既有旁路/回执规则。
SGR 清理后，pnpm 在原始行坐标上规划互不重叠的布局替换和进度角色，再一次渲染。
已识别的更新通知、依赖清单和与数量精确匹配的装饰条可压紧并报告为 `build_log_reduction`，
恢复等级为语义 `lossless`，即使恢复为 `none` 也不写 Stash。边框和填充无需逐字节重建，
但通知正文、全部依赖事实和完成信息必须完整保留。

只处理完整 LF 记录。非 SGR 控制字符、CRLF、裸 CR、TAB 会停用 pnpm 新转换。
警告、脚本头和未知正文保护 SGR 清理后的整个剩余后缀，不因后续空行/进度恢复解析。
进度至少连续九条才考虑省略，首尾各保留两条；任一计数下降即分段。
`done`、包总数、通知/类别边界及最终耗时/版本保持可见，布局变换不能合并原先分离的进度段。

进度省略可与布局压紧同时生效，共用 `build_log_reduction` 标签，恢复等级为 `retrievable`，
要求 Stash 和宿主实际恢复路径。每个省略区间计入 Marker 后须独立减少字符与估算 Token，
不能用布局收益补贴省略。恢复不可用、省略无收益、超预算或写入失败时，压缩器返回布局候选及
全部 tentative writes 和 Stash 错误。入口仍将 Stash 错误提升为 Runtime 错误，宿主因此可能
保留原文而不是采用布局。唯一 Pipeline 仲裁与 `StashLedger` 保留 dry-run、时限、回滚和 generation
归属保护。两种转换内部共用 `BuildLogOperation::Reduction`，映射到 `AppliedOperation::BuildLogReduction`。
操作按 `terminal_cleanup`、`build_log_reduction` 排序，每项最多报告一次，只记录最终结果实际采用的操作。
详见 [pnpm 输出边界](../../../../docs/user-guide/zh/tokenless/index.md#pnpm-安装输出)。

## 搜索路径共享

PostTool 在搜索路径共享已开启且支持文本替换时，将识别为 `search_results` 的内容交给
`SearchResultsCompressor`。原生 `Grep` 和识别出的 Shell `grep`、`rg` 直接调用此压缩器，
不要求内容检测先判为搜索结果。其他工具保留按内容派发：带行号记录要求 `api_response`
来源，纯路径列表也接受命令输出。
成功的 `path:line:text` 列表至少包含三条记录，连续记录可以共享完整文件路径。
行号、正文、顺序和换行保持字节可逆。输入契约不包含路径内的冒号和上下文列表；
任意不支持的记录都会拒绝整份候选。字节数与估算 Token 均减少后，再经过现有 Runtime 仲裁。
实际采用时操作为 `search_path_sharing`，Python SDK 对应 `AppliedOperation.SEARCH_PATH_SHARING`；
恢复等级为 `lossless`，不写入 Stash。

纯路径列表也可共享公共目录前缀，允许 `command_output` 和 `api_response` 来源。
至少包含三条 POSIX 路径，使用统一的 LF 或 CRLF；收到的全部路径、顺序、重复项及末尾换行均保留。
压缩器验证完整输入，包括超出检测器采样窗口的部分。空行、控制字符、含冒号的路径
（包括 URL 和 Windows 路径）、混合行尾、双前导斜杠或有歧义的相对根目录均拒绝压缩。
不带前缀的相对根目录仅接受字母、数字、`.`、`_`、`-`；以 `/`、`./`、`../` 开头的路径
可包含空格、引号和反斜杠，通过 JSON 字符串编码。压缩器不读取文件，也不规范化路径。
`file_content` 和 `file_read` 均跳过此路径列表策略；既有文件读取的 JSON、表格、日志处理不变。
对于搜索专用通路之外的工具，既有源码和表格检测优先于纯路径列表检测。路径探测的有界采样停在最后一个完整行尾，
不会把路径中间或 CRLF 中间的字节截断片段视为记录。搜索专用通路之外，资格检查先解析完整路径列表，
压缩器再解析并生成候选，随后进入公共仲裁。

路径列表视图以 `[Path list; all received paths retained; full path = base + each paths entry]`
开头，随后是含 `base`、`paths`、`line_ending`、`trailing_newline` 的紧凑 JSON。
将 `base` 与每个解码后的 `paths` 项拼接，以 `line_ending` 连接，仅当 `trailing_newline`
为 true 时补充末尾行尾，即可逐字节还原。公共前缀始终以 `/` 结束。完整视图（包括声明和元数据）
的字节数和估算 Token 都减少后，才进入收益仲裁。

压缩器还会为同一批列表渲染按目录分组的候选视图。当公共前缀较短、而相邻条目各自重复较长
的目录链时（例如排序后的相对路径树），分组能进一步压缩：

```text
[Paths grouped by directory; full path = base + Dir + entry; entries LF-separated; restore with CRLF; no final newline]
Base="src/tokenless/crates/"
Dir="tokenless-ccr/"
Cargo.toml
Dir="tokenless-ccr/src/backends/"
in_memory.rs
mod.rs
sqlite.rs
Dir="tokenless-ccr/src/"
key.rs
lib.rs
```

分组视图只在相邻条目之间共享目录：`Base=` 行存放以 `/` 结尾的公共前缀（JSON 引号编码），
每个 `Dir=` 行存放相对 `base` 的目录（JSON 引号编码），其余行是该目录下的条目，按接收顺序排列。
条目之间始终以 LF 分隔；声明中的 `restore with` 从句记录原始输入使用的行尾（CRLF 或 LF），
`final newline` 从句记录输入是否以行尾结束（`CRLF; final newline`、
`CRLF; no final newline` 或 `restore with LF; no final newline`；两个从句均未标注即表示
LF 且带末尾换行）。条目若作为裸行会产生歧义——为空，或以 `Base=`、`Dir=`、`"` 开头——
则与声明一样使用 JSON 引号编码。还原时，将 `base` + 当前 `Dir` + 各条目依次拼接，
以记录的行尾连接，仅当声明记录了末尾换行时补充行尾。
仲裁会在两个候选中选择较小者，且候选只有逐字节还原输入、并同时减少字节数与估算 Token
时才会输出；没有目录局部性的列表（例如按修改时间乱序排列）保持公共前缀视图或原样通过。
资格判定、路由、`search_path_sharing` 开关、统计上报与可恢复性均不变；两种格式在统计中
仍上报为 `search_path_sharing`。

PreTool 保留已识别的原生 `find` 调用，以便 PostTool 接收原生结果。包含此类调用的整条复合命令也保留原样；
混合输出不属于路径列表，不进入该压缩策略。后置路由本身不要求命令为 `find`，其他未优化工具结果
只要内容符合格式也可复用。检测会穿透嵌套的透明 Shell 关键字（`noglob`、`command`、
`builtin`、`exec`、`nocorrect`）、`env`、进程 Wrapper（`timeout`、`time`、`nice`、`nohup`）
和 `uv run`。进程 Wrapper 与固定的 RTK v0.49.0 参数语法一致：带值选项、Flag、附加值、
数字调整量、`--` 及位置参数；未知选项会停止识别。Tokenizer 在识别可执行命令前移除 Shell
反斜杠换行续行。RTK 源码不变。不会读取或推断自定义 `hooks.transparent_prefixes`；
配置此类前缀后，其中的 `find` 仍可能被 RTK 改写。

此设计有意以 RTK 覆盖率换取原生命令保真：混合复合命令、`-print0`、`-ls`、`-printf`
等非路径列表动作不使用路径列表压缩，少于三条路径或公共前缀无净收益的列表保持原样。
首条为 `.` 或不带路径分隔符的起始目录也不符合严格路径列表格式；此版本不引入单独的头部记录编码。
确定性的三条原生 find 回放中，60 条路径的列表得到压缩；含两条路径的混合复合命令和单路径
查询透传，所有返回路径均可直接读取。这只是 fixture 覆盖情况，不是 Agent 整轮收益度量，
也不是与 RTK Token 用量的比较。

Claude Code Adapter 仅提取无上下文选项的原生 Grep `mode=content` 响应，将其文本槽声明为
API 响应，再只替换原输出对象的 `content` 字段。包括宿主限额在内的其他元数据保持不变；
完整性指保留收到的所有记录，不表示宿主裁剪前的全部可能命中。
其他 Grep 模式和宿主保留既有路由。

RTK 输出所有权会在执行完成后再次确认。PreTool 在 Runtime 数据目录的 `rtk-outcomes.db`
原子保存原命令和实际执行的完整命令，并为每条改写后的 RTK 命令分配执行批次和序号，独立于统计开关。随包 RTK 记录
输出字节是否改变；Token 估算持平或增加不能证明内容未变。PostTool 按 Agent、会话、工具调用
一次性消费结果之前，要求其 `command` 与记录的命令（含执行批次）完全一致；命令缺失或被修改时
保留旁路，因此重试注册失败也不能复用旧结果。只有全部输出字节未变且工具执行成功，才使用
保存的原命令继续进入原有管线，
保留文件读取来源及其他命令相关保护。重试改写无法跟踪时尝试使前一批次失效；即使失效操作失败，
命令匹配也阻止此次重试复用旧结果。迟到结果不能重建已删除的批次。
输出已改变、结果缺失或过期、旧版 RTK、状态读写失败时，仍跳过后置压缩。复合调用要求每条
新改写命令都有结果；包含既有或无法跟踪的 RTK 命令、无法可靠解析的 Shell 替换时，保守地停用后续处理。
PowerShell Wrapper 目前不生成回执，保留 RTK 旁路。
Core 会记录结果查询失败的警告；RTK 侧写入失败保持静默，避免污染工具输出。
未消费结果一天后过期，后续改写通过索引清理过期记录。
管线沿用宿主声明的能力。Common Hook、Hermes 和 OpenClaw 在查询结果前已为 RTK 输出声明
`recovery: none`，因此回退仅接受无损候选；本次修改不会为这些调用启用 Stash/retrieve 恢复。
继续 PostTool 不保证产生压缩：候选结果仍须通过原有保真与收益检查。

搜索路径共享默认开启。CLI 可通过 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` 关闭；
未设置或为空时保持开启，`1`、`true`、`yes`（不区分大小写）也表示开启；其他值均关闭。
该变量独立于 JSON 配置文件。
Rust 使用 `RuntimeConfig.search_path_sharing_enabled`；Python 使用
`TokenlessConfig(search_path_sharing_enabled=False)` 或 `TokenlessRuntime` 的同名参数。
关闭该域时搜索列表原样返回且不计算搜索候选。其他工具名仍可使用 JSON、表格和日志压缩。
关闭路径共享时，已识别的原生 `find` 仍跳过 RTK 改写。这是独立于压缩的正确性边界：停用优化
不应重新引入摘要化或被改动的查询路径。完整恢复此前的 RTK 路由策略需要回退此变更。
精确名称 `Grep` 始终排除这些域，以保留全部已收到命中；关闭路径共享不会恢复此功能引入前
自定义 `Grep` 工具的 JSON、表格和日志派发。全局压缩开关仍控制已开启域的 dry-run。
整任务节省取决于工作负载；保留全部收到的字节并不保证后续工具调用更少或总 Token 用量更低。

两种视图有意共用 `search_path_sharing`，以该操作表示优化类别并保持 Protocol v2 不变。
统计和 `AppliedOperation.SEARCH_PATH_SHARING` 无法区分两种格式。此处描述的头部是内部
渲染细节，不是带版本保证的 SDK 判别字段；没有导出公开解码器或稳定的头部常量。
这些示例用于说明无损视图，不构成 SDK 程序化解码契约。

带行号记录的视图以 `[Search results grouped by file; all received lines retained]` 开头。
后续 `File=` 行包含 JSON 编码的路径；数据行必以 ASCII 数字和 `:` 开头，
因此不可能被误认为文件头。还原时在每个数据行前拼接当前解码路径与 `:`，保留原始行尾；
同一路径在非连续位置出现时会生成新的文件头。

解包的 Grep 列表在统计中记作 `api_response`，而非 `file_content`，因为它是筛选后的工具响应，
不是权威文件副本。按来源分组的历史查询因此存在版本边界。Core 将精确工具名 `Grep` 视为
仅适用搜索压缩的工具；即使搜索路径共享关闭，也不会转入 JSON、表格或日志压缩。SDK 调用者
仅应对符合此搜索契约的工具使用该名称。

## Stats 查询

`TokenlessStats` 是只读的公开查询客户端，复用 CLI 相同的 Rust `StatsRecorder` 和
`stats.db` Schema。它提供 typed 的状态、汇总、最近记录、记录详情、结构化 Diff 和
baseline 对比结果。`TokenlessSdk.stats` 会针对 Runtime 数据目录延迟创建该客户端，
因此 Stats 数据库损坏不会改变生命周期初始化或压缩侧的 fail-open 行为。这里的只读是
指公开操作；为与 CLI 保持一致，打开客户端时可能创建或迁移 `stats.db`，所以数据目录
必须可写。

Summary、List 和 Compare 只开放指标；记录详情以及 Record/Tool-use 的详细 Diff 可能
包含保存的工具内容，并继续遵守现有 1 MiB 输入和 500 行 Diff 上限。Token 数量是估算值，
Runtime 只记录候选确实减少估算 Token 的操作。Summary 或 Compare 未指定 Limit 时，
使用 Recorder 的 10,000 条记录上限；Session 和 Tool-use Diff 同样最多加载最近 10,000
条匹配记录。Compare 预期先传入 dry-run Baseline Session，再传入启用 Tokenless 的
Session；客户端不会推断或强制这两种模式。Python API 不清空数据，也不修改全局记录
开关。

## 打包与验证

`make python-wheel` 会构建 `tokenless`，把它暂存为
`anolisa_tokenless/_bin/tokenless`，再生成 CPython 3.11 stable ABI 平台 Wheel。跨平台构建器
可以通过 `PYTHON_TOKENLESS_BINARY` 指定为同一 Wheel 目标构建的 `tokenless` 文件。
`make test-python-runtime` 会在全新环境安装 Wheel，并在不依赖系统 `tokenless` 或 RTK 的条件下验证
四个生命周期和 Stats 查询。

`anolisa-tokenless-agentscope` 支持 AgentScope 1.0.11 至 1.0.x 和 2.0.x。1.x Adapter
使用 Tokenless Toolkit、模型代理和公开的实例 Hook；2.x Adapter 使用 `on_model_call`
和 `on_acting`。2.0.0 在配对的 Middleware/Tool 中保存 marker，后续版本还会把它持久化
到 `AgentState.middle_context`。两者都开放完整 SDK；2.0.0 支持直接构造 Agent，App
集成从 2.0.1 开始。内置工具具有显式契约；应用必须为每个自定义工具注册 `ToolContract`，
确保 `ContentOrigin` 不从输出文本推断。
