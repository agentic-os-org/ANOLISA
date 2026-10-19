# AW 架构

[English](../../en/aw/architecture.md)

AW 在原生 Agent 的工具调用前后执行策略。Adapter 接收原生回调，本地服务管理
绑定和事件，Host 调用 Provider，最后由 Adapter 将结果交还 Agent。
模型调用、工具调度和权限审批仍由 Agent 负责。

## 使用流程

![AW 用户流程与组件架构](../../../images/aw/architecture_zh.png)

图中按用户操作组织各包，箭头表示运行时协作；绿色区域是 Provider 协作目录。
具体实现可独立开发和分发，接入方式见 [Provider 指南](providers.md)。

## Crate 关系

AW 框架包含 `crates/` 下的 8 个 crate。具体 Provider 在 `providers/` 中单独维护。

| Crate | 职责 | 直接依赖的 AW crate |
| --- | --- | --- |
| `aw-service` | `aw` CLI、原生 Adapter、本地服务和客户端 | config、contracts、core、exec、host、provider |
| `aw-host` | 准备 Provider，执行已准入的 Hook 步骤 | config、exec、provider |
| `aw-provider` | 校验 Provider 消息和能力准入 | config |
| `aw-exec` | 管理进程期限、输出上限和退出清理 | 无 |
| `aw-config` | 解析配置，校验字段和静态引用 | 无 |
| `aw-core` | 能力计划执行器、Host/Journal 接口和 FileJournal | contracts |
| `aw-contracts` | Schema、规范编码和记录间一致性校验 | 无 |
| `aw-package` | `aw-build` 构建打包，`aw-package` 安装和生成配置 | config、exec |

依赖列省略 `aw-` 前缀。`aw-package` 的测试还依赖 `aw-service`。

### 独立 Provider

| Cargo 包与二进制 | 目录 | 职责 | 依赖的框架 crate |
| --- | --- | --- | --- |
| `aw-provider-sec-core` | `providers/sec-core/` | 桥接 sec-core CLI，提供安全检查 | exec、provider |
| `aw-provider-tokenless` | `providers/tokenless/` | 桥接 Tokenless CLI，提供输出压缩 | exec、provider |

这两个实现目前与框架共用 Cargo workspace，便于统一构建和测试，但通过独立进程
接入，不计入 8 个框架 crate。Tokenless Provider 的测试还依赖 sec-core Provider。
其他团队可独立开发、分发 Provider，见[协作指南](providers.md)。

## 一次 Hook 请求

1. `aw run` 中的 Adapter 检查原生入口，登记回调并启动 Agent。
2. Agent 触发回调，Adapter 把原生事件转成服务请求。
3. `aw-service` 管理绑定、事件期限和审计，交给 `aw-host` 执行步骤。
4. Host 用 `aw-provider` 校验协议，通过 `aw-exec` 启动配置中的 Provider 命令。
   内置桥接程序再调用 Tokenless 或 sec-core CLI；双方通过进程协议连接。
5. 调用结果沿原路返回，Adapter 按原生接口采用阻断或结果替换。Provider 返回成功
   只表示调用成功，是否实际采用效果须由 Adapter 确认。

原生 Hook 脚本也通过 `aw-exec` 执行，保留原始输出和退出码，不经过 Provider 握手。
AgentSight 是独立的可选观测来源；`aw inspect` 的只读视图不参与 Hook 准入。

### `aw-core` 在哪里

当前 Hook 路径是 `aw-service → aw-host → aw-exec`。服务复用
`aw-core::journal::FileJournal` 保存审计，但不调用 `Core::prepare/execute`。

`aw-core::Core` 是单独的能力计划执行接口。使用它的嵌入方要提供
`ProviderHost`、`Clock` 和 `Journal`；它依赖 `aw-contracts` 校验计划和执行记录。
`aw-host::Host` 与这个 `ProviderHost` 接口各有自己的契约，不能当成同一个调用层。

## 目录与代码入口

`src/aw/Cargo.toml` 只定义 workspace。8 个通用框架 crate 位于 `crates/`，
2 个具体 Provider 位于 `providers/`，各自保存源码和测试。原来根目录 `src/`
中的 contracts 代码已迁入 `crates/aw-contracts/`。

`providers/` 是协作入口，允许收录第一方与社区实现；Provider 也可以独立发布，
通过进程协议接入。[Provider 开发与分发](providers.md)说明目录约定和打包范围。

| 路径，相对于 `src/aw/` | 内容 |
| --- | --- |
| `crates/aw-service/src/main.rs`、`crates/aw-service/src/cli/` | CLI、启动流程和原生 Adapter |
| `crates/aw-service/src/server.rs`、`crates/aw-service/src/runtime/` | 本地服务与事件执行 |
| `crates/aw-host/src/lib.rs`、`crates/aw-host/src/event.rs` | Provider 准备与调用 |
| `crates/aw-core/src/journal.rs` | 持久审计实现 |
| `crates/aw-contracts/src/registry.rs` | 编译进程序的契约 Schema 清单 |
| `providers/sec-core/`、`providers/tokenless/` | 具体 Provider 实现、配置示例和测试 |
| `adapters/` | 随包分发的 Python/JavaScript 原生桥接资源 |
| `scripts/`、`tests/` | workspace 检查、跨语言向量检查和原生验收 |
| `docs/design/` | 协议约定、边界与设计理由 |

详细设计从[设计索引](../../../../src/aw/docs/design/README_zh.md)按调用顺序阅读。
开发环境和检查命令见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。
