# Codex CLI

[English](../../../en/aw/agents/codex.md)

AW 通过 `aw run` 启动本地 Codex CLI，接入 `UserPromptSubmit`、
`PreToolUse` 和 `PostToolUse` 命令 Hook。Codex 保留模型配置和原生权限。
AW 在每个事件的单个命令 Hook 内依次执行已配置的 Provider 步骤。

将 [sec-core 示例](../../../../../src/aw/crates/aw-service/examples/aw.codex-security.yaml)
作为一份 `aw.yaml`，覆盖提示词、工具输入和工具输出检查。填写 Codex、sec-core Provider、
CLI 和正在运行的 sec-core socket 的绝对路径。
先单独安装并登录 Codex。从 `aw.yaml` 所在目录运行：

```bash
aw config validate --config aw.yaml
aw connect --config aw.yaml --agent codex
codex
# In Codex, run /hooks and trust the three exact AW command definitions; then exit.
aw run --config aw.yaml --agent codex
aw service status --config aw.yaml
aw service stop --config aw.yaml
```

`aw connect` 在 `CODEX_HOME/hooks.json` 或 `~/.codex/hooks.json` 中加入稳定定义，
保留已有 Hook，也不授予信任。`aw run` 在启动会话前记录 Codex 报告的版本，检查 Hook 清单、
信任状态和 AW 策略。直接启动 Codex 时，已安装的 AW 命令处于空闲状态。
AW 可执行文件路径变化后，需要重新安装并审查信任。

AW 在同一 Codex home 内串行安装。更新已有 Hook 文件时，将被替换的文件保留为
`hooks.json.aw-backup`，编辑器通过已打开文件描述符提交的晚写入也会保存在其中。
每次重装都要求先协调这份备份，包括中断后的无变更重试，以及成功安装后的再次安装。
先停止原生 Hook 编辑器，核对当前文件与备份，合并用户修改，再将备份移出这个保留路径。
随后重新安装；修改 Hook 定义后需重新审查原生信任。
发布前观察到取消时保持当前 Hook 不变；发布提交后，AW 会完成同步与恢复检查。

业务客户端使用本地 app-server stdio 时，将 `spec.agents.codex.argv` 设为
`[/absolute/path/to/codex, app-server]`，由客户端以 stdio 子进程方式启动
`aw run --config aw.yaml --agent codex`。新线程沿用启动目录和
Codex 原生配置时，仍执行同一份 AW Provider 步骤。0.163.0 和 0.166.0 的真实
app-server 已通过提示词阻断、工具阻断和普通模型文本替换。启动准入只接受本地 stdio
（`app-server` 或 `--listen stdio://`），固定配置客户端沿用原生配置。下文的网关入口支持指定的模型参数。AW 在清单探测
和正式 app-server 中均设置 Codex 的远程控制禁用标记。Codex 0.166.0 在读取持久化状态前
消耗该标记；正式进程报告远程控制已禁用。

每个 Codex AW 实例最多累计保留 1024 条事件关联，已关闭事件仍占用容量。
一次工具调用的 before 和 after Hook 各占一条；提示词 Hook 共用这份容量，
启用提示词检查后会少于 512 对工具事件。在同一 app-server 内创建新线程不会重置计数。
耗尽时 AW 返回 `native_event_limit`；`on_error: block` 会阻断后续提示词或工具操作，
`withhold_result` 会扣留后续结果，`report` 步骤按既有失败策略继续。
业务应在达到上限前、当前任务结束后轮换 worker：关闭 stdio 子进程，等待 `aw run` 退出，
再启动新的 `aw run` 实例。旧关联随实例释放，新的实例恢复容量，共享 AW 服务可继续运行。

普通 stdio 客户端须保持线程与轮次的工作目录和配置不变。Codex 的 `thread/start.config` 可以关闭
Hook，AW 会将普通 stdio RPC 直接交给 Codex。此入口要求业务客户端受信，并用启动配置创建
新线程。若 worker 在轮次之间同步 `config.toml`，需要保持 Hook 启用及 AW 定义完整。
恢复、分叉、工作目录与配置覆盖、配置写入需要受控 RPC 边界后才能获得同等安全保证。

