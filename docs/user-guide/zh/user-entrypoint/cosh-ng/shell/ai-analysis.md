# AI 分析

[English](../../../../en/user-entrypoint/cosh-ng/shell/ai-analysis.md)

增强集成可以检查命令失败和有价值的诊断输出，随后建议下一步或启动 Agent
分析。Assisted 和 Shell-only 都可提供命令执行后的洞察，只有
Assisted 会在执行前路由自然语言。Native 没有命令事件、洞察或 Agent 请求路由。

## 选择模式

Enhanced 是默认集成。运行时使用 `/mode analysis <mode>` 切换分析模式，也可以
通过 `shell.analysis_mode` 持久化。

| 模式 | 行为 |
|------|------|
| `smart` | 默认模式。评估失败和诊断输出，并展示有用的洞察供你复核。 |
| `auto` | 只对少量高置信失败自动启动分析；其他情况仍先提供建议。 |
| `manual` | 关闭主动建议、失败洞察、自动分析和个性化输入建议；需要时显式请求分析。 |

示例：

```text
/mode analysis smart
/mode analysis auto
/mode analysis manual
```

## 运行时行为

- 命令失败不一定会启动Agent请求。`cosh`会先判断失败是否可操作，以及现有证据是否可靠。
- 建议或操作卡片会让你决定是否分析；选择**跳过**即可保留当前命令结果。
- 分析会使用命令、退出状态和有界输出摘录，并在终端中流式显示结果。
- 分析进行时可按`Ctrl+C`取消。

用下面的配置设置默认模式：

```toml
[shell]
analysis_mode = "smart"
```

其他斜杠命令见[交互命令](interactive-mode.md)，环境变量覆盖见[配置](../configuration.md)。
