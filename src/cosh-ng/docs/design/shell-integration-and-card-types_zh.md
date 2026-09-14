# Shell 集成与类型化卡牌

[English](shell-integration-and-card-types.md)

## 当前状态

显式的无 Hook Native 启动、默认 Enhanced Assisted、输入归属类型和 Agent
行内输入已经实现。Enhanced 会话内的路由状态也可在同一子 Shell 中切换。
Direct Exec 仅保留内部类型，没有用户入口，也不是当前可见状态。

## 架构决定

`cosh` 默认使用 `ShellIntegration::Enhanced` 的 Assisted 路由子状态。这保留
PR 前的产品行为，marker 和 OSC 命令事件、隐式自然语言分类、斜杠命令及
Agent handoff 默认可用。

用户可以在启动时选择 `ShellIntegration::Native`。此时子 bash 或 zsh 拥有
全部普通输入，并按原生规则加载启动文件。Cosh 不生成 marker rcfile，不安装
`DEBUG` trap，不修改 `PROMPT_COMMAND`，也不开启 `extdebug`、`functrace` 或
`errtrace`，不会暴露 marker token、观察命令或提供执行后洞察。

```toml
[shell]
integration = "native"
```

```bash
COSH_SHELL_INTEGRATION=native cosh-shell
```

非法集成值会以可见错误拒绝启动。集成状态在子 Shell 的整个生命周期中保持不变。
运行中的原生 Shell 若要开启增强集成，需要向该进程注入状态，因此本阶段要求
重新启动。

在 Enhanced 会话的空主提示符处，`Shift+Tab` 可以切换 AI 路由，不会重启子
Shell。Shell 进程、工作目录、变量、函数和后台任务都会保留。这个状态是
Enhanced 内部的路由子状态，不等同于没有 hook 的 Native 集成。OSC marker
仍然存在，用来证明提示符所有权并保证能够安全切回。

## 输入所有权与可见状态

符号首先描述按下 Enter 前的输入所有者，不描述已经产生的输出。输入类型保存在
`InputOwner` 中，渲染器不会根据用户文本首字符猜测路由。

| 符号 | 状态 | 所有者 | 行为 |
|---|---|---|---|
| 无 | Native | 子 Shell | 每个字节直接写入 PTY。Cosh 不装饰用户原提示符，也不观察命令事件。 |
| `◌` | Enhanced Shell-only | 子 Shell，Cosh 观察 | 包括 `hello`、`/` 和 `??` 在内的普通输入都交给 Shell。Enhanced marker 集成仍加载，因此执行后洞察和安全切换仍可用。 |
| `◇` | Enhanced Assisted | Shell 执行，Cosh 可路由 | Cosh 可以在 Shell 执行前观察、分类或路由提交的输入。 |
| `◆` | Agent | Agent runtime | `/agent` 打开无边框行内 Composer，并在可编辑文本前持续显示 `◆ `；普通文本（包括 `ls`）按 Agent 请求处理；开头的 slash 控制命令在本地分发。 |
| `/` | Cosh Command | Cosh 控制面 | 明确的斜杠命令，只在 Enhanced Assisted 中拦截。 |

`◇` 和 `◌` 在 PS1/PROMPT 前各占一个独立的外层终端状态行。Enhanced hook 的
`prompt_ready` 边界发布该行；Agent 或面板返回、路由模式切换也发布当前状态。
普通 candidate、ghost 和经过认证的 slash-guard 重绘只重画原提示符与输入，
不能追加状态行。状态行随输出滚动；明确切换路由时，先前状态留在终端历史中，
不会通过猜测旧状态所在的屏幕行来原地改写。

PS1/PROMPT 和 PTY 尺寸保持原值。子 Shell 从第 0 列绘制提示符，Readline/ZLE
因此能计算提示符与输入的每一格，包括 ANSI、CJK、组合字符和多行文本。
状态行不占用保留的终端区域；Shell 自主清屏/重绘可能移除它，直到下次发布或控制权返回。

Bash 既有的历史隐私提交保护仍会在接受经过光标编辑、镜像无法证明不含秘密的草稿时
添加一个前导空格。这是 accept-line 阶段的显示变化，与编辑区几何分开；参数字节和
隐私保护逻辑保持原样。

在 Enhanced 的空主提示符处按 `Shift+Tab` 会发布 `◌` 状态行并关闭 Cosh 输入拦截，
再次按下则发布 `◇` 并恢复路由，不向 Shell 提交空命令，也不重启子 Shell。
Shell 行已有内容时，按键序列原样交给 Shell；
prompt ghost 或卡牌处于活动状态时，保留原有的 `Shift+Tab` 行为。提示符边界门禁
保证快捷键不会误入 PS2、heredoc、前台程序或全屏应用。