示例在模型请求前检查用户文本的提示词注入和 PII；对 Bash 工具，在执行前检查
代码与输入 PII。示例阻断未映射工具；其他工具需映射原生输入字段并验收后使用。
`tool.before` 标为必需时，AW 在启动前拒绝其他原生 PreToolUse Hook。
Codex 可能在 AW 检查原始输入后执行其他 Hook 改写的参数。若未标为必需，
已有 PreToolUse Hook 可改写实际执行参数；AW 的检查仅对应原始输入。
PostToolUse 记录完成事件。Codex 0.163.0 和 0.166.0 已验收示例中最后的
`scan_tool_output` 步骤：检测到输出 PII 时返回 `replace_result`，替换普通模型请求中的文本。
该步骤默认示例配置 `on_error: withhold_result`，在 Provider 失败或 Hook 结果缺少受支持的
文本投影时返回固定 withheld 文本；也可按需配置 `on_error: report`，仅记录失败审计并放行原结果。
已绑定的 PostToolUse Hook 在回调准备失败时仍会交付 withheld；超过 1 MiB 的 Hook 输入会在进入
Provider 扫描前触发这一处理。
启用替换时，启动准入拒绝 Code Mode 和其他 PostToolUse Hook。示例已设置
`pii.output_mode: replace_result`；sec-core 对命中的 PII 交付固定 withheld 文本，
当前不按字段保留原始结果结构。

这一 Hook 行为会把原生 Hook 标记为 stopped。CLI 报告版本保留为验收证据；启动准入
检查 Hook 接口，网关入口还要求版本为 0.166.0。Code Mode、未触发 PostToolUse 的工具失败、
策略前结果捕获、遥测、终端流式输出和原生 Hook 命令失败仍不受保护。MCP 结构化结果在普通
模型请求中转成替换文本。需要完整的输出交付保护时，应在原生工具或其他可强制执行的边界实现。

准入入口为新建本地 TUI、`codex exec` 和本地 `codex app-server` stdio。AW 拒绝其他
CLI 入口、远程监听、未经准入的启动配置覆盖和未知参数，因为这些操作可能改变已检查的 Hook 清单或会话边界。
命令行提示词必须放在 Codex 的 `--` 后，避免被解析为隐藏或未来新增的子命令。
TUI 目标使用 `aw run --config aw.yaml --agent codex -- -- "Check this project"`；
exec 目标配置 `argv: [/absolute/path/to/codex, exec]`，同样使用提示词分隔符。
`--ephemeral` 等选项放在 Codex 的分隔符之前。
`tool.before` 未标为必需时，其他原生 Hook 可与 AW Hook 并发执行；
Codex 自身的权限判定继续生效。
支持的效果见[能力矩阵](../../../../user-guide/zh/aw/capabilities.md)，
本地模型验收范围见[原生矩阵](../../../../../src/aw/tests/native-matrix.json)。

## 本地 Responses 网关接入 Codex 0.166.0

此入口使用从本修订构建的 AW 和 TypeScript SDK。受控 stdio 入口在首个线程创建前绑定 worker。在 AW 配置中固定以下 argv，
这样客户端遗漏网关参数时，AW 会在启动前拒绝该实例。

```yaml
argv: [/absolute/path/to/codex, app-server, --listen, "stdio://", -c, 'model_provider="gateway"']
```

1. 预热前创建私有 `CODEX_HOME`，确定实际工作目录的规范路径，两者在 worker 生命周期内固定。
   将已审查的沙箱、审批、Shell 环境、工具与 MCP 设置写入原生 `config.toml`。
   部署时隔离工具对 HOME、工作目录内生效的配置层和网关的写权限。
