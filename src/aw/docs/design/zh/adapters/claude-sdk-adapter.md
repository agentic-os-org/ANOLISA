# Claude SDK Adapter

[English](../../en/adapters/claude-sdk-adapter.md)

显式常驻模式见[常驻 SDK 生命周期](resident-sdk.md)。
下文任务 worker 模式保留原有生命周期与失败策略。

`claude-sdk` Adapter 启动一个前台 Python worker，使用 Claude Agent SDK
及其 bundled Claude Code CLI，已验证基线为 SDK 0.2.88 / CLI 2.1.161。Python script 或 `-m` module 在公共工厂中
显式使用 `aw_claude.create_client(options)`。AW 通过私有启动目录和 `PYTHONPATH`
注入模块与 binding，不修改 SDK 全局构造器，也不推断业务入口。

## 工厂与就绪

工厂复制 options 与 Hook matcher 列表，保留调用方对象。它为 `PreToolUse` 添加一个
AW callback，并为 `PostToolUse`、`PostToolUseFailure` 各添加一个。AW steps 在该
callback 中顺序等待有界 `aw hook` 进程；SDK/CLI 调度与其他 matchers 继续独立执行。
工厂完成 SDK initialization 握手后才写同用户 readiness receipt。未创建 client 的
worker 不能报告成功就绪。每个 worker 持有一个任务 binding；Gunicorn 子 worker、
常驻多任务服务和远端执行器需要独立生命周期合同，本 launcher 不会自动接入。

Preflight 实际执行 bundled binary 的版本探测。工厂固定该 binary，拒绝调用方
`cli_path` 覆盖或缺失 bundled executable，不回退 PATH。模型配置与认证仍由 SDK
管理。worker options cwd 必须等于 AW 启动 cwd。

## 输入准入与恢复

配置 `input.submit` 后，工厂在调用 SDK 前检查 `query` 和常驻 `connect(prompt)`。
`aw_submission` 冻结受支持的文本消息，为本次输入生成一个 UUID turn，并复用
`submission::normalize`。事件 session 身份来自 SDK 输入信封的 `session_id`，
缺省使用 `query` 参数，其原生默认值为 `default`。这是提交标签；CLI 随后生成自身
的会话 UUID。AW binding instance 与唯一 turn 隔离客户端和连续输入。
工具 Hook 继续使用 CLI 原生 session 和 tool-use ID。

输入任务在迭代器验证及策略调用期间占用客户端。全部提交步骤与 `CloseInputEvent`
清理成功后，常驻客户端才启动工具调用账本。同次输入的步骤共享不可变内容与 Service
期限。`finally` 通过现有 CLI close-event 桥关闭事件，覆盖拒绝后未访问的步骤及取消时
正在执行的步骤。清理受到取消屏蔽，收到确认后才释放输入任务标记。输入准入拒绝不会
建立模型或工具调用账本条目。

已审计且可恢复的 Provider 故障复用现有 Claude callback 故障分类。`block` 返回输入
拒绝，`report` 返回中性结果；两种策略下风险拒绝都会终止本次输入。scope、身份、
协议、审计及无法确认的清理错误保留失败边界。常驻初始化成功后首条输入被拒绝时，
客户端保持已连接；启动故障仍关闭 SDK、relay 和 scope。前台受检客户端同样要求顺序
消费响应并拒绝重连。未配置输入检查的前台客户端沿用原生 SDK 类和输入模式。

