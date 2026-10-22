# 运行维护

[English](../../../en/user-entrypoint/aw/operations.md)

以下命令使用安装时的配置路径。所有共享同一份配置的 Agent 退出后，再停止对应 AW 服务。

## 查看状态与停止

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`inspect` 包含 Provider 摘要。退出 Agent 后 AW 服务仍可运行，用 `stop` 停止，
再用 `status` 确认。sec-core 和 AgentSight 服务需分别管理。

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
