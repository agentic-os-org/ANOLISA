# Claude SDK Adapter

[English](../../en/adapters/claude-sdk-adapter.md)

`claude-sdk` Adapter 启动一个前台 Python worker，使用 Claude Agent SDK 0.2.88
及其 bundled Claude Code CLI 2.1.161。Python script 或 `-m` module 在公共工厂中
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

## 工具策略与身份

私有 `aw hook` 传输以退出码 2 和 stdout JSON 字符串携带明确阻断原因；
stderr 保留为本地诊断。原生命令退出码 2 按其合同将 stderr 作为阻断原因，
通过同一传输保留。

阻断返回原生 `permissionDecision: deny`；before `on_error: block` 的 callback
传输失败也明确返回 deny。中性响应保留原生权限；既有 Hook 可在 AW 并发执行时
拒绝工具，既有 allow 不能覆盖 AW deny。为避免最终参数未经检查，原 pre callbacks
取得独立输入副本；返回 `updatedInput` 时改为明确 deny。`can_use_tool` callback
保留 allow/deny，但发生 `updated_input` 修改时改为 deny。本合同不支持改参或审批。
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
不会替换结果。CLI 2.1.161 对 SDK MCP `isError: true` 响应仍发送 `PostToolUse`，
callback payload 不包含该 flag。因此 post 事件不证明业务成功；MCP 错误分类需要
执行器/业务层的观察合同。被拒绝的工具不补造 after。取消会回收 owned Hook 进程组，
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

只为原生 `Bash` 的 `PostToolUse`、非图片且未中断的 stdout 提供替换投影。最终返回
原生 `updatedToolOutput`，保留 stderr 与其余 envelope 字段。失败工具、MCP tools、
媒体及未知形状没有投影，保持原样。CLI 不提供 MCP `isError`，其结果压缩需要可信
业务状态合同。RTK 重试命令的归属识别复用现有 Provider，避免重复压缩。

## 验证范围

`tests/test_claude_sdk_native.py` 使用真实 SDK、bundled CLI、SDK MCP server，
在 owned loopback HTTP port 返回确定 Anthropic 数据。它检查允许/阻断、Hook 失败、
原 Hook veto/allow/改参、permission 改参、拒绝 `PermissionRequest` matchers、
原生 Bash 失败、并行工具、两轮会话、连续 34 次策略拒绝/exit 9/SIGTERM fail-closed 后允许执行及
持久化 AW journal，不调用真实模型或云业务服务。运行文件/transcripts 存在 output
目录。可选 `--tokenless` 使用已构建的 Protocol v2 CLI 与 PATH 中的 RTK，验证
Bash 模型可见替换、原始观察值、失败/report 行为、重试拒绝及跳过 MCP 错误压缩。
私有 UDS 目录也在 output 下，并在 service 停止后删除。socket 路径过长时
明确要求选择更短的 output 路径。该验证不认证业务部署或其他 SDK
版本。可选测试固定 MCP 1.26.0：SDK 0.2.88 依赖范围可能选择 MCP 2.x，但后者缺少
SDK 使用的 `Server.list_tools` 方法。

工厂要求 `setting_sources=[]`、无 `settings` 覆盖、无 CLI plugins。`extra_args`
仅允许 `strict-mcp-config: None`；SDK 原生 `strict_mcp_config` 也保留。模型和认证
配置不变。通过 settings sources 发现项目 skills 的应用，需要先明确接入设计才能
使用该首版受限 Bridge。每个前台 worker 只允许创建一个 client。
