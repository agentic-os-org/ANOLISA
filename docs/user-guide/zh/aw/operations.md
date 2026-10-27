# 日常维护

[English](../../en/aw/operations.md)

## 查看状态与停止

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`status` 查看实例，`inspect` 查看 Provider 摘要。先退出共享这份配置的所有 Agent，再停止 AW，并用 `status` 确认服务已退出。sec-core 和 AgentSight 服务由各自管理者维护。

## 检查安装

沿用安装时的 `AW_PREFIX`。

```bash
aw list --installed --prefix "$AW_PREFIX"
aw doctor --prefix "$AW_PREFIX"
```

`list` 列出安装记录；`doctor` 检查文件是否缺失、被修改或权限异常。通过时返回 0，发现问题时返回 1 并列出路径。

## 升级与卸载

1. 退出 Agent，用旧版 AW 和原配置停止服务。
2. 备份配置与状态目录，将新包[安装](installation.md)到新目录。
3. 用新路径生成配置，使用新的状态目录，验证实际工具调用。
4. 保留旧安装和备份；回退时停止新服务，切回旧路径与配置。

系统安装按以下命令卸载。个人安装去掉 `sudo` 并改用 `--scope user`。

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX" --scope system --yes
```

卸载保留外部配置、审计和 Agent 数据。包文件被修改、缺失或仍在使用时，按错误处理后重试。
出现 `needs_recovery` 时，停止包写入操作，按[包恢复步骤](../../../developer-guide/zh/aw/reference/packages.md)处理。

## 数据与排错

`state_dir/journal` 保存 AW 调用元数据。Tokenless 的正文留存另见[数据与隐私](tokenless.md#数据与隐私)。

| 现象 | 处理 |
| --- | --- |
| 校验通过，启动失败 | 检查错误所指的 Agent 版本、程序路径和 Provider 配置 |
| 配置修改未生效 | 退出 Agent，用原配置停止旧服务，再启动新配置 |
| 配置或目录权限错误 | 使用当前用户拥有的私有目录，配置编辑避免符号链接 |
| 服务协议不匹配 | 用旧二进制停止服务，再统一使用新 AW |
| 强制退出后留下 socket | 确认原服务已退出，再移除它遗留的 `aw.sock`，保留 journal |
| `native_event_limit` | 重启 AW 管理的 Agent 实例；仅新建原生会话不会重置计数 |
| 压缩统计为空 | 确认发生了受支持的工具调用，并检查录制设置 |

更多命令可用 `aw --help`、`aw commands` 查询。补全脚本由 `aw completion bash`（或 `zsh`、`fish`）输出。
自动化报告见[CLI 参考](../../../developer-guide/zh/aw/reference/cli.md)，容器持久化和容量见[部署指南](../../../developer-guide/zh/aw/deployment/lifecycle.md)。
