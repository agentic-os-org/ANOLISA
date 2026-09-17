# cosh-ng 排查指南

[English](../../../en/user-entrypoint/cosh-ng/troubleshooting.md)

排查 cosh-ng 问题分三步：先跑 doctor，再看磁盘上的运行时证据，需要求助时导出脱敏诊断包。本指南覆盖 cosh-ng 可能"无声失败"的几类问题：shell 退出但无报错、cosh-core 进程残留、输入不再路由到 Agent。

## 第一步 — 跑 doctor

先看活体事实。

- 在受影响的会话内运行 `/health`。它会探测 core 是否存活（有响应 = 存活；无响应 = 死亡或降级），并打印路由事实：生效的 AI 开关状态、integration 模式、`command_not_found_handler` 归属、最近一次路由决策。core 存活且路由事实能解释回退时，会给出命名 finding——"路由兼容性回退，非 provider 故障"。
- 在会话外运行 `cosh-shell doctor`。头部汇总 runtime（存活的 shell/core 进程，是否配对或孤儿）、logs（日志等级、各组件 24h ERROR 计数）、crashes（24h 崩溃次数与时间戳）。若 routing 行显示 live 探针不可用，请到受影响的会话内运行 `/health`。

按每条 finding 的 remediation 提示操作。多数 remediation 指向 `cosh-shell diagnostics export`，求助前先跑一次。退出码：0 健康、1 警告、2 错误。

## 第二步 — 读运行时证据

所有诊断状态都在 `~/.copilot-shell/` 下：

| 路径 | 内容 |
|---|---|
| `logs/cosh-shell.log.<date>`、`logs/cosh-core.log.<date>` | 按日日志；重点看事发时间附近的 WARN/ERROR 行 |
| `cosh-shell-crash.log`、`cosh-core-crash.log` | 每次 panic 一行 JSON（时间戳、版本、pid、panic 信息） |
| `run/shell-<pid>.json`、`run/core-<pid>.json` | 每个活跃会话一个文件；崩溃或 SIGKILL 留下的条目就是异常退出的证据。`cosh-shell doctor` 会报告它们，并给孤儿 core 提示确切的 kill 命令；新会话启动会清理超过一周的陈旧条目 |

默认日志等级看不到问题时，可临时提高详细度：

- `COSH_LOG=debug cosh-shell`（优先级最高），或 `RUST_LOG=debug`
- `~/.copilot-shell/config.toml` 中 `[logging] level = "debug"`

优先级：`COSH_LOG` > `RUST_LOG` > 配置 `[logging] level` > 默认 `info`。

## 第三步 — 导出诊断包

`cosh-shell diagnostics export` 收集脱敏诊断包：配置、日志、crash 记录、run registry 条目与健康事实。默认覆盖最近 24 小时；问题发生更早时扩大窗口：

```
cosh-shell diagnostics export --since-hours 72
```

诊断包是自解释的：evidence 字段携带组件版本、采集时间与各文件含义，不熟悉 cosh 的维护者或 Agent 也能直接解读。附上一句问题描述：

> 现象：…… — 发生时间：…… — 已尝试：`cosh-shell doctor`、`/health`、……

## 输入路由判读矩阵（zsh，自然语言输入不再路由）

Enhanced zsh 会话中，自然语言输入经 `command_not_found_handler` 路由。当它不再到达 Agent 时，在会话内判定原因：

```
typeset -p _COSH_AI_ENABLED _COSH_HAS_USER_COMMAND_NOT_FOUND
whence -v command_not_found_handler
```

| 观察 | 判读 | 处理 |
|---|---|---|
| `??` 能到达 Agent，但裸自然语言输入不能 | 路由或 hook 降级，不是 provider 故障 | 运行 `/health`，再导出诊断包 |
| `_COSH_AI_ENABLED=0` | 预期回退：AI 已关闭 | 重新开启 AI，或接受回退 |
| `_COSH_HAS_USER_COMMAND_NOT_FOUND=1` | 兼容性回退：用户自己的 command-not-found handler 优先 | 移除用户 handler，或接受回退 |
| 变量正常但输入仍不路由 | 采集 wrapper 覆盖或 marker generation 漂移 | 运行 `/health` 检查 marker generation，并附诊断包报告 |

`??` 是差分探针：它必然不是存在的命令，因此一定走 command-not-found 路径。若 `??` 能到达 Agent 而普通输入不能，说明 command-not-found 路径本身是通的，问题在路由决策或 hook 链——这正是 `/health` 报告为"路由兼容性回退，非 provider 故障"的事实。
