# AW 命令参考

[English](../../en/aw/cli-reference.md)

本页集中说明 `aw` 的子命令、参数和作用。首次使用请从[快速入门](index.md)开始；下载安装步骤见[安装](installation.md)。

## 语法与帮助

下文的 `FILE`、`DIR`、`TARGET` 等大写名称都需要替换成实际值，不是原样输入的参数。`ABS_FILE`、`ABS_DIR`、`ABS_PATH` 要求绝对路径。选项放在所属命令之后；AW 没有全局 `--config` 或 `--json`。除可重复的 `--template-var` 和位置参数外，每个选项只提供一次。

```bash
aw --help
aw --version
aw commands
aw commands --json
aw run --help
aw config init --help
```

公开命令支持 `-h` / `--help`；根命令的 `-V` / `--version` 显示 AW CLI 版本。帮助或版本标志只能出现一次，且必须放在最后。帮助、命令枚举和补全生成均不加载配置、不启动服务。

`--agent TARGET` 选择 `spec.agents` 下的目标名，例如 `qoder`，不会安装该 Agent。AW 不展开配置文件中的 Shell 变量；向命令传参时，请由 Shell 展开。

## 公开命令总览

| 命令 | 用途 |
| --- | --- |
| `aw run` | 通过 AW 启动已配置的 Agent |
| `aw agent connect` | 注册原生 Hooks，或连接受支持的现有进程 |
| `aw config init` | 离线生成配置 |
| `aw config show` | 以 JSON 打印全部或部分配置 |
| `aw config validate` | 校验配置，可比较策略更新范围 |
| `aw config add-provider` | 添加完整的 Provider 定义 |
| `aw config remove-provider` | 删除没有被引用的 Provider |
| `aw config add-event` | 添加完整的事件定义 |
| `aw config add-hook` | 向已有事件追加步骤 |
| `aw config remove-hook` | 删除指定事件下的步骤 |
| `aw config native-sources` | 列出脱敏后的原生配置来源声明 |
| `aw config add-native-source` | 记录原生配置来源引用 |
| `aw config remove-native-source` | 删除来源引用 |
| `aw service serve` | 在前台运行本地 AW 服务 |
| `aw service status` | 查询已有服务 |
| `aw service stop` | 请求服务退出 |
| `aw update` | 对一个受支持且空闲的实例启用新策略 |
| `aw inspect` | 查询服务及可选的 Provider 摘要 |
| `aw install` | 安装本地 Provider 扩展；也保留旧版原生 Hook 连接语法 |
| `aw uninstall` | 卸载本地管理的 Provider 扩展 |
| `aw list` | 列出已记录的本地 Provider 安装 |
| `aw doctor` | 检查安装文件 |
| `aw recover` | 恢复中断的本地包操作 |
| `aw commands` | 枚举公开命令，可输出机器可读的参数信息 |
| `aw completion` | 打印 Shell 补全脚本 |
| `aw instance` | 使用 JSON 登记、确认、查询和释放业务自管实例 |
| `aw audit-export` | 无需控制连接，读取一页保留的公共审计元数据 |
| `aw request` | 将 stdin JSON 操作发送给服务，供集成与诊断使用 |
| `aw policy` | 运行 AW 的 command Provider 协议，供自定义规则集成使用 |

## 输出选项

以下管理命令支持共用输出选项：本地扩展模式的 `install`、`uninstall`、`list`、`doctor`、`recover`，以及 `config init`、`config validate`、`config add-provider`、`config remove-provider`、`config add-event`、`config add-hook`、`config remove-hook`、`config add-native-source`、`config remove-native-source`。

| 选项 | 默认值与含义 |
| --- | --- |
| `--json` | 用带版本号的管理报告代替面向人的摘要 |
| `--quiet` | 隐藏成功摘要，保留错误与清理警告 |
| `--verbose` | 在 stderr 补充操作范围信息 |
| `--color auto\|always\|never` | 默认 `auto`，控制管理摘要颜色 |

`--json` 与其他三个面向人的输出选项互斥；`--quiet` 与 `--verbose` 互斥。`--color` 可与任一面向人的输出选项组合。这些选项不改变原生 Agent 或协议流。

