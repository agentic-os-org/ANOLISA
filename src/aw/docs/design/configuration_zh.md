# 统一配置边界

[English](configuration.md)

`aw-config` 负责期望配置解析；`aw-contracts` 继续负责能力 wire Schema、canonical
编码和记录约束。用户配置不修改已有 Schema，也不能让现有 Registry 将任意
Provider JSON 当作 canonical wire 元数据接受。

`AWConfiguration` 外层将版本、对象种类和身份与 `spec` 分开，运行状态不作为
期望输入接受。这借鉴声明式对象结构，不引入 Kubernetes API 或依赖。Provider
实例是命名对象，由有序事件步骤引用，同一个实现可以实例化多份私有配置。

配置 Schema 是字段结构的权威定义。可复用离线校验器将有界 YAML 解析成 JSON，
校验结构后检查静态关系。`Configuration::as_value` 暴露已校验文档，不另外维护
一份公开 Rust 字段模型，也不隐式填入默认值。Provider 私有对象保留有限小数和
Unicode 键；已有 wire 编码保持不变。本增量不定义配置 revision 或其摘要编码。

静态校验拒绝未知公共字段、重复键及步骤 ID、悬空 Provider/guard 引用、非法工具
选择器、与事件不匹配的效果/失败动作，以及启用的 `ask` 步骤。
`security.violation` 沿用 POC 的主动末尾检查事件名，只能由启用的 tool-before
guard 激活；这不保证全局 Hook 顺序。

后续协议增量定义 Provider 的 `describe`、`validate_config`、`invoke`，以及
私有 Schema 校验和效果准入。配置中的操作名不证明实现存在；控制效果及失败
处置不能通过 `required: false` 丢弃。运行时还需区分可选观察不支持与已安装、
已连接、已触发、已采用，执行共享预算并验证实际消费。

独立服务、四框架适配、sec-core 集成、安装管理和审计在这些合同之后实现。
cosh、Herdr 保持为后续公共服务客户端；这里的字段及库依赖均不要求它们存在。