原生 `UserPromptSubmit` Hook 在工厂边界之后执行，保留拒绝及附加上下文能力。
AW 不扫描其后续变换。准确的输入形状、限制和应用恢复方式见
[Claude 接入说明](../../../../../../docs/developer-guide/zh/aw/agents/claude-sdk.md#提交前检查用户文本)。

## 工具策略与身份

私有 `aw hook` 传输以退出码 2 和 stdout JSON 字符串携带明确阻断原因；
stderr 保留为本地诊断。原生命令退出码 2 按其合同将 stderr 作为阻断原因，
通过同一传输保留。

阻断返回原生 `permissionDecision: deny`；before `on_error: block` 的 callback
传输失败也明确返回 deny。中性响应保留原生权限；既有 Hook 可在 AW 并发执行时
拒绝工具，既有 allow 不能覆盖 AW deny。SDK 传入的原 pre callbacks 取得独立输入副本；
其 `updatedInput` 响应改为明确 deny。包装后的 `can_use_tool` callback 保留 allow/deny，
但发生 `updated_input` 修改时改为 deny。这些检查覆盖 Python SDK callbacks。
CLI 从 settings、项目配置或插件加载的 Hook 在包装范围之外执行，可以在 AW 检查公共
Hook 输入后改写普通 Bash 命令；改写后的命令可能执行，期间没有第二次 AW 检查。
此 Adapter 提供公共 Hook 覆盖，缺少最终参数的执行前检查接点。需要该保证的应用必须
另行采用经过验收的执行边界。AW 自身不返回批准。
启用 `tool.before` 路由时，工厂在 client 初始化前拒绝非空
`PermissionRequest` matchers：AW 无法检查其嵌套 `decision.updatedInput`，且
CLI 2.1.161 在 SDK control-protocol fixture 中没有落实其返回的权限决定。
请改用受支持的 `PreToolUse` 或 `can_use_tool` callbacks。启用 `tool.before` 时也拒绝
调用方指定 `permission_prompt_tool_name`；受保护 `can_use_tool` 的 SDK 内部 stdio
权限通道继续支持。

事件保留 native session ID 和 tool-use ID，在对应 AW instance 内关联 before/after；
原生 subagent 归属保留在 `native` 输入。Bash/Read/Write/Edit 对应 AW
bash/file_read/file_write；其他工具使用精确原生名称。身份、cwd、callback 事件或
参数形状无效时按配置的错误策略处理。

两类原生 post 事件均映射 `tool.after`。失败结果包含 native error 和 interrupt flag，
不会替换结果。SDK 0.2.88 + CLI 2.1.161 实测，两条 MCP 路径的失败行为一致：
报告错误的工具结果（SDK handler 合同的 `is_error`，映射为线上 `isError`）以
`PostToolUseFailure` 到达钩子，不含 `tool_response`（仅有 `error` 与
`is_interrupt`），模型收到原始错误结果。MCP 投影因此只覆盖成功结果，失败结果
保持原样；业务级成功仍以执行器/业务层的观察合同为准。
被拒绝的工具不补造 after。取消会回收 owned Hook 进程组，
不承诺远端工具副作用被取消。

## Bash 优化

复用 Tokenless Provider 的 `pre_tool` 与 `post_tool`，不新增 SDK 专属 Provider 协议。
PreTool 重试建议通过原生 deny 的有界原因完整返回，不返回 `updatedInput` 或批准。
模型的新调用重新经过所有 before 策略及原生权限检查。before 提前拒绝或 fail-closed
错误会关闭该 AW event，包括尚未进入的步骤。

配置 `replace_result` 时，Bridge 在同一原始 payload 上按配置顺序执行 AW after 链。
观察器与自定义步骤位于唯一的最终 replacement Provider 前，看到原始结果且没有文本
投影。所有步骤共享 service event 预算；report 失败保留原生结果，并在剩余预算内继续
后续步骤。独立 native matchers 保留原生调度并取得输入副本；配置 AW replacement
时拒绝其改写输出的响应。

原生 `Bash` 的 `PostToolUse`（非图片且未中断的 stdout）保持对象投影：最终返回
原生 `updatedToolOutput`，保留 stderr 与其余 envelope 字段。原生 HTTP MCP 工具
（`mcp__*`）投影模型可见字符串：存在 structuredContent 时为其序列化串，纯文本
结果时为单一文本内容块。替换以该字符串作为 `updatedToolOutput` 返回，bundled CLI
在下一模型请求采纳，保持 structuredContent 优先的原生语义。MCP 失败结果
不进入该路径：CLI 将错误结果以无 `tool_response` 的
`PostToolUseFailure` 送达，保持原始错误形态。媒体、多块及未知形状没有投影，
保持原样。RTK 重试命令的归属识别复用现有 Provider，避免重复压缩。

## 验证范围

`tests/test_claude_sdk_native.py` 使用真实 SDK、bundled CLI、SDK MCP server，
在 owned loopback HTTP port 返回确定 Anthropic 数据。它检查允许/阻断、Hook 失败、
原 Hook veto/allow/改参、permission 改参、拒绝 `PermissionRequest` matchers、
原生 Bash 失败、并行工具、两轮会话、连续 34 次策略拒绝/exit 9/SIGTERM fail-closed 后允许执行及
持久化 AW journal，不调用真实模型或云业务服务。运行文件/transcripts 存在 output
目录。可选 `--tokenless` 使用已构建的 Protocol v2 CLI，验证
Bash 模型可见替换、原始观察值、失败/report 行为、重试拒绝、受管 MCP 错误跳过压缩
及 HTTP MCP 错误结果保持原样（`PostToolUseFailure`，不压缩）。
私有 UDS 目录也在 output 下，并在 service 停止后删除。socket 路径过长时
明确要求选择更短的 output 路径。该验证不认证业务部署或其他 SDK
版本。可选测试固定 MCP 1.26.0：SDK 0.2.88 依赖范围可能选择 MCP 2.x，但后者缺少
SDK 使用的 `Server.list_tools` 方法。

工厂保留 `setting_sources`（包括原生默认发现）、内联或文件形式的 `settings`、
本地 `plugins` 和 SDK/HTTP/stdio `mcp_servers`。SDK 按原生顺序加载原始来源并发现
项目 Skills；AW 沿用其加载逻辑，配置文件内容保持不变。上文的 callback 组合与运行时
限制继续有效。`extra_args` 仍只允许 `strict-mcp-config: None`；settings 和 MCP 使用
SDK 对应选项传入。模型和认证配置不变。配置兼容范围与 Hook 效果分别验收，
Skills/plugins 内部执行的命令仍需单独核对覆盖边界。
每个前台 worker 只允许创建一个 client。

## 常驻模型路由

SDK 握手后、接受 query 前，固定版本原生 `get_settings` 控制返回 CLI 合并后的环境。
中继将其叠加到进程/options 环境，按原生来源优先级使用上游模型地址、代理和 TLS
配置，认证继续由 CLI 处理。上游环境在客户端连接时固定，变更后须创建新客户端。
`apply_flag_settings` 仅在会话内覆盖中继地址和关闭非必要
流量的标志，配置文件和其他 settings 保持不变。再次读取原生配置确认有效中继地址后，
connect 才返回；配置控制和中继启动共用五秒截止时间。控制接口不支持、路由未生效或
使用非 Anthropic 传输时，连接失败并清理 SDK、中继和 scope，不回退到直连模型。
`resident-model-settings` 验收覆盖内联/文件 settings、user、project/local、默认来源、
认证、两轮 query 及并发客户端。

显式常驻 [MCP handler 工厂](claude-mcp.md) 进一步支持可靠 MCP 成功状态分类及单文本替换，使用 Adapter 所属调用关联，并抑制已认领工具重复的 AW 原生回调。

`tests/claude_submission.py` 将固定 SDK/CLI、真实 AW、sec-core Provider/后端与记录请求
的本地 Anthropic 端点组合，覆盖前台和常驻客户端、常驻首条输入、字符串与异步文本、
Prompt/PII 风险、block/report 下的 Provider 非零/超时/无效响应、取消、迭代异常、
原生提交拒绝、后续输入及资源释放。Provider 传输故障由测试包装器注入，普通扫描进入
真实后端。外部模型和生产业务镜像需要分别验收。