2. 在部署镜像上审查三个 AW 命令 Hook 的可执行路径、匹配器、同步模式和超时。
   按 `user_prompt_submit`、`pre_tool_use`、`post_tool_use` 保存对应原生
   `hooks/list.currentHash`，作为受信部署输入。若新 HOME 中每个 AW Handler 均位于
   第 0 组第 0 项，向 `reviewedHookTrust(home, pins)` 传入经 `realpath` 解析后的物理规范路径
   （Codex 按 `hooks.json` 的物理路径查找信任键），将其结果追加到生成的 TOML，
   在该 HOME 下执行 `aw connect --config aw.yaml --agent codex`。这可省去每个 HOME
   单独启动 Codex 查询信任哈希的步骤。启动探针仍会检查实际清单、信任和工作目录。
   定义变化后重新审查哈希。额外的生命周期 Hook 需要单独完成信任。
3. 启动带鉴权的本地 Responses 网关，用 `LOCAL_CODEX_GATEWAY_KEY` 传递密钥。
   将客户端的 spawn 入口改为下面的 AW 命令，安装和启动沿用相同 HOME 和 cwd。

```js
import { spawn } from "node:child_process";
import {
  gatewayArgs, reviewedHookTrust, forwardCodexResponses,
} from "@anolisa/aw-sdk/codex-gateway";

const gateway = { provider: "gateway", name: "Local Gateway", envKey: "LOCAL_CODEX_GATEWAY_KEY" };
const child = spawn(aw, [
  "run", "--config", awConfig, "--agent", "codex", "--", ...gatewayArgs(port, gateway),
], { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
```

追加的五对 `-c` 参数确定 Provider 名称、`http://127.0.0.1:PORT/v1` 地址、
环境变量名、Responses 协议并关闭 WebSocket。`gateway.provider` 须与 AW 目标的
`model_provider` 一致，ID 格式为 `[a-z][a-z0-9_-]{0,63}`。显示名称须非空，且不含引号或控制字符。
网关密钥使用专用环境变量，名称格式为 `[A-Z][A-Z0-9_]{0,63}`。保留项包括 `HOME`、`PATH`
及带有 `LD_`、`CODEX_`、`AW_`、`PYTHON` 或 `NODE_` 前缀的名称。库存探针与正式启动使用相同参数。
RPC 入口接受 `initialize`、`initialized`、`thread/start`、纯文本 `turn/start`
和 `turn/interrupt`。轮次只能引用本 worker 新建并返回的线程。新线程可指定启动 cwd、
模型、固定 Provider、临时线程标记、开发者指令、上下文窗口和推理强度。
恢复、分叉、动态工具、远端执行环境、注入 Hook、配置写入和其他覆盖会被拒绝。
工作目录变化时新建 worker。`aw instance prepare` 依赖原生注册或 SDK 工厂入口，
Codex 0.166.0 仅支持启动期绑定，已有 PID 无法补挂。

从控制器的线程配置中移除 Bash MCP PostToolUse 声明，由 AW 处理原生结果替换。
在带鉴权的模型网关中，逐次将解析后的 Responses 请求交给业务凭证解析所用的同一份
实时脱敏器。Responses 直转、Chat 转换和协议重试都须在回调内使用已检查的请求。

```js
return forwardCodexResponses(payload, text => redactor.text(text), (checked, counts) => {
  recordCredentialScan(counts);
  return forwardToConfiguredModel(checked);
});
```

回调取得已检查的请求及工具结果扫描数、变更数。遇到未知输入项、结构化工具输出、
间接引用的会话或脱敏器故障时，网关停止转发并向 Codex 返回错误。AW Provider 在网关脱敏前接收原始工具结果。凭证 PreToolUse
只能按允许原生改写输入的策略预装并完成信任，必需的 AW 输入检查会拒绝这个并行 Hook。
Shell 的 `aggregatedOutput`、MCP 的 `item/completed.result`、转录和通知消费者均可能在
模型出口前获得原文，需要在各自存储或导出的位置处理。