`service status`、`service stop`、`update`、`request`、`config show` 和 `config native-sources` 已经输出 JSON，不接受 `--json`。`inspect` 必须带 `--json`。`run` 保留 Agent 的输出、退出码和信号。JSON 报告字段与脚本退出码见 [CLI 自动化参考](../../../developer-guide/zh/aw/reference/cli.md)。

## 启动与连接

### `aw run`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--config FILE` | 必填 | 用于启动的 AW 配置 |
| `--agent TARGET` | 必填 | 配置中的目标名 |
| `--native-profile NAME` | 取决于 Adapter | 已有 Hermes profile 目录；其他 Adapter 可能拒绝此项 |
| `--native-settings FILE` | 取决于 Adapter | 原生设置；Qoder 可选设置叠加文件，OpenClaw 必填 |
| `--native-state-dir DIR` | 取决于 Adapter | OpenClaw 必填的已有状态目录 |
| `--reloadable` | 默认关闭 | 为受支持入口选择受限的稳定策略更新桥接模式 |
| `-- AGENT_ARGS...` | 可选 | 将字面参数追加到已配置的原生 argv |

OpenClaw 要求同时提供 `--native-settings` 与 `--native-state-dir`，并拒绝 `--native-profile`。Hermes 要求 `--native-profile`，并拒绝另外两个原生路径选项。在其他 Adapter 上使用这些选项前，请查阅 [Agent 集成指南](../../../developer-guide/zh/aw/native-agents.md)。

普通常驻 Qoder SDK 和 QwenPaw 无需 `--reloadable` 也支持策略更新。该标志选择另一种受限模式，并非所有 Agent 通用的更新开关。详见[能力支持](capabilities.md)与[策略更新](../../../developer-guide/zh/aw/deployment/policy-updates.md)。

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

