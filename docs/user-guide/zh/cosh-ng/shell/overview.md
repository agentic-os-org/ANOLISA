# 交互式终端

[English](../../../en/cosh-ng/shell/overview.md)

`cosh --help` 查看启动参数和公开命令入口，无交互终端时也可使用。
它只读取界面语言偏好，不启动内部 Shell 或认证流程。
内部 Bash 帮助使用 `cosh --shell bash --help`；增强集成的 Cosh 会话内使用 `/help` 查看现有会话命令参考。
`cosh guide on|off|status` 仅控制使用引导提示，不是所有 AI 能力的总开关。

## 配置向导与使用入门

`cosh guide` 打开配置与入门向导，首页菜单依次为：

1. **快速使用指南**：默认选中，从 Bash 首次提问开始。
2. **默认登录 Shell**：每次登录直接进入 Cosh，不必手动启动。
3. **AI 服务**：查看接入状态、管理已保存服务，不启动工作 Shell。
4. **登录健康摘要**。
5. **界面语言**。
6. **更多设置**：命令分析方式、个性化推荐与数据。

在 Bash 按需体验 AI，先按 Esc 逐级退出向导，再运行：

```bash
cosh --ask 'Why is my disk full?'
```

需要时按现有提示接入 AI。回答后仍在 Cosh，输入 `exit` 返回启动它的 Bash。
日常使用可运行 `cosh`，在同一提示符执行命令或自然语言提问，会话命令用 `/help` 查看。
已在 Cosh 中时无需再启动嵌套会话。

希望每次登录直接使用 Cosh 时，在向导首页选择**默认登录 Shell**。
核对账号与目标路径后确认，默认选中取消。sudo 如需密码会在当前终端提示，操作结束后才返回向导。
变更只影响后续登录，当前会话不变；有可信受管记录时可恢复原 Bash。
以 Cosh 作为登录 Shell 时，`exit` 会结束该次登录。

打开向导不会激活试用或调用模型，首次提问才验证自动接入；ECS 身份检测成功不代表具备试用资格或剩余额度。
已保存的 AI 服务配置显示连接未验证。独立 CLI `cosh guide on`、`cosh guide off`、
`cosh guide status` 继续控制使用提示，不列入向导设置菜单。

配置明确失败保留当前表单已确认字段，离开表单丢弃未保存输入。Esc 逐级返回；
Ctrl-C/Ctrl-D 结束 AI 管理，已提交任务先按实际结果收尾，取消不撤销已保存变更。
缺失写入回执时，本次向导暂停同类写入；读取当前状态不能确认先前操作结果。
部分成功分别保留配置已保存、会话记录未保存的事实。分析与推荐偏好供新 Cosh 会话使用，
环境覆盖会如实展示；Bash 历史导入需要单独同意。

首屏先展示一次健康事实，随后是一个健康与个性化共用的开放式推荐区（最多三条
问句）。单条不编号；多条按 `1.` 至 `3.` 编号，并以 `›` 标记当前问句。
`Shift+Tab` 循环当前问句，`Tab` 填入后可编辑，空推荐提示符上 `Enter` 直接提问。
若列表装不下终端或提示符已被其他程序占用，Cosh 保留已打印列表作为历史，仅在
提示符处显示当前序号与问句，不回写旧行。

三个入口各司其职：`cosh --help` 是静态启动参数与公开命令索引；`cosh guide` 是交互式
配置与入门；会话内 `/help` 是现有会话命令参考。受限终端（非 TTY、过小尺寸或受限 TERM）
下 `cosh guide` 退化为打印静态说明，不进入交互。

默认登录 Shell 修改需要 sudo 权限，且仅影响新登录；真实账号、VNC 与 RPM/发布验收
另行验证，不以向导单测推断。

`cosh` 默认启动 Enhanced Assisted 模式，并保持 Shell 原生提示符外观。
Cosh 可能在 bash 或 zsh 执行前路由自然语言输入。在空提示符按 `Shift+Tab`
可进入 Enhanced Shell-only。如果要求不加载 Cosh Hook、不观察也不提供洞察，
需要在启动时选择 Native。

状态符号默认关闭。在 `config.toml` 中设置 `shell.status_symbols = true`
（或单次会话使用 `COSH_SHELL_STATUS_SYMBOLS=1`）后，新提示符出现、卡片返回
或路由模式切换时会发布 `◇`（Assisted）或 `◌`（Shell-only）状态行，它随
终端输出滚动。Shell 提示符从下一行开始，保留正常的换行与光标移动；编辑重绘
不会追加状态行。Native 会话没有 Cosh 状态行。

