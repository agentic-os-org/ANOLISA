# 使用 Tokenless

[English](../../en/aw/tokenless.md)

Tokenless 压缩工具文本，减少后续模型上下文，也可以建议输出更精简的命令。
安全检查和原生权限继续生效。

## 安装并启用

按 [Tokenless 指南](../tokenless/index.md)安装后端 CLI 和兼容的 RTK。
此 Provider 要求带补丁的 RTK 0.49.0，应使用后端 owner 的对应发行件。
AW Provider bundle 只包含桥接程序、示例与模板，不包含 Tokenless CLI 或 RTK。

按[安装页](installation.md)准备 AW CLI、`AW_HOME` 和 `AW_QODER`，再将桥接程序
安装到独立的扩展 prefix：

```bash
export AW_VERSION=0.1.0
export AW_TOKENLESS_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_TOKENLESS_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_TOKENLESS_BUNDLE"
export AW_TEMPLATE_DIR="$PWD/$AW_TOKENLESS_BUNDLE/templates"
```

这些示例使用 user scope。root 管理的扩展写入需要显式 `--scope system` 与 root
权限。不要在同一会话叠加功能相同的 Tokenless 原生插件。

选择实际后端程序，计算所选 RTK 的校验和。若有符号链接，应先解析，使
`RTK_BINARY` 指向普通可执行文件本身：

```bash
export AW_TOKENLESS_CLI="$(command -v tokenless)"
export AW_PROVIDERS_DIR="$AW_EXTENSION_PREFIX/libexec/aw/providers"
export AW_RTK_BINARY="$(readlink -f "$(command -v rtk)")"
export AW_RTK_SHA256="$(sha256sum "$AW_RTK_BINARY" | awk '{print $1}')"
"$AW_RTK_BINARY" --version
aw config init --prefix "$AW_EXTENSION_PREFIX" \
  --config "$AW_HOME/tokenless.yaml" --state-dir "$AW_HOME/tokenless-state" \
  --template "$AW_TEMPLATE_DIR/tokenless.json" \
  --template-var "PROVIDERS_DIR=$AW_PROVIDERS_DIR" \
  --template-var "TOKENLESS_CLI=$AW_TOKENLESS_CLI" \
  --template-var "RTK_BINARY=$AW_RTK_BINARY" --template-var "RTK_SHA256=$AW_RTK_SHA256" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/tokenless.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/tokenless.yaml" --agent qoder
```

版本命令须输出 `rtk 0.49.0`。模板变量是 `PROVIDERS_DIR`、`TOKENLESS_CLI`、
`RTK_BINARY` 和 `RTK_SHA256`。原生包或 Makefile 安装时，`PROVIDERS_DIR` 取实际
Provider 根目录。同时需要安全检查时，安装并启动 [sec-core](security.md)，选择
`sec-core-tokenless.json`，再提供该后端的 `SEC_CORE_CLI` 和 `SOCKET`。
OpenClaw 使用[对应 Agent 参数](configuration.md#openclaw)，其他运行时见
[集成参考](../../../developer-guide/zh/aw/providers/tokenless.md)。

由 RPM 或其他管理者安装的 Provider 也可直接在 YAML 中通过绝对 `transport.argv`
配置。

## 运行时会发生什么

- 执行前可能收到命令重试建议。AW 阻断原调用，Agent 重新发起建议的调用后，再次经过安全和原生权限检查。
- 执行后会处理受支持的成功文本结果。失败结果、图片和不支持的内容保持原样。
- 压缩失败、超时或没有收益时保留原结果。安全检查的拒绝不会被压缩步骤覆盖。

生成的配置使用显式选择的 RTK 路径和校验和。文件缺失或校验失败时会报告错误，
不会改用其他用户安装的版本。
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
[查询接口参考](../../../developer-guide/zh/aw/providers/tokenless.md#panel-只读查询)。

Tokenless 统计使用 AW 配置中的 Agent 名称标记来源，也就是 `aw run --agent` 后指定的名称。
例如，`qoder-work` 和 `qoder-test` 即使都使用 Qoder，也会分别标记来源。
不同配置使用相同名称时，统计仍可能汇总到一起。

统计反映最近记录中的候选压缩收益，可能包含其他接入产生的数据。结合 Agent 的实际结果确认压缩内容是否进入模型上下文；当前会话的计费节省需按模型服务商的计费用量核算。

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
