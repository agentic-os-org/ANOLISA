# Codex CLI Adapter

[English](../../en/adapters/codex-adapter.md)

`codex` Adapter 通过 `aw run` 将本地 Codex 进程接入共享 Service 和 Provider 链。
`aw install` 为 UserPromptSubmit、PreToolUse 和 PostToolUse 各写入一个稳定的 AW 命令 Hook。
会话、模型、沙箱、审批和原生 Hook 调度由 Codex 管理；AW 负责启动准入、单个 AW 命令内
Provider 步骤的有序执行及允许或阻断结果。

安装有一个明确的发布边界：原子交换或首次不覆盖写入之前，观察到取消会保持当前 Hook 不变；
发布提交后，即使收到取消，也要完成同步和验证。被替换的 inode 持续保留，供未参与 AW 锁的
原生编辑器完成晚写入。任何保留备份都会阻止后续安装，包括无变更成功路径，直至操作者完成协调。
备份的存在构成跨崩溃的持久恢复状态；当前 AW 定义匹配不能证明旧内容已经协调。
人工确认步骤见[接入指南](../../../../../../docs/developer-guide/zh/aw/agents/codex.md)。
取消、发布失败和连续重装按状态转换组合测试。

启动前检查记录 CLI 报告的版本，并核对公开 Hook 清单、启用状态、信任状态和 AW 定义。
0.163.0 和 0.166.0 已有原生验收证据；报告版本不作为运行时白名单。必需的
`tool.before` 还会拒绝其他 PreToolUse Hook：Codex 并发运行匹配的 Hook，可能在 AW 检查原始输入后执行其他 Hook 的
`updatedInput`。非必需检查保留这种原生组合，证据仅对应原始输入。

本地 app-server stdio 也可作为启动入口。AW 沿用继承的 stdin/stdout 传输并检查同一套启动
Hook 清单；探测进程和正式进程均携带 Codex 远程控制禁用标记。0.166.0 在启动后报告禁用状态。
两个已验证版本在新线程沿用启动工作目录与原生配置时，均执行提示词、工具前后 Hook。
app-server 接受客户端 RPC 覆盖线程与轮次的工作目录和配置；原生实验中，
`thread/start.config` 关闭 Hook 后绕过了 AW。启动准入因此只覆盖受信、固定配置的客户端；
任意 app-server RPC 还需要独立的受控请求边界。

Adapter 以会话和工具调用身份标准化原生事件。单个 AW 命令经 Service、Host、Provider
依次执行准入步骤。工具执行前的阻断使用 Codex 原生拒绝状态；允许结果仍由原生审批决定。
Codex 0.163.0 和 0.166.0 可将 PostToolUse 的 `continue:false` reason 用作普通模型请求中的文本。
AW 只在关闭 Code Mode、没有其他 PostToolUse Hook 时准入这一 Hook 路径；最终替换步骤的
Provider 失败或结果缺少可投影文本时，使用 `withhold_result`。Codex 仍在 Hook 前捕获和记录
部分原结果，原生 Hook 命令失败时也可能交付原文；完整结果保护需要另行验证交付门禁。

配置步骤见[接入指南](../../../../../../docs/developer-guide/zh/aw/agents/codex.md)，
支持的效果见[能力参考](../../../../../../docs/user-guide/zh/aw/capabilities.md)。
