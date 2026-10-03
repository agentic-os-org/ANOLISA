# 统一配置边界

[English](configuration.md)

`aw-config` 负责期望配置解析；`aw-contracts` 继续负责能力 wire Schema、canonical
编码和记录约束。用户配置不修改已有 Schema，也不能让现有 Registry 将任意
Provider JSON 当作 canonical wire 元数据接受。

`AWConfiguration` 外层将版本、对象种类和身份与 `spec` 分开，运行状态不作为
期望输入接受。这借鉴声明式对象结构，不引入 Kubernetes API 或依赖。Provider
实例是命名对象，由有序事件步骤引用，同一个实现可以实例化多份私有配置。

Schema 是字段结构的权威定义。可复用离线校验器将有界 YAML 解析成 JSON，校验
结构后检查静态关系。`Configuration::as_value` 暴露已校验文档，不另外维护公开
Rust 字段模型，也不隐式填入默认值。Provider 私有对象保留有限小数和 Unicode
键，已有 wire 编码保持不变。Host 与服务根据完整原始配置字节计算版本，因此
格式变化也会选择不同的 revision。

## Provider 与原生步骤形态

结构化 Provider 使用 `protocol: aw-provider/v1alpha1`，步骤声明 `operation`
和 `effects`。准备阶段执行 `describe` 与 `validate_config`，再将请求效果与可信
Adapter 能力对照。当前运行时准入工具前 `observe`/`block`、工具后 `observe`。

现有原生 Hook 命令使用 `protocol: native-hook/v1alpha1`、空 `config` 对象，
步骤填写 `native: {}`，不填 `operation`、`effects`，混用两种形态会被拒绝。
原生命令不执行结构化握手；Host 保留字节输出与原生退出状态供 Adapter 转交，
并提供与结构化步骤一致的有界执行和审计元数据。原生输出不声明跨框架 AW 效果。

`spec.agents.<id>.qoder.sequential` 是 Qoder 专属可选设置，应用到生成的原生
Hook 组，属于 Adapter 配置，不属于 Provider 或共享执行策略。全部匹配原生组
如何调度仍由 Qoder 决定；其他组要求 sequential 时，false 不会强制并行。

静态校验拒绝未知公共字段、重复键及步骤 ID、悬空 Provider/guard 引用、非法工具
选择器、不匹配的事件效果或失败动作、不匹配的步骤协议，以及启用的结构化 `ask`
步骤。`security.violation` 保留为主动末尾检查设计，当前运行时拒绝 guard，
不保证全局 Hook 顺序。

控制效果及失败处置不能通过 `required: false` 丢弃。运行时准入和原生采用分开
验收。Qoder 启动器接通受支持的工具回调；QwenPaw、OpenClaw、Hermes Adapter、
sec-core 集成及安装包仍待后续交付。cosh、桌面客户端和 Herdr 可以独立使用公共
服务接口，配置字段和库依赖都不要求它们存在。

运行边界与回调关联见[完整字段参考](../../../../docs/developer-guide/zh/aw/configuration.md)
和[本地服务合同](local-service_zh.md)。顺序输入重写链和跨框架审批尚不属于当前
原生 Adapter 合同。
