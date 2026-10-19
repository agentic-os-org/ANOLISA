# AW

[English](README.md)

AW 让不同 Agent 共用安全检查、工具输出压缩和自定义规则。
它通过一份配置启动原生 Agent，并在工具调用前后运行所选 Provider。
Agent 继续管理自己的模型、界面和权限；AW 保存本地执行记录，便于检查运行情况。
当前为 Linux Preview。

## 安装与使用

AW 使用 Preview 包分发，尚未接入 `anolisa install` 或 RPM。
从[安装指南](../../docs/user-guide/zh/user-entrypoint/aw/installation.md)下载并安装 core
或 all-in-one，用 `aw-package configure` 生成配置后运行。

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

退出 Agent 后，本地服务仍可查询，使用结束后显式停止。

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

core 可独立使用。Tokenless 包自带 RTK 和 TOON 编码，安全检查需要启动 sec-core
后端；这些能力都要通过配置启用。AW 不安装 Agent，也不配置模型凭据。

## 文档导航

| 任务 | 文档 |
| --- | --- |
| 选择能力和开始使用 | [AW 用户指南](../../docs/user-guide/zh/user-entrypoint/aw/README.md) |
| 安装包、生成配置和首次运行 | [安装 AW](../../docs/user-guide/zh/user-entrypoint/aw/installation.md) |
| 选择 Agent、启用 Provider 和修改配置 | [配置 AW](../../docs/user-guide/zh/user-entrypoint/aw/configuration.md) |
| 压缩工具输出 | [Tokenless](../../docs/user-guide/zh/user-entrypoint/aw/tokenless.md) |
| 启用命令安全检查 | [安全检查](../../docs/user-guide/zh/user-entrypoint/aw/security.md) |
| 查看 Agent 运行记录 | [接入 AgentSight](../../docs/user-guide/zh/user-entrypoint/aw/observability.md) |
| 查询状态、升级、卸载和排错 | [运行维护](../../docs/user-guide/zh/user-entrypoint/aw/operations.md) |
| 客户本地编译和打包 | [从源码构建 AW](../../docs/developer-guide/zh/aw/building.md) |
| 编写规则、集成 Provider 或 SDK | [AW 开发指南](../../docs/developer-guide/zh/aw/README.md) |

用户手册集中在 `docs/user-guide/{en,zh}/user-entrypoint/aw/`，接入参考在
`docs/developer-guide/{en,zh}/aw/`。组件协议设计保留在本目录的 `docs/design/`。

## Hook 支持

下表由[回归矩阵](tests/regression-matrix.json)生成，表示运行时声明并由 CI 契约测试
覆盖的能力。支持版本、工具类型和文本替换范围见
[原生 Agent 接入参考](../../docs/developer-guide/zh/aw/native-agents.md)。

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.after` / `observe` | `tool.after` / `replace_result` |
| --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ |
| `openclaw` | ✓ | ✓ | ✓ | ✓ |
| `pi` | ✓ | ✓ | ✓ | ✓ |
| `qoder` | ✓ | ✓ | ✓ | ✓ |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->

Tokenless 重试建议会阻止原调用，Agent 需发起新调用并重新经过安全和原生权限检查。
结果替换仅适用于受支持的文本位置；AW 不提供 OS 沙箱或跨 Agent 审批。
真实 SDK 和模型会话的验收范围见[贡献指南](CONTRIBUTING_zh.md)。

## 代码架构

![AW 用户流程与组件架构](../../docs/images/aw/architecture_zh.png)

根目录是纯 workspace。`crates/` 保存 9 个框架 crate，仓库根 `providers/` 收录可独立协作的
具体实现。Provider 也可以在外部仓库开发、单独分发，见[协作入口](../../providers/README_zh.md)。
Hook 请求由 `aw-service` 交给 `aw-host`，通过 `aw-exec` 调用 Provider；
`aw-core` 提供独立的计划执行接口，服务只复用它的 Journal。

[架构总览](../../docs/developer-guide/zh/aw/architecture.md)说明各包职责与运行顺序，
[设计索引](docs/design/README_zh.md)按调用流程组织详细协议。

## 开发与验证

从仓库根目录运行 AW 的 CI 检查。

```bash
python3 src/aw/scripts/check.py
```

- [开发环境和测试](CONTRIBUTING_zh.md)
