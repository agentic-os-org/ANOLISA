# 验证原生工具效果

[English](../../en/aw/validation.md)

通过允许、阻断和故障用例，验证原生 Agent 采纳 AW 策略结果。

## 四框架使用同一份配置

[共用 YAML](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/aw.shared.yaml)与
[布尔脚本](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/check-tool.py)让四个目标共用工具前
阻断、工具后观察策略。按[教程](integration.md)复制文件、
替换绝对路径并验证 Provider。这里沿用其 `AW_BIN`、`AW_CONFIG` 和 `AW_DEMO`
变量。包安装用户也可使用这两个文件，将 `AW_BIN` 指向已安装的二进制；Python 3
是该示例脚本的依赖。

先准备各 Agent 的受支持版本与原生模型配置。运行时只需安装所选目标。各框架使用
独立原生 profile，显式选择工具工作目录，并从 `$AW_DEMO/workspace` 启动。
共用 YAML 不修改模型凭据或原生权限，起步时逐个测试目标。

| 目标 | 原生前置条件 | 启动差异 |
| --- | --- | --- |
| Qoder | 支持原生命令 Hook 的 CLI >=1.1.64，已登录 | 交互或 print 入口；可选 `--native-settings` |
| OpenClaw | 2026.9.6，已配置的 JSON 和现有状态目录；先停止使用该 profile 的 Gateway | AW 新启 `gateway run`，须传入两个原生路径 |
| QwenPaw | 2.2.2.post1 / AgentScope 2.0.10，已初始化 App 工作目录与模型 | 设置 `QWENPAW_WORKING_DIR`，使用 App/API，不使用 ACP/TUI |
| Hermes | 干净的官方提交 `952c941e`、已安装的 Python console script 和已初始化的本地 chat profile | 在该 profile 中安装一次 AW 插件，运行时使用同一 profile |

启动 Qoder：

```bash
cd "$AW_DEMO/workspace"
"$AW_BIN" run --config "$AW_CONFIG" --agent qoder
```

使用已有原生绝对路径启动 OpenClaw：

```bash
"$AW_BIN" run --config "$AW_CONFIG" --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

使用已初始化目录启动 QwenPaw：

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home "$AW_BIN" run \
  --config "$AW_CONFIG" --agent qwenpaw -- --host 127.0.0.1 --port 8096
```

阅读 Hermes 的 [profile 要求](agents/hermes.md)后，安装并启动：

```bash
"$AW_BIN" install --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
"$AW_BIN" run --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
```

OpenClaw 和 QwenPaw 的服务终端保持运行，等 AW 输出原生 Hook 就绪信息后，再向
对应 Gateway 或 App 发起请求。OpenClaw 使用连接该 Gateway 的客户端，不使用
`agent --local`；QwenPaw 使用 App UI/API，不改用其他 CLI 入口。Qoder 和 Hermes
使用已启动的原生 chat。各框架的[接入页](native-agents.md)说明共存与生命周期。

### 验证原生行为

在另一个终端恢复教程中的 `AW_BIN`、`AW_CONFIG` 和 `AW_DEMO`，将 `AW_TARGET`
设为正在测试的目标。开始时标记文件应不存在；重复测试时换一个新名称。打印下述
两个提示词，分两轮发送到该目标的原生界面：

```bash
export AW_TARGET=qoder
printf 'Use your shell tool exactly once to run `printf AW_OK > "%s/%s.allowed"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
printf 'Use your shell tool exactly once to run `printf AW_DENY_TEST > "%s/%s.blocked"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
```

这些命令即使执行也只写入演示标记。第二条输入包含策略的拒绝标记。完成两轮后检查：

```bash
cat "$AW_DEMO/workspace/$AW_TARGET.allowed"
test ! -e "$AW_DEMO/workspace/$AW_TARGET.blocked"
"$AW_BIN" status --config "$AW_CONFIG"
ls "$AW_DEMO/state/journal"
```

预期读到 `AW_OK`，第二轮收到 AW 策略拒绝，且 blocked 标记不存在。通过 AW
读取对应 journal：从列出的目录选择文件，将去掉 `.jsonl` 的 64 字符文件名作为
`AW_AUDIT_KEY`。

```bash
export AW_AUDIT_KEY=REPLACE_WITH_EVENT_KEY
printf '{"method":"audit","key":"%s"}\n' "$AW_AUDIT_KEY" | \
  "$AW_BIN" request --socket "$AW_DEMO/state/aw.sock"
```

目录同时包含准备与事件 journal。事件的 `result.records[0].record.plan` 中，
`kind` 为 `event`，instance 和 binding 应与启动时输出的 `AW instance` 对应。before
拒绝调用应有 `policy_block: true`；完成的 after 步骤应指向 Provider
`check-tool` 且 `status: ok`。审计保存元数据，不保存原始输入/结果或效果的原因
文本。已闭合事件有 `terminal: true`。以下检查共同组成验收记录：

| 检查项 | 证据 |
| --- | --- |
| 允许工具 | 工具已执行、allowed 标记存在、before 及受支持的 after 步骤成功 |
| 策略阻断 | AW 返回拒绝、blocked 标记不存在、before 审计记录策略命中 |
| Provider 失败 | 停止旧会话/服务后，使用 before 脚本路径不存在的配置副本启动；调用失败，`on_error: block` 阻止标记生成，审计记录故障而非策略命中 |
| 共存 | 保留一个已有原生 Hook/插件，确认其预期行为与 AW 一起生效 |
| 重启 | 退出 Agent、停止 AW、使用原配置重新启动；新工具仍受检查，旧审计键仍可查询 |

记录框架版本、AW 提交、实际命令与审计键。合成 Host 测试与启动就绪不能替代该表。
模型拒绝、未调用工具或原生权限拒绝不算 AW 阻断。各框架的 after 覆盖不同，例如
Qoder 仅报告成功工具，不应要求为每次被拒绝的调用补造 after 事件。

### 停止与清理

退出 Qoder/Hermes，在自己启动的 OpenClaw/QwenPaw 服务终端按 Ctrl-C。等待这些
进程退出后，再停止共享 AW 服务：

```bash
"$AW_BIN" stop --config "$AW_CONFIG"
```

停止响应只确认收到请求；等待记录的 daemon PID 退出且 `aw.sock` 消失。
QwenPaw 正常退出时删除自己的临时插件。Hermes 保留已安装插件；没有 AW 绑定的
普通 Hermes 启动不会启用 AW 回调。撤销安装时，停止 Hermes 与原生配置写入，
核对安装时报告的 `backup` 和 `displaced` 文件，将 `plugins/aw-native-hooks`
移到 profile 的 `plugins` 目录之外，再恢复选定的安装前配置。保留安装后有意
进行的原生修改，不要盲目整文件覆盖。AW 没有原生插件卸载命令。

保留审计证据后，按[教程的清理步骤](integration.md#4-清理)
删除演示目录。框架 profile 单独保留。包卸载见
[包管理参考](reference/packages.md)。
