# 原生 Codex CLI Adapter

[English](codex-cli-adapter.md)

Codex 负责模型配置、调度、会话、工具执行和原生权限。AW 通过 Codex CLI 0.162.1 的
命令 Hook 接入，复用已有 launcher、Provider Host 和服务生命周期。

## Workspace 拆分后的职责

| 归属 | Codex 接入职责 |
| --- | --- |
| `aw-adapter` | 能力 catalog、参数/版本检查、Hook 清单与注册、原生标准化及回复解释 |
| `aw-service` | CLI 解析、私有绑定/链文件、回调顺序执行、RPC、事件预算与关闭、信号及实例清理 |
| `aw-host` / `aw-provider` | 已有 Provider 准备、调用及协议准入 |
| 独立 `providers/` workspace | 具体后端实现；本次不加入 Codex 专属实现 |

`crates/aw-adapter/src/adapters/codex/` 保存原生安装及启动前检查；
`crates/aw-adapter/native/codex/probe.py` 是打包的清单查询桥接。
标准化通过 `NativeContext` 只接收 target、实例标识和 cwd。类型化启动/安装选项与
调用方持有的 `LaunchFiles` 使 CLI 解析和资源归属留在服务。探测接收调用方的取消
标志，不注册信号处理器。

稳定命令 `aw codex-hook` 分派到 `crates/aw-service/src/cli/hook_chain.rs`，
由它核验私有注册/绑定文件、顺序调用已有 `aw hook` 传输并关闭事件。
原生拒绝解释委托 `aw-adapter::callback`；服务 RPC、socket、完整绑定和具体
Provider 依赖不进入 `aw-adapter`。独立 Provider 发布及包安装不属于本次变更。

## 注册与启动

`aw install` 向原生 home 的 `hooks.json` 添加 `PreToolUse`、`PostToolUse` 各一个
稳定命令，保留无关定义且不授予信任。用户通过 Codex `/hooks` 审阅确切定义；AW
可执行文件路径变更需要重新审阅。直接启动 Codex 时没有 AW 绑定，安装的命令返回中性响应。

创建绑定前，AW 检查 CLI 版本、准入参数，并通过短暂运行的 `codex app-server` 查询
公开 `hooks/list` 清单。`config/read` 确认 Hook 已启用。只读探针不创建 thread、不
编辑原生信任存储。每个配置事件必须恰有一个已启用、同步、可信的 AW handler，matcher
为 `.*`，timeout 为 60 秒。显式原生信任旁路只作为选项识别，出现在 `--` 后的提示词
中时不视为开关。

新本地 TUI 启动添加 `--no-daemon`，本地 `exec` 已在前台执行。两者继承原生配置，
通过 `AW_CODEX_CONFIG` 接收私有链文件路径。拒绝配置/profile 覆盖、worktree、cwd
切换、resume/fork 和远端入口，确保启动使用已核验的 Hook 清单。共用 launcher
负责进程、信号和绑定清理。

## 事件与效果

Codex 可以并发执行匹配的命令 Hook。因此 AW 为每个事件注册一个 handler，在内部
按配置顺序串行执行自己的步骤。独立用户 Hook 仍由原生调度。链在调用已有 `aw hook`
路径前核验文件归属、绑定、步骤集合、失败策略和预算。共享事件预算最多 55 秒，为
有界回调传输及事件关闭预留时间，保持在原生 timeout 内。

使用原生 `session_id`、`tool_use_id` 关联事件。标准化要求匹配的事件和启动 cwd、
非空标识符及对象类型工具输入。原生 payload 和结果保留给 Provider 及审计策略。

| 事件 | 采纳效果 | 原生响应 |
| --- | --- | --- |
| `tool.before` | `observe`、`block` | 中性 `{}`，或退出码 2 并在 stderr 返回拒绝文本 |
| `tool.after` | `observe` | 中性 `{}`，保留原始工具结果 |

响应不输出 `allow`、不改写输入、不覆盖原生授权。安全拒绝会停止后续 AW 步骤。
Tokenless 重试建议拒绝原调用，新调用重新检查。在 Provider discovery 前拒绝
`replace_result`：Codex PostTool 阻断反馈不是正常成功结果的替换契约。支持布尔
Hook 和完整 Provider；原始 native Hook 步骤保留在 Codex 配置中，不进入 AW 链。

拒绝或传输失败后，回调关闭跳过的事件步骤。Agent 退出及中断释放绑定和临时启动
文件，共享 AW daemon 保留到显式停止。安装定义保留供后续会话使用。回滚只删除
该可执行文件对应的两个 AW 定义，并恢复旧 AW 配置/二进制。

## 证据与边界

日常契约覆盖安装保留/幂等、回调顺序、原生失败结果保留、不支持/未信任/禁用定义
拒绝、信号清理及共享安全/Tokenless 回归。矩阵 version 2 要求每个能力单元格声明
可执行支持或准入拒绝测试；不支持的结果替换不能从回归契约中静默消失。

Nightly 安装固定版本 npm CLI，运行 `tests/test_codex_native.py`。本地 Responses
服务指示真实运行时执行或拒绝真实工具。确切用例报告覆盖信任拒绝、禁用 Hook、
允许/拒绝、策略不可用、新调用重试、独立原生拒绝、观察失败和长命令完成。
测试使用 ephemeral 会话、一次性 home，不需云端密钥。显式原生信任旁路与
`danger-full-access` 仅为夹具设置，AW 不自动添加。

这些证据不认证 TUI 信任交互、云模型、sandbox 强制隔离、MCP 或 code mode。托管
工具及通过 `write_stdin` 向运行中进程发送的输入不会触发新的 AW 前置授权。长命令
完成沿用原工具标识。原生外层 Hook 崩溃/超时不同于 AW 步骤失败，不能作为 OS 强制
执行保证。

集成契约来自[官方 Hooks 参考](https://learn.chatgpt.com/docs/hooks)及固定 CLI 导出的
公开 app-server schema。[用户指南](../../../../docs/user-guide/zh/user-entrypoint/aw/configuration.md#codex)
说明安装和支持的启动参数。
