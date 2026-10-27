# 配置与启动

[English](../../en/aw/configuration.md)

`aw.yaml` 保存 Agent 入口、Provider 和工具规则。先完成[安装与首次运行](installation.md)，下文沿用其中的 `AW_HOME`、`AW_PREFIX` 和 `AW_QODER`。

## 选择能力

创建配置时，按需要给 `aw config init` 增加参数。先安装对应 Provider。

| 目标 | 参数 | 完整步骤 |
| --- | --- | --- |
| 启动 Agent | 不加 Provider 参数 | [安装](installation.md) |
| 压缩工具输出 | `--provider tokenless` | [Tokenless](tokenless.md) |
| 检查工具命令 | `--provider sec-core --socket ABS_SOCKET` | [安全检查](security.md) |
| 同时启用两者 | `--provider sec-core+tokenless --socket ABS_SOCKET` | 先启动 sec-core 后台 |

`ABS_SOCKET` 换成实际后台的绝对 socket 路径。生成器目前支持 Qoder 和 OpenClaw；SDK 应用及其他 Agent 见[接入指南](../../../developer-guide/zh/aw/native-agents.md)。

## 修改配置

先退出使用这份配置的 Agent，再停止服务并备份。

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw config show --config "$AW_HOME/aw.yaml"
```

直接编辑 YAML。路径写成真实绝对路径，AW 不展开 `$HOME` 等变量。文件和父目录归当前用户所有，避免其他用户写入。`show` 会显示私有配置值，分享前检查内容。

修改后校验并重新启动。

```bash
aw validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

校验应输出 `configuration valid`，启动后进入 Qoder。用一次实际工具调用确认规则生效。回退时退出 Agent、停止服务，再恢复 `.bak` 文件并启动。

## 为 Hook 步骤选择工具

在已有 `tool.before.steps` 或 `tool.after.steps` 的某一步添加 `match`，只处理选中的工具。例如，配置已声明 Qoder 和 OpenClaw 时，可选择它们的 Shell 工具。

```yaml
match:
  tools: ["native:qoder:Bash", "native:openclaw:exec"]
```

事件和步骤的选择条件须同时匹配。省略 `match` 表示全部工具。原生名称区分大小写；扫描新工具时，还需配置 Provider 的[输入映射](../../../developer-guide/zh/aw/providers/sec-core.md#配置-provider)。完整选择器语法见[配置参考](../../../developer-guide/zh/aw/configuration.md)。

## 选择 Agent

`--agent` 取 `spec.agents` 中的目标名称。Qoder 从当前工作目录启动，`--` 后的参数传给原生程序。

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

### OpenClaw

需要 OpenClaw 2026.9.6 和已初始化的 profile。先停止使用同一 profile 的 Gateway，将以下占位路径换成已有安装与配置的绝对路径。

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

等待 `AW openclaw native hooks ready` 后，通过连接该 Gateway 的客户端使用。`agent --local` 会绕过它。启用安全或压缩时，在 `config init` 中增加上表参数。

## 自定义规则与服务部署

自定义规则可读取工具事件并返回 JSON 布尔值，见[Hook 教程](../../../developer-guide/zh/aw/providers/hooks.md)。
常驻应用的[策略更新](../../../developer-guide/zh/aw/deployment/policy-updates.md)、[外部进程接入](../../../developer-guide/zh/aw/deployment/process-attach.md)和[退出、并发设置](../../../developer-guide/zh/aw/deployment/lifecycle.md)由应用部署方配置。