内部类型模型为未来可能出现的结构化 `argv` 执行器保留 `DirectExec` 和 `▶`。
当前没有执行器和用户入口，所以 `▶` 不属于当前可见输入状态。

`/mode analysis` 的 `manual`、`smart`、`auto` 控制后台失败分析和建议策略，不能
改变当前输入所有者。输入归属与后台分析是两个正交状态。

原生输入绕过候选内容缓存、prompt ghost、斜杠路由和卡牌捕获。信号、终端尺寸
变化和 EOF 等终端控制仍由 PTY 生命周期处理。

## Composer 命令提交

Composer 的 capture 明确携带 Agent 输入来源。命令候选复用 registry 的公开名称，
按名称去重，保留完整集合，仅渲染当前选中项所在的六行窗口。输入线程使用当前
编辑器文本和光标同步计算 slash 候选，避免同一读取块内的 Tab 或 Enter 接受旧候选。
Enter 仅在单行草稿只有命令 token 时复用补全，并在提交事件前发送更新后的编辑器
快照。带参数或多行的草稿保留原文。
路径和 Skill 的 runtime 补全必须匹配文本与光标快照才能接受。

Capture 和提交事件保留 Composer 的工作目录快照，输入事件桥将它填入 slash
intercept 的 cwd，不依赖 ShellReady 时序或全局 prompt cwd 缓存。
提交事件携带 slash 路由标记；输入事件桥只生成一次 slash 或 Agent intercept。
Slash 提交不会保留待处理的 Composer 请求元数据。现有 slash/control consumer
负责解析、确认、执行以及 prompt 恢复；渲染器不执行命令，也不会自动重开 Composer
抢占后续卡片。普通草稿和 Shell 路径补全沿用原有路由。

## 输出事件卡牌

输出身份保存在 `CardKind` 中。输出符号说明事件类型，不参与输入路由，也不能
授予权限。

| 符号 | 事件 | 契约 |
|---|---|---|
| 无 | Agent Response | 标题与边框已清楚表明 Agent 回复，不重复显示输入态 `◆`。 |
| `/` | Slash Command | Cosh 控制面结果。 |
| `*` | Tool Call | 结构化 Agent 工具调用。 |
| `!` | Permission | 系统创建的请求，绑定具体 run、request、tool use、工具名称和输入。 |
| `·` | System | 只读状态或提示。 |

当前 UI 为斜杠面板、工具调用、权限卡牌和系统提示显示事件符号。Agent 回复
保留有框标题但不显示 `◆`。Shell 输出仍是原生终端流。

Permission 卡牌只能从结构化 `ToolPermissionRequest` 创建。`! allow` 之类
文本会留在原有卡牌内容中，不能授予权限。普通输出以任何卡牌符号开头时，也
不会再次解释。

## 对相关问题的影响

Native 为 #2687 提供了干净的架构边界。用户可以选择完全不存在 marker 选项和
trap 的会话，而不是隐藏其可观察状态。Enhanced v2 同时移除了全局 `DEBUG`
trap，不再强制开启 `extdebug`、`functrace` 或 `errtrace`。它通过有界的提示符、
command-not-found 和 PTY 集成点工作，同时保留用户自己的 trap 定义与选项状态，
因此解决 #2687 的可观察契约，但不把 Enhanced 描述成零注入模式。为了保留现有
产品语义，Enhanced Assisted 仍为默认值。

Enhanced 剩余的集成面是显式且有限的，包括 `PS0`、`PROMPT_COMMAND`、
`_cosh_*` helper、`command_not_found_handle` 和限定范围的 `COSH_*` 状态。
Native 仍是严格零注入选择。#2683 记录的 xtrace 输出，以及 #2541 记录的退出
状态和信号正确性，仍属于独立契约。

## 已知限制

- 修改 `shell.integration` 后仍需新建 `cosh-shell` 会话。
- `Shift+Tab` 只切换 Enhanced 会话内部的路由子状态，不能为 Native 子 Shell
  动态安装 Enhanced hook。
- 原生集成不提供隐式自然语言路由、斜杠拦截、命令边界账本、marker handoff
  或洞察。
- 当前原生会话没有安全的终端内 Agent 热键或面板。没有额外集成时，系统无法
  可靠证明 prompt 所有权。
- Direct Exec 没有用户入口，也不渲染输入态。
- 增强集成是有界集成而非零注入。若要求提示符、helper、环境变量和
  command-not-found 均无集成，必须在启动时选择 Native 会话。
- `_COSH_*`、`_cosh_*` 以及限定范围的 `COSH_*` 状态是 Enhanced 集成的保留
  实现命名空间。修改、取消设置或将这些变量标记为 `readonly` 会导致未定义
  行为；若要求严格隔离这些名称，请使用 Native 模式。
