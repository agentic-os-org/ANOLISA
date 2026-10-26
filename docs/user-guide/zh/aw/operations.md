# 运行维护

[English](../../en/aw/operations.md)

以下命令使用安装时的配置路径。所有共享同一份配置的 Agent 退出后，再停止对应 AW 服务。

## 命令帮助与参数

先运行 `aw --help`（或直接运行 `aw`），再逐级查看分组或操作的帮助。
帮助不会加载配置、访问服务或修改文件。

```bash
aw --help
aw agent connect --help
aw config validate --help
aw service status --help
aw -V
```

公开命令同时支持 `-h` 和 `--help`。帮助须放在最后；后面追加参数或重复帮助
选项都会报错。版本查询仅用于根入口：`aw --version` 或 `aw -V`。
未知参数、重复参数、缺少值及选择冲突会在操作开始前失败。
普通参数错误保留退出码 1；帮助和版本查询成功返回 0。

参数只属于列出它的命令。没有全局 `--json`：`inspect` 必须携带它，
而 `service status`、`service stop`、`request`、`config show` 和运行时策略
`update` 本来就返回 JSON，不接受该选项。`update` 修改空闲实例的策略，
不是更新已安装的软件。`config show` 包含 Provider 私有值，分享前请检查。

支持 `--option value` 和 `--option=value`；值以 `--` 开头时使用后一种写法。
`run` 和 command 模板的 `config init` 中，`--` 后的参数按原样传递，
包括 `--help`、空字符串和空格。原生 Agent 保留原有输出、退出码和信号处理。
内部 Hook 与生命周期回调保留协议特定的回复和错误码。独立 `aw-package` 可执行文件已删除。
首次安装使用 bundle 根目录的 `aw`，必须显式提供绝对 `--bundle` 路径，不再自动查找旁边的 bundle。

## 命令发现与补全

无需加载配置、访问服务或执行 Provider 即可列出公开命令。清单和补全脚本与
解析及帮助共用同一份命令声明；隐藏的兼容别名与内部回调仍能调用，但不进入发现结果。

```bash
aw commands
aw commands --json
aw completion bash
aw completion zsh
aw completion fish
aw completion powershell
aw completion elvish
```

`completion` 向 stdout 输出脚本，不自动安装或修改 shell 启动文件。
检查后按对应 shell 的常规补全方式加载。必须提供 shell 名称，不支持的名称会在操作前报错。

`commands --json` 返回一个对象，包含 `format: 1`、CLI 的 `name`、crate `version`
和 `commands` 数组。每项提供完整命令路径、说明及可见参数，包括名称、值标签与
固定候选值。`required` 仅表示无条件必需；条件要求、冲突与参数透传仍以实际解析器
和操作帮助为准。这是语法清单，不是完整验证 schema、已安装包清单或运行时／Provider
能力报告。成功返回 0，语法错误或输出流失败返回 1。

## 管理结果

