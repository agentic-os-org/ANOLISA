# Core Hook 策略准入

[English](../en/core-admission.md)

[设计索引](README.md) · [架构总览](architecture.md)

`aw_core::admission` 面向单份配置快照提供无副作用的 Hook 策略准入接口。该模块把
配置解析结果、原始字节 SHA-256 版本号、目标 Agent、可信 Adapter 能力声明与准入后
的 Hook 步骤绑定为不可变 Core 类型，同时保留 `aw_provider::admission` 作为唯一
规则实现。

`crates/aw-service/src/runtime.rs` 与 `crates/aw-host/src/lib.rs` 的生产 Hook
准备路径通过该接口执行，完整顺序为
`AdmissionEngine::preflight → Host::discover → AdmissionEngine::admit → Host::from_admitted`。
`Host::prepare` 兼容入口内部委托同一流程，原生 Hook 事件执行路径继续保持
`Adapter → Service → Host → aw-exec → Provider`。

## 准入阶段与类型

`AdmissionEngine` 在初始化时离线编译内置 `aw-config` Schema，并提供两个纯函数式
阶段转换：

| 阶段 | 入口 | 输入 | 输出 |
| --- | --- | --- | --- |
| 静态预检 | `AdmissionEngine::preflight` | 原始配置字节、目标名称、可信 `AdapterCapabilities` | `PolicyCandidate` |
| 证据准入 | `AdmissionEngine::admit` | 按值消费的 `PolicyCandidate`、经协议校验的 `BTreeMap<String, ProviderEvidence>` | `AdmittedPolicy` |

`preflight` 使用 `aw_config::Validator` 解析输入字节，按原始输入字节计算小写十六
进制 SHA-256 作为 `revision`，并复用 `aw_provider::admission::preflight` 完成目
标、事件、效果、工具选择器、传输方式与失败处置检查。语义相同但字节编码不同的配置
文档保持不同 `revision`，与 `aw-host::Host::revision` 算法一致。

`admit` 按值消费 `PolicyCandidate`，使已校验的 `aw_config::Configuration`、
`revision`、`target` 和 `AdapterCapabilities` 固定在同一配置快照上，并复用
`aw_provider::admission::admit` 核对 Provider 的 operation、events、effects 及
私有配置证据。使用 `aw-hook/v1alpha1` 或 `native-hook/v1alpha1` 的步骤不需要
`ProviderEvidence` 条目。

`AdmissionError` 封装 `aw_config::Error` 与 `aw_provider::Error`，保持
`Core::prepare` 和 `Core::execute` 使用的 `aw_core::Error` 不变。

## 不变量与运行时边界

`PolicyCandidate` 与 `AdmittedPolicy` 的内部字段全部私有，不实现 `Deserialize`
或公开结构体构造器，仅暴露只读访问器（`configuration`、`revision`、`target`、
`capabilities`、`providers`、`requirements` 与 `steps`）。调用方无法注入未校验的
`AdmittedStep`，也无法在预检、Provider 发现与 Host 构建之间替换配置快照或
Provider 证据。`Debug` 输出省略配置文档与 Provider 证据内容，避免泄露 Provider
私有配置值。

`AdmittedPolicy` 仅表示所供快照通过能力准入：

- `aw-host` 负责有界 `describe`/`validate_config` 交互、运行时文件读锁、
  Provider 进程执行与清理。
- `aw-service` 负责活跃绑定、事件租约、步骤数与调用预算、策略激活门以及
  `aw_core::journal::FileJournal` 持久审计。
- `aw-adapter` 负责原生 Hook 注册、回调核验以及目标 Agent 对阻断或结果替换的实际
  采用。
