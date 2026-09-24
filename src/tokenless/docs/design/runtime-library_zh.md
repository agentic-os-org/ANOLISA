# Tokenless Runtime 库

[English](runtime-library.md)

## 目标

`anolisa-tokenless` 是框架无关的进程内 Tokenless SDK。平台 Wheel 同时包含 PyO3
Runtime 和固定版本的 RTK 可执行文件，因此 Python 应用不要求 `tokenless` 或 `rtk`
出现在 `PATH` 中。

公开的 `TokenlessSdk` 把宿主框架的四个生命周期映射为 Tokenless 行为：

| 生命周期 | 行为 |
|---|---|
| `before_model` | 可恢复的 Function Calling Schema 压缩，并按需发布恢复工具 |
| `pre_tool` | 对 Adapter 明确指定的命令字段执行 RTK 改写 |
| `post_tool` | 状态路由、响应压缩、TOON 候选选择和环境错误提示 |
| `retrieve` | 受可见 marker 授权的 byte-exact Stash 恢复 |

Tool Ready 在产品范围内硬关闭，不属于该 API。

## 协议与状态

Adapter 把框架对象转换为四组不可变 Request/Response：`BeforeModelRequest`、
`PreToolRequest`、`PostToolRequest` 和 `RetrieveRequest`。`Attribution` 要求 Agent 和
Session 标识；PreTool 与普通 PostTool 还必须提供 Tool Use 标识。工具 Schema 统一为
OpenAI Function Calling JSON，但生命周期操作是 Tokenless 自身契约，不是 OpenAI 请求。

`tokenless-runtime` 统一持有 SQLite Stash 和统计记录器。Schema 与响应压缩共享 Stash，
候选被丢弃时会回滚对应 key。TOON 作为 Rust 库直接链接，不启动进程。只有 Adapter
提供 `command_field` 时才调用 RTK；每个改写后的 wrapper 都锚定到 Wheel 内置文件，并
携带本次执行的归属信息。内容检测、阈值、TOON 选择、诊断、授权和 Stash 策略都保留在
Rust Core，而不是 Python 配置中。

SDK 不保存进程级“当前 Session”。`before_model` 返回精确的可见 Marker 集合。Adapter
声明自己是否已有 Marker 授权恢复路径，并持有面向 Agent 的命令或 Tool 声明。AgentScope
在模型调用之间保持 Retrieve Tool 静态，把 Marker 集合保存在框架 Session 状态中；`retrieve` 只授权该集合
中的 Marker。宿主应用继续保存原始工具结果供 UI 和业务逻辑使用，只把复制
后的最终模型可见文本传给 `post_tool`；Retrieve 输出禁止再次进入 PostTool。

非法输入、内置 RTK 缺失、生命周期操作失败、挂载失败和工具重名会快速失败。Passthrough、
No Savings 和 Recoverability Unavailable 等正常 Core Disposition 返回 typed 结果。候选
只有严格更短时才会采用；Schema 和响应截断还必须能够恢复。

## 搜索路径共享

PostTool 仅在搜索路径共享已开启、来源为 `api_response` 且支持文本替换时，将识别为
`search_results` 的内容交给 `SearchResultsCompressor`。这是按内容派发的 Core 能力，不限定 Agent ID；
文件内容和命令输出均不进入该域，包括未由 RTK 处理的 Bash 输出。
成功的 `path:line:text` 列表至少包含三条记录，连续记录可以共享完整文件路径。
行号、正文、顺序和换行保持字节可逆。输入契约不包含路径内的冒号和上下文列表；
任意不支持的记录都会拒绝整份候选。字节数与估算 Token 均减少后，再经过现有 Runtime 仲裁。
实际采用时操作为 `search_path_sharing`，Python SDK 对应 `AppliedOperation.SEARCH_PATH_SHARING`；
恢复等级为 `lossless`，不写入 Stash。

Claude Code Adapter 仅提取无上下文选项的原生 Grep `mode=content` 响应，将其文本槽声明为
API 响应，再只替换原输出对象的 `content` 字段。包括宿主限额在内的其他元数据保持不变；
完整性指保留收到的所有记录，不表示宿主裁剪前的全部可能命中。
其他 Grep 模式和宿主保留既有路由。RTK 负责的 Bash 输出继续绕过原生 PostTool 压缩。

搜索路径共享默认开启。CLI 可通过 `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` 关闭；
未设置或为空时保持开启，`1`、`true`、`yes`（不区分大小写）也表示开启；其他值均关闭。
该变量独立于 JSON 配置文件。
Rust 使用 `RuntimeConfig.search_path_sharing_enabled`；Python 使用
`TokenlessConfig(search_path_sharing_enabled=False)` 或 `TokenlessRuntime` 的同名参数。
关闭该域时搜索列表原样返回且不计算搜索候选。其他工具名仍可使用 JSON、表格和日志压缩。
精确名称 `Grep` 始终排除这些域，以保留全部已收到命中；关闭路径共享不会恢复此功能引入前
自定义 `Grep` 工具的 JSON、表格和日志派发。全局压缩开关仍控制已开启域的 dry-run。
整任务节省取决于工作负载；保留全部收到的字节并不保证后续工具调用更少或总 Token 用量更低。

首行为固定格式声明，后续 `File=` 行包含 JSON 编码的路径；数据行必以 ASCII 数字和 `:` 开头，
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

`make python-wheel` 会构建固定版本 RTK，把它暂存为
`anolisa_tokenless/_bin/rtk`，再生成 CPython 3.11 stable ABI 平台 Wheel。跨平台构建器
可以通过 `PYTHON_RTK_BINARY` 指定为同一 Wheel 目标构建的 RTK 文件。
`make test-python-runtime` 会在全新环境安装 Wheel，并在不依赖系统 RTK 的条件下验证
四个生命周期和 Stats 查询。

`anolisa-tokenless-agentscope` 支持 AgentScope 1.0.11 至 1.0.x 和 2.0.x。1.x Adapter
使用 Tokenless Toolkit、模型代理和公开的实例 Hook；2.x Adapter 使用 `on_model_call`
和 `on_acting`。2.0.0 在配对的 Middleware/Tool 中保存 marker，后续版本还会把它持久化
到 `AgentState.middle_context`。两者都开放完整 SDK；2.0.0 支持直接构造 Agent，App
集成从 2.0.1 开始。内置工具具有显式契约；应用必须为每个自定义工具注册 `ToolContract`，
确保 `ContentOrigin` 不从输出文本推断。