本地 bundle 的 `aw install`、`aw uninstall`、`aw list --installed`、`aw doctor`、`aw recover`、
`aw config init`、`aw config validate`、
`aw config add-provider`、`aw config remove-provider`、`aw config add-event`、
`aw config add-hook` 和 `aw config remove-hook` 支持局部 `--json`。
旧原生连接、`aw agent connect` 和旧 `aw validate`
不支持这个管理输出选项。

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --json
aw config remove-hook --config "$HOME/aw/aw.yaml" --event tool.before --id check --json
aw config validate --config "$HOME/aw/aw.yaml" --json
```

报告在 stdout 输出一个 JSON 对象。`format: 1` 标识格式版本，`command` 标识
操作，`status` 为 `succeeded`、`failed`、`cancelled` 或 `partial`。
成功返回有类型的 `result`，例如：

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

`changed: false` 表示语义未变，保留原文件。安装结果包含所选前缀；配置生成结果
包含配置路径和模板名，不返回 Provider 配置值，也不代表运行时已生效。
`uninstalled` 表示所属包文件已移除，外部配置与状态保留。安装检查本地说明文件和
文件内容，不代表发布者可信或原生 Agent 验收通过。

操作失败返回退出码 1 和 `failed` 对象，其中 `error` 包含 `code` 与固定摘要
`message`。错误码为 `io`、`invalid_json`、`invalid_yaml`、`package_contract`、
`invalid_configuration`、`cancelled`、`needs_recovery`、`package_in_use` 和
`plan_blocked`，不复制底层原始错误。
验证仅在离线执行；`policy_update_scope: true` 表示 `--against` 仅检查允许的编辑
范围，不代表运行时生效。原 `aw validate` 输出保持不变。

本地安装与卸载另提供 `mutation.committed`、`mutation.recovery` 和必要的
`warnings`。`committed` 表示包文件和安装记录已越过持久数据库提交点。
恢复状态为 `not_attempted`、`complete` 或 `incomplete`，仅覆盖本次操作跟踪的资源。
`not_attempted` 不表示没有写入；`complete` 不证明整机状态。
警告包含固定 `code`、资源 `path` 与可选的粗粒度 `io_kind`，不复制 I/O 错误字符串。

成功返回 0，已提交卸载但备份清理留下警告时仍然如此；警告在 JSON 和人类 quiet
模式均可见。失败且恢复不完整时使用 `partial`，退出 1，并保留主因 `error.code`，
包括取消。其他已观察到的包协作取消使用 `cancelled`，保持既有包退出惯例返回 1；
其余失败使用 `failed` 并返回 1。失败不表示全部变更已回滚。
包报告将通知放入 JSON，不额外输出 stderr 警告。

语法错误仍在 stderr 输出文本，退出码为 1，stdout 为空，即使被拒绝的参数中
包含 `--json`。默认人类诊断与显式 `config show` 保留原有契约；这不是全局
私有数据脱敏功能。服务 JSON、stdin/stdout 协议和原生 Agent 输出不套管理报告。

## 已安装包与文件检查

先列出指定安装目录中的包，再检查它们的文件：

```bash
aw list --installed --prefix "$AW_PREFIX"
aw doctor --prefix "$AW_PREFIX"
aw list --installed --prefix "$AW_PREFIX" --json
aw doctor --prefix "$AW_PREFIX" --json
```

`list --installed` 读取包记录，不检查已安装文件是否仍然存在或匹配。
`doctor` 逐个检查记录中的文件，报告缺失、内容变化、权限不符、符号链接和其他
文件类型异常。发现文件问题时返回 1，并列出所有检测到的问题，不自动修复；
检查通过返回 0。两个命令都离线、只读，不创建目录或锁文件，不启动服务、
执行 Provider 或更新安装记录。

对于已登记的 raw 安装，`doctor` 还检查数据库位置文件和已有的安装锁；所有包卸载后
保留的空安装也需要它们。缺失时会报告具体路径；清单仍保留中央数据库中的包信息，
便于继续诊断。

### 安装记录保存在哪里

省略 `--prefix` 时，两个命令读取所选包数据库，默认使用当前用户的数据库：
`$XDG_STATE_HOME/aw/packages`；`XDG_STATE_HOME` 未设置或为空时，使用
`$HOME/.local/state/aw/packages`。非空的 `XDG_STATE_HOME` 必须为绝对路径。
这个数据库与 `aw.yaml` 中运行时使用的 `state_dir` 分开。
`--scope system` 选择 `/var/lib/aw/packages`，这是 root 管理的 raw AW 数据库，
不是 RPM 或 DEB 数据库。`sudo` 不会自动选择 scope。显式指定安装目录时，查询按
目录中记录的位置读取数据库，不搜索其他目录。普通用户可读取可读的 system 数据库，
并按 system 安装生成自己的 AW 配置。

数据库的 `installed.json` 保存格式版本、记录更新编号和已安装包记录，包括包版本、
安装目录、依赖和所属文件。已有的 `lock` 用于协调访问，`transactions/` 保存未完成的
包操作。查询不会创建这些文件。数据库不存在时返回空清单；数据库已经存在但不完整、
格式错误或版本不支持时返回错误。发现未完成事务时返回 `needs_recovery`，不自动恢复。

新安装在 `<prefix>/.aw-packages/aw-core.json` 中只保存数据库位置、scope 和安装编号。
软件包信息以集中数据库为准，该文件必须与数据库对应，不能与旧组件记录共存。
旧安装器会拒绝这种文件格式，并在改变文件前退出。

旧 Preview 安装在 `<prefix>/.aw-packages/*.json` 中保存组件记录，查询时需要显式
指定绝对路径 `--prefix`。省略 `--prefix` 不会搜索旧安装。已登记目录又出现相冲突
的旧记录时，命令报错，不合并或悄悄丢弃记录。
旧记录没有保存原安装范围，因此文本显示 `not recorded`，JSON 使用 `scope: unknown`。
查询不导入旧记录。每次迁移尝试将原始 JSON 字节分别存档到所选数据库的
`legacy/<installation_id>/<transaction_id>/`，提交成功后才把旧记录替换成唯一的位置
文件。回滚尝试的存档单独保留，不会阻止旧安装器修改记录后的再次迁移。

第一次写入旧安装时还会检查记录中的 AW 可执行文件是否与当前命令一致。
不一致时会在迁移写入前返回 `runtime_unknown`，因为旧运行时尚不能参与新的包锁。
请保留旧安装，将此 Preview 安装到新目录；当前不支持 core 原地升级。

自动化检查可使用管理 JSON 输出。`doctor` 发现文件问题时返回 `status: failed`、
`error.code: integrity_check`，问题列表保留在 `result.report.issues` 中。
JSON 只写入 stdout，不另向 stderr 输出诊断。数据库或记录错误同样返回 1，不能把它们
当作空安装或文件检查通过。局部 `--quiet`、`--verbose` 和 `--color` 与其他管理命令遵循
相同规则。

## 安装计划与恢复

执行前先查看本地软件包的变更计划：

```bash
aw install --bundle /tmp/example-base --prefix "$AW_PREFIX" --dry-run --json
aw install --bundle /tmp/example-base --prefix "$AW_PREFIX"
aw install --bundle /tmp/example-mant --prefix "$AW_PREFIX"
aw uninstall example.mant --prefix "$AW_PREFIX" --dry-run
aw uninstall example.mant --prefix "$AW_PREFIX" --yes
```

以上示例使用 user scope。对于 root 管理的目录，加上 `--scope system`，并用 `sudo`
执行写操作；预览本身不授予权限。计划列出包名、精确版本、增加或删除的文件、依赖和
观察到的阻碍。无法执行的计划返回 1，带 `--dry-run` 时也一样。预览不创建安装目录、
数据库、锁或恢复目录。执行时持有写锁并重新检查输入，过期计划不能授权改变后的文件。
JSON 把这些信息保存在 `result.plan` 中，包括 `problems[].code` 和
`problems[].message`。受阻计划使用 `error.code: plan_blocked`，同时保留计划；
成功预览没有 mutation 报告。

独立 bundle 包含 `package.json` 和 `payload/` 文件树。说明文件保存包名、SemVer
版本、Linux 架构、每个文件的 SHA-256 和模式，以及精确依赖。普通工具无需声明
Provider，也无需依赖 `aw-core`。例如 `example.mant` 可以使用自己的 `7.4.2` 版本，
精确依赖 `example.base` 的 `2.1.0`。依赖必须位于所选安装目录中；命令不下载缺失包，
也不从另一个安装目录选取依赖。Preview 包保留原有的同构建要求。暂不支持替换已安装
的包或原地升级。
每个包文件最多 1 GiB。安装或追加计划按实际校验过的已有文件与新文件字节计算，
将同一安装目录中有记录的文件总量限制为 4 GiB。超限追加在任何写入前即被阻止，
避免装入之后超出后续卸载所需的校验范围。
同一目录最多保存 32767 个包属文件。计划还会核算恢复记录大小，
超过其容量的操作会在改变包文件前被拒绝。

卸载会检查所选数据库内的反向依赖，不删除没有记录归属的文件。文件被修改、缺失，
或协作的 AW 运行时仍持有该目录时，卸载会拒绝执行。移除最后一个包后，空登记、
数据库位置文件和目录锁仍保留，清单显示零个包。后续安装复用该目录，旧安装器仍不能写入。

中断的操作会留下持久恢复记录。停止包写入操作后，选择对应的 scope 执行：

```bash
aw recover --json
sudo aw recover --scope system --json
```

数据库已提交时，恢复会完成清理；未提交时则恢复之前的文件和记录。改变过或无法识别
的文件会被保留并报告冲突，解决前请保留报告中的备份。存在未完成事务时，普通查询
和包写操作返回 `needs_recovery`。恢复不执行 Provider，也不修复任意文件损坏。
如果无法确认数据库发布已持久化，操作会报告恢复不完整并保留事务记录。
恢复时会重新检查持久化条件，成功后才删除这些恢复依据。
包数据库不代表运行中的实例清单。

## 终端输出

上节列出的管理命令也支持以下局部人类输出选项：

| 选项 | 效果 |
| --- | --- |
| `--quiet` | 隐藏成功摘要，保留错误和安装器清理警告。 |
| `--verbose` | 在 stderr 补充固定的操作范围说明，不包含配置值。 |
| `--color auto` | 仅在对应输出流连接终端时为摘要着色，默认使用此模式。 |
| `--color always` | 即使输出到管道或环境变量禁用颜色，也为摘要着色。 |
| `--color never` | 禁用摘要颜色。 |

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --quiet
aw config remove-provider --config "$HOME/aw/aw.yaml" --name unused --verbose
aw uninstall --prefix "$AW_PREFIX" --yes --color never
```

`--quiet` 与 `--verbose` 互斥。`--json` 与这三种选项都冲突，不会被静默隐藏。
它们不是全局参数，不用于原生连接、服务或协议命令、`config show`
或旧 `aw validate`。转发分隔符 `--` 后的参数仍按原样传递。

非空 `NO_COLOR` 或 `TERM=dumb` 会禁用自动着色；显式 `always` 覆盖这些设置。
成功摘要使用 stdout，操作错误使用 stderr，分别检查各自是否连接终端。
呈现选项不授予权限，也不代替确认。新卸载入口使用下文的命令局部确认规则；quiet 不会跳过确认。

CLI 自己生成的管理摘要和操作错误，会转义元数据中的控制字符与双向文本控制符，
避免路径注入终端指令或额外行。安装／卸载通知同样转义，其他命令保留现有诊断格式。
这不是全局私有数据脱敏。
JSON 值和原生输出、协议流保留原格式。

输出流失败时，文件可能已经发布，命令仍返回非零；这不代表操作已回滚。
重试前先检查文件，CLI 不会为了补发输出而重试操作。

## 查看状态与停止

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`inspect` 包含 Provider 摘要。退出 Agent 后 AW 服务仍可运行，用 `stop` 停止，
再用 `status` 确认。sec-core 和 AgentSight 服务需分别管理。

分组命令保留相同的参数与 JSON 响应：

```bash
aw service status --config "$HOME/aw/aw.yaml"
aw service stop --config "$HOME/aw/aw.yaml"
aw service serve --config "$HOME/aw/aw.yaml" --state-dir "$HOME/aw/state"
aw service --help
aw --version
```

`serve` 持续在前台运行，直到被停止或中断。它不安装 systemd 单元，也不启动 Agent。
原有 `aw serve/status/stop` 命令保持兼容。`--version` 显示 CLI crate 版本，
不是已安装包的 Preview 发布标识或原生 Agent 版本。

## 升级与卸载

升级前退出 Agent，用旧版 AW 和原配置停止服务，备份配置与状态目录。
新包安装到新目录，再用新路径生成配置。保留旧安装目录和备份，便于回退；
Preview 尚无跨版本数据迁移，升级时使用新的状态目录。

回退时切回保留的旧安装目录与配置。新 core 只安装 `bin/aw`。已登记目录保存的数据库
位置文件会被旧安装器拒绝，复制旧安装器不能让它继续管理新目录。请把该文件和数据库
一并保留。

卸载使用安装时的前缀。系统安装执行下列命令，个人安装去掉 `sudo`。

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX" --scope system --yes
```

卸载保留外部配置、审计和原生 Agent 数据。包文件被修改或缺失时会先报错，
按提示处理，不要直接删除仍在使用的目录。
当前运行时在使用已安装 CLI、Provider 及其进程期间持有包读锁，卸载会拒绝仍被持有的
目录。如果记录中的 AW 可执行文件与执行卸载的命令不同，AW 无法确认旧运行时是否
支持这些锁，即使文件完整也会在计划中报告 `runtime_unknown`。请保留该目录，等待受支持的
迁移方式。这不会列举任意外部启动的程序，也不代表后续实例管理命令已经实现。

### 卸载确认

`aw uninstall` 仅在 stdin 和 stderr 都连接终端、且未选 JSON 时，在 stderr
显示 `Remove package-owned files from ...? [y/N]`。`--dry-run` 不询问确认，也不执行
卸载。直接回车、回答 no 或 EOF
都会拒绝卸载并退出 1；仅 `y` 或 `yes` 确认，不区分大小写并忽略首尾空白。
等待提示时尚未开始包操作；此阶段 SIGINT 和 SIGTERM 分别返回 130、143，不开始卸载。

自动化、重定向、无终端容器或 JSON 使用显式确认：

```bash
aw uninstall --prefix "$AW_PREFIX" --yes
aw uninstall --prefix "$AW_PREFIX" -y --json
```

非交互或 JSON 未带 `--yes` 时，在包操作前失败；JSON 返回 `failed`、
`error.code: confirmation_required`，没有 mutation 报告。向 stdin 管道输入 `yes`
不算确认。`--quiet` 仅隐藏成功摘要，仍需要确认。`--yes` 仅跳过本次提示，不绕过
归属、完整性、路径和权限检查，也不会执行 sudo。它仅用于卸载，不适用于安装、配置、
服务或原生 Agent 命令。

脚本使用 `aw uninstall --prefix ... --yes`。
包操作中的既有协作取消与取消提示阶段不同，仍按上文返回 typed 管理结果和退出码 1。

## 保存数据

AW 的 `state_dir/journal` 保存调用元数据，不保存工具正文。
Tokenless 等组件有自己的数据目录和留存设置，参见[Tokenless 数据说明](tokenless.md#数据与隐私)。

需要跨重启保留记录时，在配置中使用明确的状态目录。该目录须归运行用户所有且权限为
`0700`。容器中还需要持久化该目录，以及框架自己的 profile 和工作区。
默认的自动状态目录可能位于临时存储。详细部署步骤见
[容器与持久化](../../../developer-guide/zh/aw/native-agents.md#容器与持久化数据)。

## 常见问题

| 现象 | 处理方式 |
| --- | --- |
| 配置校验成功，启动仍失败 | 按错误检查 Agent 版本、可执行路径和 Provider 配置 |
| 修改配置后仍使用旧行为 | 用原配置或原 socket 停止旧服务，再启动修改后的配置 |
| 目录或配置权限不符合要求 | 使用当前用户拥有的私有目录，配置编辑时避免符号链接 |
| 服务协议不匹配 | 用旧二进制停止旧服务，再统一使用新安装的 AW |
| OpenClaw 显示结果替换不可用 | 检查支持的框架版本及 middleware API；此时安全检查和完成观察仍可工作 |
| 强制退出后留下 socket | 先确认原服务已退出，再移除该服务遗留的 `aw.sock`，保留 journal |
| 长时间运行出现 `native_event_limit` | 退出并重新启动 AW 管理的 Agent；新建原生会话不会重置实例计数 |
| 压缩统计为空 | 确认发生了受支持的工具调用，并检查 Tokenless 的录制设置 |

当前每个 Agent 实例最多关联 1,024 个原生事件；同时使用 before 和 after 时，一次工具
调用通常占两个条目。长期运行的 Gateway/App 需要安排实例重启。

检查策略是否生效时，应同时观察 AW 的处理结果和实际工具行为。
模型没有调用工具或被原生权限拒绝，都不能证明 AW 的规则已经执行。
