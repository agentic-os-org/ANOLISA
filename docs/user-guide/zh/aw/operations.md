# 日常维护

[English](../../en/aw/operations.md)

## 查看状态与停止

```bash
aw service status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw service stop --config "$HOME/aw/aw.yaml"
```

`status` 查看实例，`inspect` 查看 Provider 摘要。先退出共享这份配置的所有 Agent，再停止 AW，并用 `status` 确认服务已退出。sec-core 和 AgentSight 服务由各自管理者维护。

Pi/Claude SDK、Qoder SDK 与 QwenPaw 常驻任务在 status 的
`instances[].scope_validity` 中报告状态。`native_alive` 表示 worker 存活，
`covered_bridge` 表示受监测的 AW 通路。桥接为 `failed` 时，覆盖内调用被拒绝，
独立健康任务继续运行。先排空并关闭失败任务，再创建新的受管任务；原生清理无法
确认时需重启其 owner。调用不会自动重放，AW 范围外能力保留原生行为。

`inspect --json` 的 `service.resident_scopes` 汇总 `native_alive`、`ready`、
`failed`、`pending` 和 `unmonitored` 数量。启动 ready 只能说明启动时已就绪。
空闲检查在上次检查结束两秒后运行；十五秒未成功检查时，服务拒绝该 scope 的执行。
汇总为 `null` 表示所连服务未提供这些数据。

## 检查安装

检查本地 Provider 扩展时，沿用安装时的 `AW_EXTENSION_PREFIX`。

```bash
aw list --installed --prefix "$AW_EXTENSION_PREFIX"
aw doctor --prefix "$AW_EXTENSION_PREFIX"
```

`list` 列出 Provider 扩展记录；`doctor` 检查文件是否缺失、被修改或权限异常。通过时返回 0，发现问题时返回 1 并列出路径。AW CLI 与后端安装仍由原安装者或原生包管理器维护。

## 升级与卸载

1. 退出 Agent，用旧版 AW 和原配置停止服务。
2. 备份配置与状态目录，通过原安装者升级 AW CLI 和后端，将新 Provider 包[安装](installation.md)到新的扩展目录。
3. 用新路径生成配置，使用新的状态目录，验证实际工具调用。
4. 保留旧安装和备份；回退时停止新服务，切回旧路径与配置。

含 `.aw-packages` 的旧目录可以继续登记在同一个用户数据库中，同时向新目录安装。
AW 保留旧记录和文件。检查新安装时使用 `--prefix "$AW_EXTENSION_PREFIX"`；
显式操作旧目录仍会因其元数据被拒绝，管理旧安装需使用旧版 CLI。
全局 `doctor` 仍会报告旧元数据不受支持。

以下命令移除本地管理的 Provider 扩展：

```bash
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --yes
```

root 管理的扩展需要 `sudo` 和显式 `--scope system`。卸载保留外部配置、审计和 Agent 数据，不移除 AW CLI、后端或其服务。包文件仍在使用时，先停止持有文件的 AW 进程再重试。
如果 `doctor` 或卸载报告文件被修改或缺失，先检查受影响路径，再使用 `--keep-changed` 预览移除：

```bash
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --keep-changed --dry-run
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --keep-changed --yes
```

这会移除选中包的登记和未变化文件。内容、权限模式、文件类型发生变化的路径及符号链接保持原样，缺失路径继续保持缺失。预览中的 `KEEP PATH` 和成功移除后的 `KEPT PATH` 列出这些路径。可在 `uninstall` 后追加包 ID 选择包；省略 ID 时选择该 prefix 内的全部包。剩余依赖、读取权限错误和不安全的安装目录仍会阻止移除。

空登记和锁继续保留。整个 prefix 已缺失时，AW 会重建这些元数据文件并清空包记录。保留文件转为用户管理；重新安装前先移动或删除冲突路径，也可选择新的 prefix。

出现 `needs_recovery` 时，停止包写入操作，按[包恢复步骤](../../../developer-guide/zh/aw/reference/packages.md)处理。

## 数据与排错

`state_dir/journal` 保存 AW 调用元数据。Tokenless 的正文留存另见[数据与隐私](tokenless.md#数据与隐私)。

| 现象 | 处理 |
| --- | --- |
| 校验通过，启动失败 | 检查错误所指的 Agent 版本、程序路径和 Provider 配置 |
| Claude Python SDK 启动预检报 `command execution deadline exceeded` | 在配置指定的解释器环境中检查 Python 启动与 APM 初始化耗时 |
| 配置修改未生效 | 退出 Agent，用原配置停止旧服务，再启动新配置 |
| 配置或目录权限错误 | 使用当前用户拥有的私有目录，配置编辑避免符号链接 |
| 服务协议不匹配 | 用旧二进制停止服务，再统一使用新 AW |
| 强制退出后留下 socket | 确认原服务已退出，再移除它遗留的 `aw.sock`，保留 journal |
| `native_event_limit` | 重启 AW 管理的 Agent 实例；仅新建原生会话不会重置计数 |
| 压缩统计为空 | 确认发生了受支持的工具调用，并检查录制设置 |

AW 内部的 Python 探测脚本通过私有管道返回版本元数据和 JSON。AW 单独收集标准输出中的启动提示，结果超出字节上限时终止探测。
探测运行时时，AW 设置 `OTEL_SDK_DISABLED=true`，并从 `PYTHONPATH` 中移除以
`opentelemetry/instrumentation/auto_instrumentation` 结尾的目录。Hermes 使用应用环境解析
dotenv、插件选择和 Hook 回调时限。业务进程保留原有的插桩设置和模块搜索路径。

QwenPaw 通过入口指定的解释器，在一次探测中识别生成的入口并读取静态版本。转发包装器可以打印启动提示，也可以报告自己的版本。
对于自定义启动器或无法静态读取版本的生成入口，AW 在应用环境中执行原命令的 `--version`，
其标准输出应只包含 `QwenPaw, version <版本>`。入口检查和必要的回退共享 5 秒时限。

使用 Claude Python SDK 或 AgentScope 时，`aw run` 和 `aw instance prepare` 为版本探测预留
15 秒，包含解释器启动时间。Claude 内置 CLI 的 `--version` 命令单独限时 4 秒。
启用 `security.violation` 后，Claude 的指纹探测另有 15 秒时限。

更多命令可用 `aw --help`、`aw commands` 查询。补全脚本由 `aw completion bash`（或 `zsh`、`fish`）输出。
自动化报告见[CLI 参考](../../../developer-guide/zh/aw/reference/cli.md)，容器持久化和容量见[部署指南](../../../developer-guide/zh/aw/deployment/lifecycle.md)。
