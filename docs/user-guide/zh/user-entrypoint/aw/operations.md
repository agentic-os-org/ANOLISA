# 运行维护

[English](../../../en/user-entrypoint/aw/operations.md)

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
内部 Hook 与生命周期回调保留协议特定的回复和错误码。兼容入口 `aw-package`
保留原解析器，包括安装时可自动查找二进制旁的 bundle。

## 管理结果

本地 bundle 的 `install`、`uninstall`、`config init`，以及
`config add-provider/remove-provider/add-event/add-hook/remove-hook` 支持局部
`--json`。旧原生连接 `install --config`、`agent connect`、`config validate`
和兼容入口 `aw-package` 不支持这个管理输出选项。

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --json
aw config remove-hook --config "$HOME/aw/aw.yaml" --event tool.before --id check --json
```

报告在 stdout 输出一个 JSON 对象。`format: 1` 标识格式版本，`command` 标识
操作，`status` 为 `succeeded` 或 `failed`。成功返回有类型的 `result`，例如：

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

`changed: false` 表示语义未变，保留原文件。安装结果包含所选前缀；配置生成结果
包含配置路径和模板名，不返回 Provider 配置值，也不代表运行时已生效。
`uninstalled` 表示所属包文件已移除，外部配置与状态保留。安装验证沿用现有
Preview 包检查，不代表发布者可信或原生 Agent 验收通过。

操作失败返回退出码 1 和 `failed` 对象，其中 `error` 包含 `code` 与固定摘要
`message`。错误码为 `io`、`invalid_json`、`invalid_yaml`、`package_contract`，
不会把底层原始错误复制进这份报告。成功返回 0。失败不声称全部变更已经回滚。
安装器已有的清理警告仍可能写入 stderr；这份报告不将这些警告替换为事务清单。

语法错误仍在 stderr 输出文本，退出码为 1，stdout 为空，即使被拒绝的参数中
包含 `--json`。默认人类诊断与显式 `config show` 保留原有契约；这不是全局
私有数据脱敏功能。服务 JSON、stdin/stdout 协议和原生 Agent 输出不套管理报告。

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
新包安装到新目录，再用新路径生成配置。保留旧包和备份，便于回退；
Preview 尚无跨版本数据迁移，升级时使用新的状态目录。

卸载使用安装时的前缀。系统安装执行下列命令，个人安装去掉 `sudo`。

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX"
```

卸载保留外部配置、审计和原生 Agent 数据。包文件被修改或缺失时会先报错，
按提示处理，不要直接删除仍在使用的目录。

## 保存数据

AW 的 `state_dir/journal` 保存调用元数据，不保存工具正文。
Tokenless 等组件有自己的数据目录和留存设置，参见[Tokenless 数据说明](tokenless.md#数据与隐私)。

需要跨重启保留记录时，在配置中使用明确的状态目录。该目录须归运行用户所有且权限为
`0700`。容器中还需要持久化该目录，以及框架自己的 profile 和工作区。
默认的自动状态目录可能位于临时存储。详细部署步骤见
[容器与持久化](../../../../developer-guide/zh/aw/native-agents.md#容器与持久化数据)。

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
