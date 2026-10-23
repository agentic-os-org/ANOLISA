# AW 设计索引

[English](../en/README.md)

先读[架构总览](../../../../../docs/developer-guide/zh/aw/architecture.md)，了解 crate 依赖和代码入口。
当前 Hook 调用与审计经过以下路径。

源码归属、内部接口与后续发布工作的范围见[架构边界](architecture-boundaries.md)。

`aw-cli` 提供统一的 `aw` 入口；独立 `aw-package` 可执行文件已删除，Rust 库保留。
安装和离线编辑直接调用 `aw-package`
库，运行时命令调用 `aw-service`。此次拆分不改变 Hook 路径或协议版本。

```mermaid
flowchart LR
    Agent[原生 Agent] --> Adapter[aw-adapter]
    Adapter --> Service[aw-service]
    Service --> Host[aw-host]
    Host --> Exec[aw-exec]
    Exec --> Provider[Provider 进程]
    Provider --> Backend[Tokenless 或 sec-core CLI]
    Service --> Journal[aw-core FileJournal]
```

结果沿原路返回，由 Adapter 在原生接口中采用。
`aw-core::Core` 的能力计划执行是另一套 API，见后文。

## 沿一次请求阅读

| 阅读顺序 | 负责的部分 |
| --- | --- |
| [配置](configuration/configuration.md) | `aw-config` 在运行前校验期望配置 |
| [策略更新边界](runtime/policy-reload.md) | 离线比较和显式空闲激活；外部接入仍在规划中 |
| [Adapter 分发](adapters/adapter-dispatch.md) | aw-adapter 准备原生注册、转换事件并采用效果 |
| [本地服务](runtime/local-service.md) | `aw-service` 管理绑定、事件和审计，把步骤交给 Host |
| [Provider Host](runtime/provider-host.md) | `aw-host` 准备 Provider，执行已准入的步骤 |
| [Provider 协议](providers/provider-protocol.md) | `aw-provider` 校验请求、响应和能力 |
| [有界执行](runtime/bounded-execution.md) | `aw-exec` 管理命令传输和进程清理 |

## 按接入对象阅读

| 接入对象 | 文档 |
| --- | --- |
| 原生 Agent 回调 | [AgentScope](adapters/agentscope-adapter.md)、[Claude SDK](adapters/claude-sdk-adapter.md)、[Hermes](adapters/hermes-adapter.md)、[OpenClaw](adapters/openclaw-adapter.md)、[Pi](adapters/pi-adapter.md)、[QwenPaw](adapters/qwenpaw-adapter.md)；Qoder 见 [Adapter 分发](adapters/adapter-dispatch.md)与[本地服务](runtime/local-service.md) |
| Provider 独立开发与收录 | [协作目录](../../../../../providers/README_zh.md)与[开发分发指南](../../../../../docs/developer-guide/zh/aw/providers.md) |
| 安全 Provider 与后端就绪检查 | [sec-core Provider](providers/sec-core/sec-core-provider.md)，再读 [sec-core 健康检查](providers/sec-core/sec-core-health.md) |
| Tokenless Provider | [接入参考](../../../../../docs/developer-guide/zh/aw/tokenless.md) |
| 打包和配置编辑 | [构建指南](../../../../../docs/developer-guide/zh/aw/building.md)，再读[配置编辑](configuration/configuration-editing.md) |
| 本地包数据库、预览、卸载和中断恢复 | [本地包管理](packaging/package-management.md) |
| 只读查看状态 | [Provider 视图](providers/provider-views.md)，独立于 Hook 准入 |

## 审计、证据与能力计划

| 主题 | 与 Hook 路径的关系 |
| --- | --- |
| [Core 执行与存储](core-execution.md) | 服务复用 FileJournal；计划执行器由嵌入方另外接入 Host/Journal 接口 |
| [AgentSight 只读连接器](integrations/agentsight/agentsight-readonly.md) | 供服务读取可选的外部观测数据 |
| [证据与上下文](runtime/evidence-context.md) | 原生身份关联、观测和显式上下文冻结；Schema 由 `aw-contracts` 管理 |
| [AgentSight 集成规划](integrations/agentsight/agentsight-integration.md) | 说明观测与后续覆盖率、增量导出、语义评估的关系；这些后续阶段尚未提供 |

安装和日常使用见[用户指南](../../../../../docs/user-guide/zh/aw/index.md)。
