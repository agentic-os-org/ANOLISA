# AW 快速入门

[English](../../en/aw/index.md)

AW 让你在已有 Agent 中使用工具安全检查、输出压缩和自定义规则。
本页带你通过 AW 启动 Qoder，验证一次工具调用，再停止本地服务。
Agent 的界面、模型账号和权限设置继续由原框架管理。

## 开始前准备

- 使用 Linux。CI 预编译包需要 Linux x86_64 和 glibc 2.39 或更高版本，例如 Ubuntu 24.04。
- 按 Qoder 自身的说明安装 CLI 并登录。已验证基线为 1.1.64；`qodercli` 必须在 `PATH` 中。
- 以普通用户执行以下步骤。其他 Agent 及其前提条件见[配置与启动](configuration.md)。

## 1. 安装 AW

从目标提交对应的成功 AW Integration 运行中下载 `aw-core` 压缩包和匹配的
`SHA256SUMS`，按[安装指南](installation.md#获取安装包)校验并解压。
进入解压后的 `aw-core` 目录，安装预编译 CLI：

```bash
export AW_PREFIX="$HOME/.local"
make install PREFIX="$AW_PREFIX"
export PATH="$AW_PREFIX/bin:$PATH"
aw --version
```

`aw --version` 应显示安装的版本号。当前 AW 使用独立安装包，尚未提供
`anolisa install aw` 或 RPM 安装入口。若已安装 AW，将 `AW_PREFIX`
设为其安装前缀后继续。

## 2. 生成配置

将配置和运行数据放在安装文件之外的私有目录中。
以下配置使用空策略启动 Qoder，无需安装 Provider 或后端组件。

```bash
export AW_HOME="$HOME/aw"
export AW_QODER="$(command -v qodercli)"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
qodercli --version
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
aw config validate --config "$AW_HOME/aw.yaml"
```

`AW_QODER` 必须是可执行文件的绝对路径。`config init` 创建 `aw.yaml`，
拒绝覆盖已有文件。校验通过后应显示 `configuration valid`。

<a id="一次运行"></a>

## 3. 启动并验证工具调用

```bash
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

应先看到 `AW instance ...; service ...` 消息，再进入 Qoder 界面。
让 Qoder 使用工具读取当前目录，并按原生提示授权。
确认工具实际运行且结果正常显示。`qoder` 是生成的 `aw.yaml` 中的目标名称。

首次运行用于验证启动流程。空策略不会执行安全检查或输出压缩，
完成首次运行后，可按下文[增加能力](#增加能力)启用对应 Provider。

## 4. 使用后停止

退出 Qoder。AW 服务仍可被后续会话复用，查看状态后显式停止它：

```bash
aw service status --config "$AW_HOME/aw.yaml"
aw service stop --config "$AW_HOME/aw.yaml"
```

停止服务前，先退出共享此配置的所有 Agent。停止后保留配置和审计数据。
启动报错、升级和清理方式见[运行维护](operations.md)。

<a id="选择需要的能力"></a>

## 增加能力

安装对应 Provider 和后端组件，再配置策略，并通过实际工具调用验证效果：

| 目标 | 指南 |
| --- | --- |
| 减少工具结果占用的上下文 | [使用 Tokenless](tokenless.md) |
| 在工具执行前检查风险 | [使用 sec-core](security.md) |
| 关联 AgentSight 运行记录 | [接入 AgentSight](observability.md) |

原生权限判断始终保留；AW 当前的 Hook 检查不提供 OS 沙箱隔离。

<a id="从这里开始"></a>

## 后续使用与参考

| 要做什么 | 阅读哪一页 |
| --- | --- |
| 选择其他 Agent、增加规则或修改配置 | [配置与启动](configuration.md) |
| 查找子命令、参数、默认值和示例 | [CLI 命令参考](cli-reference.md) |
| 查看状态、停止服务、升级或排错 | [运行维护](operations.md) |
| 对比 Hook、MCP 与策略更新的支持范围 | [能力支持矩阵](capabilities.md) |
| 编写 Provider 或接入 SDK 应用 | [开发指南](../../../developer-guide/zh/aw/index.md) |
