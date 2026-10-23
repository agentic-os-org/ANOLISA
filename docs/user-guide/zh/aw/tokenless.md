# 使用 Tokenless

[English](../../en/aw/tokenless.md)

Tokenless 压缩工具返回给模型的文本，减少后续上下文占用。
它也可以在执行前建议输出更精简的命令。安全检查和 Agent 原生权限继续生效。

## 安装并启用

需要 AW core 和同一次构建的 `aw-provider-tokenless`，也可以直接安装 all-in-one。
按[安装页](installation.md)准备 `AW_PREFIX`、`AW_HOME` 和 `AW_QODER`。
分包用户在解压后的 artifact 目录增加 Tokenless。

```bash
export AW_VERSION=0.1.0-preview.1
export AW_TOKENLESS_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_TOKENLESS_BUNDLE.tar.gz"
sudo "./$AW_TOKENLESS_BUNDLE/aw" install --scope system --bundle "$PWD/$AW_TOKENLESS_BUNDLE" --prefix "$AW_PREFIX"
```

这里沿用安装页的 system scope。若 core 安装在普通用户自己的前缀中，扩展包也应
去掉 `sudo` 并使用 `--scope user`。core、每个扩展包和卸载都须使用同一个 scope；
`sudo` 不会自动选择 system scope。

已安装 all-in-one 时跳过这一步。包内包含 Tokenless CLI 和所需 RTK，
无需另外安装 RTK、TOON、Python wheel 或 Tokenless 原生插件。
不要在同一会话叠加功能相同的 Tokenless 插件，以免重复处理。

```bash
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/tokenless.yaml" --state-dir "$AW_HOME/tokenless-state" \
  --provider tokenless --qoder "$AW_QODER"
aw validate --config "$AW_HOME/tokenless.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/tokenless.yaml" --agent qoder
```

OpenClaw 使用[对应的 Agent 参数](configuration.md#openclaw)。需要同时检查风险时，
安装并启动 [sec-core](security.md)，把 Provider 参数改为
`--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock`。将 socket 路径改成实际后端使用的路径。
其他框架与 SDK 的接线见[集成参考](../../../developer-guide/zh/aw/tokenless.md)。

## 运行时会发生什么

- 执行前可能收到命令重试建议。AW 阻断原调用，Agent 重新发起建议的调用后，再次经过安全和原生权限检查。
- 执行后会处理受支持的成功文本结果。失败结果、图片和不支持的内容保持原样。
- 压缩失败、超时或没有收益时保留原结果。安全检查的拒绝不会被压缩步骤覆盖。

包生成的配置使用自带 RTK。文件缺失或校验失败时会报告错误，不改用用户自行安装的版本。
当前 AW 接入不提供压缩结果的恢复入口。

## 查看效果

让 Agent 在工作区执行一条确实会返回文本的工具调用，例如读取文件或查看 Git 状态。
在另一个设置了相同 `AW_PREFIX`、`PATH` 和 `AW_HOME` 的终端查看摘要。

```bash
aw inspect --config "$AW_HOME/tokenless.yaml" --json
```

检查 Tokenless 卡片和最近的步骤记录。`no_data` 表示尚无统计记录，`unsupported`
表示当前配置没有可查询的指标；出现错误时先检查 Provider 和数据目录。
新生成的 Tokenless 配置已包含查询设置，旧配置的补充方法见
[查询接口参考](../../../developer-guide/zh/aw/tokenless.md#panel-只读查询)。

Tokenless 统计使用 AW 配置中的 Agent 名称标记来源，也就是 `aw run --agent` 后指定的名称。
例如，`qoder-work` 和 `qoder-test` 即使都使用 Qoder，也会分别标记来源。
不同配置使用相同名称时，统计仍可能汇总到一起。

统计反映最近记录中的候选压缩收益，可能包含其他接入产生的数据。
它不等于当前会话的计费节省；是否进入模型上下文，还需结合 Agent 的实际结果确认。

## 数据与隐私

AW 审计只保存元数据，Tokenless 使用独立的数据目录。
Tokenless 默认统计可能保存完整的压缩前后工具文本；本地 SLS 日志保存指标与归属信息。
已有遥测采集器可能继续收集这些日志。

Agent 名称可能出现在统计记录和日志中。命名时避免包含敏感信息，
分享统计或日志前请检查其中的名称和内容。

不需要录制时，在启动 AW 之前设置以下变量。若服务已经运行，先退出 Agent 并停止旧服务。

```bash
export TOKENLESS_STATS_ENABLED=0
export TOKENLESS_SLS_ENABLED=0
```

关闭录制不会删除历史数据，也不影响压缩功能。更多设置见
[Tokenless 配置与隐私](../tokenless/configuration-and-privacy.md)。

## 停用

退出 Agent 后停止服务。

```bash
aw stop --config "$AW_HOME/tokenless.yaml"
```

删除配置中的 Tokenless 步骤及不再使用的 Provider，校验后重新启动。
数据目录按需要保留；只停用功能不会删除统计记录。
