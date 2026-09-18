# 工具审批

[English](../../../../en/user-entrypoint/cosh-ng/shell/approval.md)

Agent使用受保护工具前，`cosh`可能显示审批卡片。允许操作前，请检查工具、输入内容、风险和Hook警告。

## 选择审批模式

运行时使用`/mode approval <mode>`切换，也可以设置`shell.approval_mode`。

| 模式 | 行为 |
|------|------|
| `recommend` | 只解释和建议，不发出工具调用。 |
| `auto` | 默认模式。符合条件的只读或低风险工具可以自动执行；高风险、受保护或外部操作会先询问。 |
| `trust` | 二次确认后，本次会话中的Provider工具请求自动执行。 |

在 `auto` 模式下，符合条件的简单只读命令可以使用 `2>/dev/null` 或
`2>>/dev/null` 抑制 stderr，无需审批卡片。支持重定向操作符后的空格，
以及 `/dev/null` 外的引号，例如：

```sh
find /tmp -maxdepth 3 -name '*cosh*' 2>/dev/null
```

抑制 stdout（`>/dev/null`、`1>/dev/null`）、混合输出重定向和重定向到普通文件
仍需审批。命令仍须通过只读规则和风险检查；抑制 stderr 不会使修改类命令获得
自动执行资格。执行结果保留 stdout 和退出状态，命令失败时也不例外。

这些自动命令在请求时的工作目录中使用受控环境执行。`HOME`、`LANG`、
`LC_ALL`、`LC_CTYPE` 和 `TZ` 来自 cosh 进程；交互 shell 后续通过 `export`
做出的修改不会同步，`GIT_*` 变量也不会传入。因此，时区、locale 和 Git
相关结果可能与在该 shell 中执行相同命令不同。需要 shell 当前环境的命令，
请直接在前台 shell 中执行。

使用下面的命令二次确认trust模式：

```text
/mode approval trust confirm
```

Trust模式不是无条件绕过。`reboot`、`shutdown`、`halt`等无法恢复的系统控制命令仍需审批卡片，高风险请求也不能创建持久trust key。

## 查看并处理卡片

检查工具名称、输入预览、风险和Hook警告，然后选择**批准**或**拒绝**。预览被截断时使用**详情**；有多个请求排队时，卡片会显示队列位置。

批准`shell`工具后，`cosh`会在前台bash或zsh中执行命令。命令输出和交互提示仍在终端显示，也可以按`Ctrl+C`中断。已批准的前台命令会逐个执行。

已批准命令等待密码、pager或普通终端输入时，`cosh`可以显示提示，并在默认120秒后中断等待。设置`shell.input_wait_timeout_secs = 0`可关闭超时；全屏TUI和管道读取不受此限制。

审批决定会保存在运行日志中。启用审计日志后，脱敏副本也会写入审计时间线；需要追查时请看[审计指南](../cli/audit.md)。

## 配置

```toml
[shell]
approval_mode = "auto"
trusted_commands = ["ls", "cat", "echo"]
input_wait_timeout_secs = 120
```

`trusted_commands`只匹配精确trust key，不按任意命令片段匹配，也不能绕过无法恢复命令的安全门禁。环境变量覆盖见[配置](../configuration.md)。

配置和环境变量也兼容旧值 `balanced`、`suggest` 和 `strict`，并按
`recommend` 处理。非法值会安全回退到 `recommend`；`/mode` 只接受三个
canonical 名称。