AW 可按需启动本地服务；Agent 退出后，服务可能保留供后续复用。查询和退出方法见[服务与策略更新](#服务与策略更新)。

### `aw agent connect`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--config FILE` | 必填 | AW 配置 |
| `--agent TARGET` | 必填 | 配置中的原生目标 |
| `--native-profile NAME` | 取决于 Adapter | 持久注册时使用的原生 profile |
| `--pid PID` | 可选正整数 | 连接调用者所有、空闲且受支持的 Qoder TUI 或 QwenPaw App 进程 |
| `--remove` | 默认关闭；要求 `--pid` | 移除 AW 管理的连接 Hooks，保留原生进程运行 |

`--pid` 与 `--native-profile` 互斥。不带 `--pid` 时，执行 Adapter 支持的持久注册，不会安装 Agent 软件。带 `--pid` 时，需要通过就绪、所有权与空闲检查。前置条件和断开行为见[进程连接](../../../developer-guide/zh/aw/deployment/process-attach.md)。

## 配置

生成和编辑配置都是离线操作：不会启动 Provider、运行 Agent 或激活新策略。编辑前先停止使用旧配置的进程，或在编辑后按受支持的空闲更新流程应用。

### `aw config init`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--prefix ABS_DIR` | 必填 | 用于包和模板校验的已有安装前缀 |
| `--config ABS_FILE` | 必填 | 安装前缀之外的新私有配置文件，不覆盖已有文件 |
| `--state-dir ABS_DIR` | 必填 | 私有 AW 运行和审计目录，不得与安装文件或包元数据重叠 |
| `--qoder ABS_FILE` | 未提供 `--openclaw` 时必填 | Qoder 可执行文件 |
| `--node ABS_FILE` | 提供 `--openclaw` 时必填 | Node 可执行文件 |
| `--openclaw ABS_FILE` | 未提供 `--qoder` 时必填；要求 `--node` | 可读的 OpenClaw 入口文件 |
| `--provider none\|command` | 默认 `none` | 不调用 Provider，或生成布尔命令规则 |
| `--template ABS_FILE` | 可选；与 `--provider` 互斥 | 发行方或 Provider 提供的显式配置模板 |
| `--template-var NAME=VALUE` | 可重复；要求 `--template` | 字面模板变量，名称不得重复；`PREFIX` 与 `STATE` 由 AW 提供 |
| `--check ABS_PROGRAM` | `--provider command` 模式必填 | 布尔命令规则的可执行程序 |
| `--effect block\|observe` | `--provider command` 模式必填 | 命令返回 `true` 时的效果 |
| `--reason-code CODE` | `--provider command` 模式必填 | 非空原因码，最长 128 个 ASCII 字母、数字、`_`、`.` 或 `-` |
| `-- CHECK_ARGS...` | 可选；要求 `--provider command` | 传给检查程序的字面参数 |
| 共用输出选项 | 可选 | 见[输出选项](#输出选项) |

可选择 Qoder、OpenClaw 或两者。`--node` 与 `--openclaw` 必须同时提供。命令规则专用选项和末尾检查参数要求 `--provider command`；生成时校验可执行路径，但不会执行检查。已有状态目录必须归当前用户所有，权限为 `0700`。

模板变量名须为 1–128 个 ASCII 大写字母、数字或 `_`，不能覆盖保留变量。

生成器支持 Qoder 和 OpenClaw 入口。其他 Agent 按各自的[集成指南](../../../developer-guide/zh/aw/native-agents.md)编写配置。Provider 模板见[配置与启动](configuration.md)。

### `aw config validate`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--config FILE` | 必填 | 待校验配置 |
| `--against BASELINE_FILE` | 可选 | 同时检查改动是否属于允许的策略更新范围 |
| 共用输出选项 | 可选 | 见[输出选项](#输出选项) |

校验只检查配置和离线更新范围，不确认原生就绪状态、不启用策略。此命令允许相对配置路径。

### `aw config show`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--config ABS_FILE` | 必填 | 已有私有配置 |
| `--provider NAME` | 可选 | 选择 Provider，与 `--event` 互斥 |
| `--event EVENT` | 可选 | 选择事件 |
| `--id ID` | 可选；要求 `--event` | 选择事件内的 Hook 步骤 |

不提供选择条件时，以 JSON 打印完整配置。输出包含 Provider 私有值，分享前先检查。

### 配置编辑命令

下列命令均要求 `--config ABS_FILE`。定义文件包含一个完整的 YAML/JSON 对象。编辑会校验并发布完整配置，可能改写注释与格式，请先备份。表内的修改命令均支持[共用输出选项](#输出选项)。

| 命令 | 其他必填参数 | 含义 |
| --- | --- | --- |
| `aw config add-provider` | `--name NAME --definition FILE` | 在 `NAME` 下添加完整的 Provider 定义 |
| `aw config remove-provider` | `--name NAME` | 在相关 Hook 引用移除后删除 Provider |
| `aw config add-event` | `--event EVENT --definition FILE` | 添加完整事件，包括其中的步骤 |
| `aw config add-hook` | `--event EVENT --definition FILE` | 追加完整步骤，`id` 来自定义文件 |
| `aw config remove-hook` | `--event EVENT --id ID` | 删除步骤，事件为空时仍保留事件 |
| `aw config add-native-source` | `--agent TARGET --definition FILE` | 追加 AW 来源描述，不打开被引用的原生配置 |
| `aw config remove-native-source` | `--agent TARGET --id ID` | 删除描述，保留原生文件 |

`config show` 及上述编辑命令要求使用归当前用户所有、无符号链接的绝对私有配置路径。权限、幂等行为、定义格式和来源描述字段见[配置编辑器](../../../developer-guide/zh/aw/reference/configuration-editor.md)。

### `aw config native-sources`

要求 `--config ABS_FILE --agent TARGET`。以 JSON 打印脱敏的来源声明及顺序。这只是声明查询，不加载原生文件、不激活覆盖范围。不接受共用输出选项。

## 服务与策略更新

### `aw service serve`

要求 `--config FILE --state-dir ABS_DIR`。在前台运行，直到收到中断或退出请求。状态目录为私有目录，不安装 systemd 单元或系统服务。日常启动通常通过 `aw run` 按需启动本地服务。

### `aw service status` 与 `aw service stop`

两者均要求且只能选择下列一项：

| 参数 | 含义 |
| --- | --- |
| `--config FILE` | 通过原始启动配置或已记录的连接定位已有服务 |
| `--socket ABS_PATH` | 连接显式指定的本地 socket |

两个命令都不启动服务，均返回 JSON。`stop` 前先退出共享该服务的 Agent；外部 sec-core 或 AgentSight 服务由各自的管理方负责。详见[日常运维](operations.md)。

### `aw update`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--config FILE` 或 `--socket ABS_PATH` | 二选一 | 选择已有服务 |
| `--agent TARGET` | 未提供 `--instance` 时必填 | 选择配置目标；匹配多个根实例时还需 `--instance` |
| `--instance ID` | 未提供 `--agent` 时必填 | 选择实例；可与 `--agent` 组合，要求同时匹配 |
| `--policy FILE` | 使用 `--socket` 时必填，否则默认用 `--config` | 完整候选配置 |
| `--expected-revision SHA256` | 默认使用选中实例的当前版本 | 乐观并发前置条件，冲突不自动重试 |

目标必须支持更新，并在文档要求的清理边界保持空闲。输出为 JSON，不要添加 `--json`。离线 `config validate --against` 只检查更新范围。暂停派发、应用候选、回退和恢复见[策略更新](../../../developer-guide/zh/aw/deployment/policy-updates.md)。

### `aw inspect`

要求 `--config ABS_FILE --json`。查询服务状态和可选 Provider 摘要，可能执行配置中的 Provider view 命令。不会安装缺失后端。详见[日常运维](operations.md)和各 Provider 指南。

## 本地 Provider 扩展

这些命令管理本地解压的 Provider 扩展，要求 `package.json` 声明 `aw-provider/v1alpha1`。不接受 AW 核心、后端软件、`manifest.json` 包、URL 或 RPM/DEB 包。通过其他管理器安装的软件继续归原管理器所有。

### `aw install`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `--bundle ABS_DIR` | 安装扩展时必填 | 本地已解压的 Provider 包 |
| `--prefix ABS_DIR` | 安装扩展时必填 | 目标安装目录 |
| `--dry-run` | 默认关闭 | 显示文件、包和冲突，不写入 |
| `--scope user\|system` | 默认 `user` | 包数据库范围；system 写操作要求 root |
| 共用输出选项 | 可选 | 见[输出选项](#输出选项) |

旧的原生连接形式见[兼容与内部入口](#兼容与内部入口)，不能与扩展安装选项混用。

### `aw uninstall`

| 参数 | 必填 / 默认值 | 含义 |
| --- | --- | --- |
| `PACKAGE...` | 可选 | 要卸载的精确包 ID；省略则删除此前缀中所有已记录的包 |
| `--prefix ABS_DIR` | 必填 | 已注册安装目录 |
| `--dry-run` | 默认关闭 | 预览删除范围和阻碍，不写入、不询问确认 |
| `--scope user\|system` | 默认 `user` | 包数据库范围；system 写操作要求 root |
| `-y`、`--yes` | 默认关闭 | 确认删除，不绕过所有权、完整性或占用检查 |
| 共用输出选项 | 可选 | 见[输出选项](#输出选项) |

交互卸载会询问确认。JSON 或非终端卸载必须带 `--yes`。外部配置和状态会保留。

### `aw list`、`aw doctor` 与 `aw recover`

| 命令 | 参数 | 含义 |
| --- | --- | --- |
| `aw list` | 必填 `--installed`；可选 `--prefix ABS_DIR`、`--scope user\|system`、共用输出选项 | 读取安装记录，不支持远程搜索 |
| `aw doctor` | 可选 `--prefix ABS_DIR`、`--scope user\|system`、共用输出选项 | 检查已安装文件完整性与权限，不修复文件、不启动服务 |
| `aw recover` | 可选 `--scope user\|system`、共用输出选项 | 根据该范围的持久记录恢复中断事务 |

Scope 默认为 `user`；root 身份不会自动选择 system。省略 scope 时，`list` 和 `doctor` 可沿前缀已记录的范围查询；显式提供冲突的 scope 会被拒绝。不提供 `--prefix` 时，查询所选范围的全部记录。`recover` 会写入，system 范围需要 root。数据库位置和冲突处理见[包管理与恢复](../../../developer-guide/zh/aw/reference/packages.md)。

## 发现、补全与集成

### `aw commands`

无需必填参数。可选 `--json` 从与解析、帮助相同的声明生成带版本号的命令路径和参数信息。它描述公开语法，不表示已安装 Provider 或 Agent 能力支持；条件必填规则仍需查看命令帮助或本参考。

### `aw completion`

要求一个位置参数 `SHELL`，取值为 `bash`、`zsh`、`fish`、`powershell` 或 `elvish`。只向 stdout 打印脚本，不自行安装。例如：

```bash
aw completion bash > aw.bash
```

### `aw request`

供集成与诊断使用，要求 `--socket ABS_PATH`。可选 `--timeout-ms N` 范围为 `1..60000`，默认 `5000`。从 stdin JSON 读取一个服务操作，发送给已有服务并返回 JSON；不启动服务。日常使用优先选择 `service status`、`service stop` 和 `update`。[本地服务契约](../../../../src/aw/docs/design/zh/runtime/local-service.md)定义操作及传输数据。

### `aw policy`

除帮助外不接受选项。通过 stdin/stdout 提供 command Provider 协议，供 AW 管理自定义规则；不是查看或更新策略的命令。布尔命令集成见[自定义 Hooks](../../../developer-guide/zh/aw/providers/hooks.md)。

## 兼容与内部入口

| 保留的命令 | 推荐命令及差异 |
| --- | --- |
| `aw serve --config FILE --state-dir ABS_DIR` | `aw service serve`，参数相同 |
| `aw status (--config FILE \| --socket ABS_PATH)` | `aw service status`，参数相同 |
| `aw stop (--config FILE \| --socket ABS_PATH)` | `aw service stop`，参数相同 |
| `aw validate --config FILE [--against BASELINE_FILE]` | `aw config validate`；旧别名不接受共用输出选项 |
| `aw install --config FILE --agent TARGET [--native-profile NAME] [--pid PID] [--remove]` | `aw agent connect`；原生连接限制相同，不接受共用输出选项或扩展选项 |

以下隐藏命令由 Adapter 自动生成并调用，不是用户的配置步骤，不出现在 `aw commands` 或补全中。它们的私有 binding 与 stdin 协议必须匹配已安装 AW 的版本，请勿手工拼接调用或在 worker 间复制 binding。它们使用独立协议解析器，公开命令帮助和 `aw commands --json` 不枚举其接受的生命周期动作或消息。

| 内部命令 | 参数范围 |
| --- | --- |
| `aw hook` | 必填 `--adapter`、`--binding`、`--event`、`--step`、`--on-error`；可选 `--close-event true` |
| `aw bridge` | 必填 `--adapter`、`--binding`；原生连接确认时可用 `--confirm FILE` |
| `aw scope` | 必填 `--adapter`、`--binding`、`--action`；调用路由使用 `--event`、`--step`、`--on-error`、`--close-event true` |

内部生命周期动作和消息是随版本变化的实现细节。应用集成使用 [Agent 指南](../../../developer-guide/zh/aw/native-agents.md)中的托管工厂，由工厂管理这些调用与清理。

## `aw instance`

供显式 Adapter 连接已有同用户 daemon，首期支持 Qoder SDK / QwenPaw 普通路由。`--socket PATH` 必填；`--connect` 只发现服务身份，不读取 stdin。其余调用从 stdin 接收登记、Ready、查询或释放请求，始终输出 JSON。`--timeout-ms` 默认 30000，范围 1–60000，覆盖输入、连接和请求。

```bash
AW_SOCKET=/absolute/private/state/aw.sock
aw instance --socket "$AW_SOCKET" --connect
```

成功发现返回 `result.connected: true` 与服务 identity。业务继续管理自身进程；释放实例保留其他实例和 daemon。请求字段、Ready 接线、任务 Scope 与失败恢复见[独立实例接入](../../../developer-guide/zh/aw/deployment/independent-instances.md)。

## `aw audit-export`

以服务所属用户读取保留的审计元数据。必填 `--state-dir ABS_DIR`（包含 `journal/` 的
已有服务状态目录）和 `--request ABS_FILE`（普通 JSON 请求文件）。
可选 `--timeout-ms` 接受 1–60000，默认 5000。输出固定为 JSON；报告错误时退出码为 1。
服务停止后仍可读取；此命令不启动或停止后台进程。

使用服务配置指定的状态目录，或 `aw service serve` 的目录参数。
没有保留源描述符的旧 Journal 返回 `source_missing`，旧版链返回 `unsupported_history`。
有界枚举示例与消费进度规则见 [CLI 自动化参考](../../../developer-guide/zh/aw/reference/cli.md#公共审计导出)。
