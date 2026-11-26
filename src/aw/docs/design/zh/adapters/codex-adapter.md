# Codex CLI Adapter

[English](../../en/adapters/codex-adapter.md)

`codex` Adapter 通过 `aw run` 将本地 Codex 进程接入共享 Service 和 Provider 链。
`aw connect` 为 UserPromptSubmit、PreToolUse 和 PostToolUse 各写入一个稳定的 AW 命令 Hook。
会话、模型、沙箱、审批和原生 Hook 调度由 Codex 管理；AW 负责启动准入、单个 AW 命令内
Provider 步骤的有序执行及允许或阻断结果。

安装有一个明确的发布边界：原子交换或首次不覆盖写入之前，观察到取消会保持当前 Hook 不变；
发布提交后，即使收到取消，也要完成同步和验证。被替换的 inode 持续保留，供未参与 AW 锁的
原生编辑器完成晚写入。任何保留备份都会阻止后续安装，包括无变更成功路径，直至操作者完成协调。
备份的存在构成跨崩溃的持久恢复状态；当前 AW 定义匹配不能证明旧内容已经协调。
人工确认步骤见[接入指南](../../../../../../docs/developer-guide/zh/aw/agents/codex.md)。
取消、发布失败和连续重装按状态转换组合测试。

启动前检查记录 CLI 报告的版本，并核对公开 Hook 清单、启用状态、信任状态和 AW 定义。
0.163.0 和 0.166.0 已有原生验收证据；普通启动入口在运行时探测能力。必需的
`tool.before` 还会拒绝其他 PreToolUse Hook：Codex 并发运行匹配的 Hook，可能在 AW 检查原始输入后执行其他 Hook 的
`updatedInput`。非必需检查保留这种原生组合，证据仅对应原始输入。

使用普通本地 app-server stdio 入口时，AW 沿用继承的 stdin/stdout 传输并检查同一套启动
Hook 清单；探测进程和正式进程均携带 Codex 远程控制禁用标记。0.166.0 在启动后报告禁用状态。
两个已验证版本在新线程沿用启动工作目录与原生配置时，均执行提示词、工具前后 Hook。
app-server 接受客户端 RPC 覆盖线程与轮次的工作目录和配置；原生实验中，
`thread/start.config` 关闭 Hook 后绕过了 AW。启动准入因此只覆盖受信、固定配置的客户端；
任意 app-server RPC 还需要独立的受控请求边界。

0.166.0 网关入口接受六个独立的模型 Provider `-c` 参数，包括数字回环端口和存放
Bearer 密钥的环境变量名。五项 Provider 设置须归属 `model_provider` 指定的 ID。
转发器在启动时取得已校验的 ID，拒绝指定其他 `thread/start.modelProvider` 的请求。
库存查询使用正式启动的 argv、cwd 和 HOME。经审查的原生定义
哈希可按新 HOME 的路径生成信任条目。每个 worker 都检查实际生效的配置层，各 HOME
使用独立的实例身份与清单结果。

绑定记录的入口为 `app-server-gateway`。stdio 转发器接受整数请求 ID，
最多保留 128 个待响应请求和 1024 个线程 ID，单帧上限 4 MiB。转发器允许创建新线程，从原生响应中
登记线程 ID，拒绝能够恢复其他配置或注入 Hook 的请求。未知请求收到协议错误；
服务端发起的动态工具或审批请求收到不支持该方法的响应。原生通知按原数据转发。
此版本的 Codex 只能在启动时绑定，缺少协作注册工厂和运行中 PID 挂接入口。

Adapter 以会话和工具调用身份标准化原生事件。单个 AW 命令经 Service、Host、Provider
依次执行准入步骤。工具执行前的阻断使用 Codex 原生拒绝状态；允许结果仍由原生审批决定。
Codex 0.163.0 和 0.166.0 可将 PostToolUse 的 `continue:false` reason 用作普通模型请求中的文本。
AW 只在关闭 Code Mode、没有其他 PostToolUse Hook 时准入这一 Hook 路径；最终替换步骤的
Provider 失败或结果缺少可投影文本时，使用 `withhold_result`。Codex 仍在 Hook 前捕获和记录
部分原结果，原生 Hook 命令失败时也可能交付原文；完整结果保护需要另行验证交付门禁。

凭证协作模式下，控制器移除自己的 Bash PostToolUse，在带鉴权的 Responses 网关调用
业务实时凭证脱敏器。AW PostToolUse 先处理原始结果并应用准入的 PII 策略；网关逐条检查
请求中显式携带的文本工具结果，再将处理后的请求传给上游发送器或 Chat 转换器。
网关拒绝后停止转发。AW Hook 审计和网关的扫描数、变更数分别记录两个阶段。
Shell 的 `aggregatedOutput` 和 MCP 的 `item/completed.result` 位于 Hook 前。
[原生验收用例](../../../../tests/codex_gateway.py)检查这些通知、模型请求、错误的信任哈希和被拒绝的 RPC。

Codex PostToolUse 的公开审计根据 MCP `isError`、结构化退出码等显式字段，以及已识别
MCP `CallToolResult` 载荷中省略 `isError` 默认成功的语义确定 `resultStatus`。shell 的
纯字符串 `tool_response` 缺少退出状态时，审计保留 `aw.native_metadata_unavailable`，
导出该事件链时返回 `projection_unavailable`。

配置步骤见[接入指南](../../../../../../docs/developer-guide/zh/aw/agents/codex.md)，
支持的效果见[能力参考](../../../../../../docs/user-guide/zh/aw/capabilities.md)。
