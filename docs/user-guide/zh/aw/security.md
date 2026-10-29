# 使用 sec-core

[English](../../en/aw/security.md)

sec-core 在工具执行前检查命令。发现风险或必需检查失败时，AW 可以阻止该次调用。
发行模板使用 `native:qoder:Bash` 和 `native:openclaw:exec` 选择
Qoder 的 `Bash` 和 OpenClaw 的 `exec`。
扫描其他工具须另行配置选择器和输入映射。
这项能力通过原生 Hook 生效，不提供 OS 沙箱隔离。

执行前扫描和执行后观察使用同一工具范围。未列出的原生工具、MCP 调用和新增工具
保留原生行为，安全步骤记录为 `skipped`，不授予权限。原生拒绝继续有效。
已选中的工具遇到映射缺失、无效输入、超时或 Provider/后端故障时阻断执行。
增加 SDK 工具时请核对[模板名称映射](../../../developer-guide/zh/aw/providers/sec-core.md#配置-provider)：
Qoder CLI 的 `Bash` 与 Qoder SDK 的 `bash` 使用不同映射键。

## 准备程序与后台

按 [sec-core 指南](../agent-sec-core/index.md)安装后端 CLI、daemon，并管理其服务
生命周期。复用实际 socket，通常为 `/run/agent-sec-core/daemon.sock`。
AW Provider 不包含也不启动后端。root daemon 必须使用 root 控制的后端安装。

先[安装 AW CLI](installation.md)，再安装 Provider 桥接程序：

```bash
export AW_VERSION=0.1.0
export AW_SEC_BUNDLE="aw-provider-sec-core-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_SEC_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_SEC_BUNDLE"
export AW_TEMPLATE_DIR="$PWD/$AW_SEC_BUNDLE/templates"
```

这些命令使用普通用户自己的扩展 prefix。root 管理的扩展 prefix 需要显式
`--scope system`，写入时需要 root 权限。

## 生成配置并运行

沿用安装页的 `AW_HOME` 和 `AW_QODER`，将 CLI 和 socket 设为后端实际路径，
并保留解包后的模板：

```bash
export AW_SEC_CLI="$(command -v agent-sec-cli)"
export AW_SEC_SOCKET=/run/agent-sec-core/daemon.sock
export AW_PROVIDERS_DIR="$AW_EXTENSION_PREFIX/libexec/aw/providers"
aw config init --prefix "$AW_EXTENSION_PREFIX" \
  --config "$AW_HOME/security.yaml" --state-dir "$AW_HOME/security-state" \
  --template "$AW_TEMPLATE_DIR/sec-core.json" \
  --template-var "PROVIDERS_DIR=$AW_PROVIDERS_DIR" \
  --template-var "SEC_CORE_CLI=$AW_SEC_CLI" --template-var "SOCKET=$AW_SEC_SOCKET" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/security.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/security.yaml" --agent qoder
```

模板要求 `PROVIDERS_DIR`、`SEC_CORE_CLI` 和 `SOCKET`。原生包或 Makefile
安装时，`PROVIDERS_DIR` 取实际 Provider 根目录，例如 `/usr/libexec/aw/providers`
或 `/usr/local/libexec/aw/providers`。同时需要压缩时，安装 Tokenless 后端和
桥接程序，选择 `sec-core-tokenless.json`，并按 [Tokenless](tokenless.md)提供
另外三个后端变量。OpenClaw 使用[对应启动参数](configuration.md#openclaw)。

由 RPM 或其他管理者交付的 Provider 也可直接在 YAML 中通过绝对 `transport.argv`
引用。

## 验证检查

在原生 Agent 中分两次请求执行下列命令，保持原生权限确认开启。

```bash
printf AW_SECURITY_OK
git -c http.sslVerify=false --version
```

第一条应正常返回。第二条只打印 Git 版本，不访问网络或修改配置，但命令中关闭 TLS
校验的选项应被规则识别，Agent 应报告 AW 策略阻断。
模型自行拒绝或没有调用工具不能算作 AW 检查通过；检查仍未执行时，查看 Agent 的原生提示。

检查失败时，按[后台诊断说明](../../../developer-guide/zh/aw/providers/sec-core.md#外部后台与显式健康诊断)
确认 sec-core 后台与 socket。选中工具的输入或扫描错误按生成配置阻断，未选中的工具跳过扫描。

## 停止与扩展

退出 Agent 后执行 `aw stop --config "$AW_HOME/security.yaml"`。
后台服务继续按 sec-core 安装指南维护。
卸载 AW 保留外部后台配置和数据。

检查其他工具时，同时修改[步骤选择器](configuration.md#为-hook-步骤选择工具)和[Provider 输入映射](../../../developer-guide/zh/aw/providers/sec-core.md#配置-provider)。
