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
0.163.0 是原生验收基线，不是运行时版本白名单。必需的 `tool.before` 还会拒绝其他
PreToolUse Hook：Codex 并发运行匹配的 Hook，可能在 AW 检查原始输入后执行其他 Hook 的
`updatedInput`。非必需检查保留这种原生组合，证据仅对应原始输入。

Adapter 以会话和工具调用身份标准化原生事件。单个 AW 命令经 Service、Host、Provider
依次执行准入步骤。工具执行前的阻断使用 Codex 原生拒绝状态；允许结果仍由原生审批决定。
PostToolUse 仅提供观察，因为 Codex 没有通用工具结果替换合同。输出强制保护需要经过
独立验证的结果交付边界。

配置步骤见[接入指南](../../../../../../docs/developer-guide/zh/aw/agents/codex.md)，
支持的效果见[能力参考](../../../../../../docs/user-guide/zh/aw/capabilities.md)。
