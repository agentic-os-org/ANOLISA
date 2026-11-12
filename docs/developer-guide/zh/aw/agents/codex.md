# Codex CLI

[English](../../../en/aw/agents/codex.md)

AW 通过 `aw run` 启动本地 Codex CLI，接入 `UserPromptSubmit`、
`PreToolUse` 和 `PostToolUse` 命令 Hook。Codex 保留模型配置、原生权限和工具结果。
AW 在每个事件的单个命令 Hook 内依次执行已配置的 Provider 步骤。

以 [sec-core 示例](../../../../../src/aw/crates/aw-service/examples/aw.codex-security.yaml)
为起点，填写 Codex、sec-core Provider、CLI 和正在运行的 sec-core socket 的绝对路径。
先单独安装并登录 Codex。从 `aw.yaml` 所在目录运行：

```bash
aw validate --config aw.yaml
aw install --config aw.yaml --agent codex
codex
# In Codex, run /hooks and trust the three exact AW command definitions; then exit.
aw run --config aw.yaml --agent codex
aw status --config aw.yaml
aw stop --config aw.yaml
```

`aw install` 在 `CODEX_HOME/hooks.json` 或 `~/.codex/hooks.json` 中加入稳定定义，
保留已有 Hook，也不授予信任。`aw run` 在启动会话前记录 Codex 报告的版本，检查 Hook 清单、
信任状态和 AW 策略。直接启动 Codex 时，已安装的 AW 命令处于空闲状态。
AW 可执行文件路径变化后，需要重新安装并审查信任。

AW 在同一 Codex home 内串行安装。更新已有 Hook 文件时，将被替换的文件保留为
`hooks.json.aw-backup`，编辑器通过已打开文件描述符提交的晚写入也会保存在其中。
每次重装都要求先协调这份备份，包括中断后的无变更重试，以及成功安装后的再次安装。
先停止原生 Hook 编辑器，核对当前文件与备份，合并用户修改，再将备份移出这个保留路径。
随后重新安装；修改 Hook 定义后需重新审查原生信任。
发布前观察到取消时保持当前 Hook 不变；发布提交后，AW 会完成同步与恢复检查。

示例在模型请求前检查用户文本的提示词注入和 PII；对 Bash 工具，在执行前检查
代码与输入 PII。示例阻断未映射工具；其他工具需映射原生输入字段并验收后使用。
`tool.before` 标为必需时，AW 在启动前拒绝其他原生 PreToolUse Hook。
Codex 可能在 AW 检查原始输入后执行其他 Hook 改写的参数。若未标为必需，
已有 PreToolUse Hook 可改写实际执行参数；AW 的检查仅对应原始输入。
PostToolUse 记录完成事件。Codex 0.163.0 可让最后一个 Provider 步骤返回
`replace_result`，替换普通模型请求中的文本。该步骤需配置 `on_error: withhold_result`；
Provider 失败或 Hook 结果缺少受支持的文本投影时，AW 返回固定 withheld 文本。已绑定的
PostToolUse Hook 在回调准备失败时也会交付 withheld；超过 1 MiB 的 Hook 输入会在进入
Provider 扫描前触发这一处理。
启用替换时，启动准入拒绝 Code Mode 和其他 PostToolUse Hook。sec-core 示例仍为观察模式；
可选的输出 PII 步骤使用 `scan_tool_output`、`effects: [observe, replace_result]` 和
`pii.output_mode: replace_result`。

复制示例配置，将 `spec.providers.security.config.pii.output_mode` 设为
`replace_result`，并在 `spec.events.tool.after.steps` 末尾追加：

```yaml
- id: output-pii
  provider: security
  operation: scan_tool_output
  effects: [observe, replace_result]
  on_error: withhold_result
```

这一版本专属行为会把原生 Hook 标记为 stopped。Code Mode、未触发 PostToolUse 的工具失败、
策略前结果捕获、遥测、终端流式输出和原生 Hook 命令失败仍不受保护。MCP 结构化结果在普通
模型请求中转成替换文本。需要完整的输出交付保护时，应在原生工具或其他可强制执行的边界实现。

准入入口为新建本地 TUI 和 `codex exec`。AW 拒绝恢复、分叉、远程模式、工作目录、
配置覆盖和未知参数，因为这些操作可能改变已检查的 Hook 清单或会话边界。
命令行提示词必须放在 Codex 的 `--` 后，避免被解析为隐藏或未来新增的子命令。
TUI 目标使用 `aw run --config aw.yaml --agent codex -- -- "Check this project"`；
exec 目标配置 `argv: [/absolute/path/to/codex, exec]`，同样使用提示词分隔符。
`--ephemeral` 等选项放在 Codex 的分隔符之前。
`tool.before` 未标为必需时，其他原生 Hook 可与 AW Hook 并发执行；
Codex 自身的权限判定继续生效。
支持的效果见[能力矩阵](../../../../user-guide/zh/aw/capabilities.md)，
本地模型验收范围见[原生矩阵](../../../../../src/aw/tests/native-matrix.json)。