## 典型工作流

1. 进入目标目录并运行`cosh`。
2. 像平常一样执行熟悉的命令。
3. 普通输入应只交给 Shell 时，按 `Shift+Tab`。
4. 在 Assisted 模式描述任务，并在允许副作用前检查卡片。
5. 离开长时间排查前运行`/session status`。

常用启动方式：

```bash
cosh
cosh --shell zsh
cosh --resume
COSH_SHELL_INTEGRATION=native cosh
```

## 输入如何分流

| 输入 | Native | Enhanced Shell-only（开启后显示 `◌`） | Enhanced Assisted（开启后显示 `◇`） |
|---|---|---|---|
| `git status` | 在 Shell 中执行。 | 在 Shell 中执行，之后可能提供执行洞察。 | 在 Shell 中执行，之后可能提供执行洞察。 |
| `hello` | Shell 通常报告命令不存在。 | Shell 通常报告命令不存在。 | 分类器会检查它，当前仍把这个有歧义的单词交给 Shell。 |
| `why did the last command fail?` | 由 Shell 处理。 | 由 Shell 处理。 | 携带最近终端证据启动 Agent 请求。 |
| `/session list` | 由 Shell 处理。 | 由 Shell 处理。 | 执行 Cosh 控制命令。 |
| Agent 工具请求 | 不可用。 | 明确接受洞察或进入 Agent 后可用。 | 按审批模式执行或显示审批卡片。 |

Native 不会安装 Cosh `DEBUG`、`RETURN` 或 `ERR` trap，也不会开启
`extdebug`、`functrace` 或 `errtrace`。Enhanced 是默认集成；使用
`shell.integration = "native"` 或 `COSH_SHELL_INTEGRATION=native` 选择
Native。切换集成需要重新启动 `cosh`，`Shift+Tab` 只切换 Enhanced 内部的
路由子状态，不需要重启。

增强集成中获批的 Shell 命令仍在前台 Shell 执行，prompt、输出、任务控制和
`Ctrl+C` 都可用。安全规则见[工具审批](approval.md)。

## Bash prompt 兼容边界

Enhanced 将用户的 `PROMPT_COMMAND` Hook 与 Cosh prompt Hook 组合执行。
集成启动前配置的用户 Hook 按所选 Bash 版本的规则保留执行顺序。
Assisted 和 Shell-only 均使用这套集成。

变量表示和子进程环境存在以下限制：

- Bash 5.1 及以上版本使用数组，保留原有 export 属性，但 Bash 不会导出数组值。
  因此，原先导出的 scalar `PROMPT_COMMAND` 不再传入子进程，原值为空字符串时也如此。
- Bash 4.3–5.0 使用不带 export 属性的组合 scalar，避免 Cosh 内部 Hook 文本
  进入子进程；用户原有的 scalar 同样不会被导出。

如果子 Shell 需要独立初始化 prompt Hook，请在各交互 Shell 的启动文件中配置。
如果会话必须保留 Bash 自身的 prompt 变量表示和环境行为，请在启动时选择 Native。
Native 不加载 Cosh Hook、不观察也不提供洞察；将 Enhanced 切换到 Shell-only
不会消除上述限制。

`--resume` 始终选择 Enhanced，即使设置了 `COSH_SHELL_INTEGRATION=native`
或 `shell.integration = "native"`。需要 Native 行为时，请勿使用 `--resume`。

这些限制仅适用于 Bash 的 `PROMPT_COMMAND`，不涉及普通环境变量或交互续行提示符 `PS2`。

## 会话与主动帮助

- 增强会话由 cosh-core 保存，并按启动 cosh 时所在工作空间隔离。恢复会话只
  恢复模型可见的对话内容，不恢复终端进程或旧终端输出。详见
  [会话恢复](session-recovery.md)。
- `smart` 是增强集成中的默认分析模式。调整主动失败帮助的方法见
  [AI 分析](ai-analysis.md)。
- `/help` 是增强集成命令集合的准确信息，简要参考见
  [交互命令](interactive-mode.md)。

## 下一步

- [工具审批](approval.md)
- [AI分析](ai-analysis.md)
- [会话恢复](session-recovery.md)
- [会话压缩](session-compaction.md)
- [Skills](../core/skills.md)
- [MCP](../mcp.md)
- [Extensions](../core/extensions.md)
