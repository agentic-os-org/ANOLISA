# 交互命令

[English](../../../../en/user-entrypoint/cosh-ng/shell/interactive-mode.md)

本页介绍如何启动`cosh`并控制当前会话。运行`/help`可查看已安装版本实际支持的命令。

## 启动`cosh`

| 命令 | 用途 |
|---|---|
| `cosh` | 启动 Enhanced Assisted（`◇ `），提供 Agent 和斜杠命令路由。 |
| `COSH_SHELL_INTEGRATION=native cosh` | 启动不加载 Cosh Hook、不观察也不提供洞察的 Native。 |
| `cosh --shell zsh` | 明确选择zsh。 |
| `cosh --isolated` | 跳过用户rcfile。 |
| `cosh --login` | 启动login shell。 |
| `cosh --resume [id]` | 打开会话选择器或恢复指定会话。 |
| `cosh -c '<command>'` | 通过Shell执行一条命令后退出。 |
| `cosh -- <program> [args...]` | 直接执行程序后退出。 |

未指定Shell时，`cosh`使用配置或检测到的bash/zsh，无法确定时回退到bash。
集成状态在启动时确定，Enhanced 是默认值。需要长期使用无 Hook 会话时，在
用户配置中设置 `shell.integration = "native"`；只使用一次时设置环境变量。

执行命令或从重定向的 stdin 执行时，将 `--isolated` 放在 Shell 自有选项之前：
`cosh --isolated -c '<command>'`。Bash 会跳过启动文件，并从环境中移除 `BASH_ENV`
和 `ENV`。Bash login 调用（`--login`、`-l`/`+l`、`-lc` 等组合形式，或 `-cosh`
这样的 login argv[0]）会在 Shell 启动前以状态码 2 拒绝，因为 Bash 无法单独禁用
`.bash_logout`。隔离命令请使用非 login 调用。命令和脚本参数中出现的 login 选项
文本会原样保留。

在此 exec 路径中，隔离 Zsh 不接受 Shell 自有参数；携带这些参数时，Cosh 会在
启动 Zsh 前返回状态码 2。交互 TUI 启动继续使用现有的隔离处理。

## 输入和编辑

- 原生集成把每个输入字节交给前台 bash 或 zsh。
- Enhanced Assisted（`◇ `）把 Shell 语法交给前台 Shell，并可以把自然语言
  请求转为 Agent 请求。
- 在 Enhanced 的空提示符按 `Shift+Tab` 可切换到 Shell-only（`◌ `）。此时
  包括行首 `/` 在内的普通输入都交给 Shell，但仍可获得命令执行后的洞察。
  再次按下即可恢复 Assisted。
- 行首 `/` 只在 Enhanced Assisted 中运行 Cosh 控制命令。Native 和 Enhanced
  Shell-only 都把它留给 Shell。
- 终端支持时，`Shift+Enter`插入换行；多行粘贴仍作为一次提交。
- 上方向键历史包含Shell输入和斜杠命令。按`Ctrl+C`可取消当前命令或Agent请求。

## 增强集成斜杠命令

| 命令 | 用途 |
|---|---|
| `/help` | 查看已安装版本支持的命令。 |
| `/agent` | 编辑一次性 cosh-core 请求，可选择 Skill 并引用工作空间路径。 |
| `/health` | 运行本地健康检查。 |
| `/status`（`/about`） | 查看运行时、Provider和会话状态。 |
| `/stats [model\|tools]` | 查看模型身份或工具活动。 |
| `/auth` | 选择或更新Provider认证。 |
| `/config language [auto\|en-US\|zh-CN]` | 查看或设置界面语言。 |
| `/mode approval [recommend\|auto\|trust]` | 查看或修改工具审批。 |
| `/mode analysis [smart\|auto\|manual]` | 查看或修改主动分析。 |
| `/session ...` | 新建、列出、恢复、清理或压缩会话。 |
| `/recommendations [on\|off\|status\|privacy\|clear]` | 管理本地输入建议。 |
| `/hooks <command>` | 查看Hook发现和信任状态。 |
| `/extensions <command>` | 管理扩展包和设置。 |
| `/skills [list\|detail\|enable\|disable]` | 管理Skills。 |
| `/mcp [list\|connect\|inspect\|refresh\|disconnect\|login\|logout]` | 管理MCP服务器。 |

`/details`、`/audit`和`/send-to-shell`等命令只有在当前卡片或任务提供所需上下文时才会出现。`/mcp login`需要按MCP指南说明在Shell中完成OAuth流程。

## 编辑一次性 Agent 请求

配置使用 cosh-core runtime 时，可以运行 `/agent`。Agent Composer 会打开多行
编辑器，但不会改变后续 Shell 输入的路由方式。按 Enter 发送请求，按
`Shift+Enter`插入换行，按`Esc`取消并恢复 Shell prompt。

在第一个 token 输入 `/` 可浏览公开的 slash 命令。继续输入按前缀筛选（`/ho`
会提示 `/hooks`）；上/下键选择候选并滚动六行列表。Tab 替换命令 token，追加一个
空格以便输入参数。单行草稿只有命令 token 时，Enter 接受并执行选中的候选
（默认第一项）；带参数或多行的草稿按原文提交。即使列表已打开，Esc 也会取消
Composer。Slash 命令复用 Shell prompt 中的本地处理器和确认卡片。
提交命令后 Composer 结束：交互卡片接管输入，或在命令完成后恢复 Shell prompt。
未知命令名会显示本地错误，不会启动 Agent turn。

`/skill:<name>` 仍为 Agent 请求选择 Skill，`/skills` 用于管理 Skill。已存在的绝对
路径（如 `/tmp`、`/etc`），以及包含另一个斜杠的路径（如 `/tmp/file`）仍作为
Agent 文本。单独的 `/` 打开命令菜单；完整的已注册命令名优先于同名路径。
要在请求中讨论 slash 命令，
可显式添加 `??` 前缀，例如 `?? /help explain this command`。后续 token 中的命令
不会激活候选列表。括号粘贴中的 Tab 和换行仍为文本。普通 Shell prompt 保留原生
路径补全。

第一个 token 可以选择一个 Skill，后续任何以`@`开头、由空白分隔的 token 都可以
引用当前工作空间内的文件或目录。

```text
/skill:repo-review inspect @Cargo.toml @src
```

- `/skill:<name>`必须是第一个 token。所选 Skill 会在其他 Agent tool 之前调用。
- `@路径`必须指向工作空间内现有的文件或目录。绝对路径、父目录跳转、通过符号链接
  逃出工作空间的路径和不支持的路径都会被拒绝。
- 每次提交最多接受 16 个有效引用。目录默认不递归，除非请求明确要求遍历。
- cosh-ng 只发送验证后的路径元数据，在构建请求时不会读取或嵌入文件内容。
- Agent turn 开始前，界面会显示每个被拒绝引用的路径和原因；这些引用不会进入
  结构化引用上下文。

不指定 Skill 或引用的纯文本请求同样有效。其他 provider runtime 不提供`/agent`；
增强会话仍可直接输入自然语言请求或使用多行输入。

审批行为见[工具审批](approval.md)，主动的失败帮助见[AI分析](ai-analysis.md)。
