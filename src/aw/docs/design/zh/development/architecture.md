# AW 框架与 Provider 开发规则

[English](../../en/development/architecture.md)

这些规则由 [AGENTS.md](../../../../AGENTS.md) 按任务索引；修改前必须阅读适用章节。

具体 Provider 放在仓库根目录 `providers/<provider>/` 独立协作，`src/aw/crates/` 保留可复用框架。
后端安全、压缩等集成归 Provider；`aw-provider` 是共享协议库。

`providers/` 接受第一方和社区实现，各自维护源码、schema、示例和测试。Provider 也可在
独立仓库使用其他语言开发，通过 `aw-provider/v1alpha1` stdio 与配置的 `transport.argv`
接入，无需加入本仓库。遵循 [Provider 贡献指南](../../../../../../providers/README.md)。

每个仓库内 Rust Provider 拥有自己的 workspace、lockfile 和 toolchain。AW workspace
只包含框架包；门禁组合各自测试清单，不把 Provider 加为 AW 成员或依赖。Provider
维护者负责依赖、测试和发布，OS 发行方选择版本、验证组合并交付安装升级。独立构建应
使用固定版本的公共 AW 库；第三方只需公开进程协议和 YAML，无需 Rust SDK 或 OS 检出。
仓库内实现目前仍使用本地 SDK 路径，集成构建联合验证框架、Provider 和后端版本。

独立 Provider SDK 发布仍为后续工作。RPM/DEB 和产品安装由发行方或系统包管理器负责。
本地 AW 扩展通过 `aw-package` 的只读计划和持久恢复管理；其私有 `store` 模块负责文件
验证、记录与事务。移动源码保留包名和二进制名。新增 Provider 目录只有经过明确打包与
生命周期验证后才加入安装器。

根 Cargo manifest 为虚拟 workspace。十个框架 crate 与具体 Provider 分别呈现：

| 职责 | 框架 crate | 边界 |
| --- | --- | --- |
| Provider 管理与配置 | `aw-package` | 经准入的本地扩展、私有文件事务与 AW 配置 |
| 原生方言 | `aw-adapter` | 目录、登记、回调、原生阶段与效果采纳 |
| 统一用户入口 | `aw-cli` | CLI 解析、呈现、兼容与命令编排 |
| 本地运行时 | `aw-service` | Client、绑定、事件、期限、取消和审计 |
| Hook 执行 | `aw-host`、`aw-exec` | 准备 Provider、执行步骤、约束进程寿命并清理 |
| 共享校验 | `aw-config`、`aw-provider`、`aw-contracts` | 配置、Provider 协议/View、schema 和记录校验 |
| 准入、计划 API 与日志 | `aw-core` | Hook 策略准入、独立计划执行 API 与 FileJournal |

Hook 路径为 Adapter → Service → Host → executor → Provider 进程，结果返回 Adapter
供原生采纳。Service 使用 `aw-core::journal::FileJournal`，独立的 `Core::prepare/execute`
保持原职责；其 `ProviderHost` 端口与 `aw-host::Host` 分开。

框架不得依赖具体 Provider，包括 dev-dependencies。`aw-adapter` 不得依赖 Service、
Host 或 CLI 内部实现。Client、wire DTO 和完整 Binding 归 `aw-service`；序列化
HookBinding、参数解析、启动信号处理和 CLI 资源归 `aw-cli`。CLI 直接调用
`aw-package` Rust API 管理本地扩展和离线配置；库不得依赖 CLI 或打印面向用户的输出。
已移除的独立 `aw-package` 可执行程序保持移除，不得重新引入第二个安装器 CLI。

本地扩展安装要求显式绝对 `--bundle` 路径，输入使用 `package.json` 并声明
`aw-provider/v1alpha1`；拒绝 `manifest.json`、AW core 和普通后端包。
`aw-package::store` 是私有事务实现，保留其本地扩展边界。

后端源码构建、组件组合和具体 Provider 默认配置归 distribution Makefile 与
`distribution/packaging/aw/` 脚本。`make -C src/aw build` 只构建 AW CLI。
组件 Makefile 引入自己的 `packaging/build.mk` 和 `install.mk`；install 使用
PREFIX、AWPRJ、DESTDIR 复制预构建文件，不执行后端安装、AW 记录写入、Hook 登记或
服务激活。Provider 在可选 AWPRJ 命名空间下保留 `aw/providers/<id>` 身份。
框架中不新增编译型发行构建器。

AW 包查询和变更拒绝外部包管理器 locator。离线配置不需要 AW 包时可以使用外部管理
的前缀；需要 AW 包的模板拒绝该前缀。此例外不读取或接管外部包清单。
运行时文件租约由 `aw-exec` 管理，无需读取包数据库。
文件所有权保持唯一，不在数据库间自动转移。已安装 Provider 可直接通过显式
`transport.argv` 和进程协议接入，无需 AW 包记录；RPM 管理的文件保留 RPM 所有权。
原生 Hook 注册使用 `aw connect`，本地 Provider 扩展安装使用 `aw install`。

Adapter 只接收 NativeContext 与类型化选项；调用方提供取消和启动文件访问，Adapter
返回计划、比较数据及原生回复。原始回调验证保留严格解码，不能信任回调自报的验证状态。

crate 源码、schema 和 Rust 测试就近存放。`crates/aw-contracts/schemas/` 中的合同均需
登记与测试，受支持版本并存；移除无消费者草稿后通过 Git 查历史，不因版本旧而删除
仍受支持合同。新 Core capability profile 需要 schema、Registry 和跨记录校验；
Provider 私有数据留在其实现。迁移 schema 保留有效字节、ID、登记名称和摘要向量。
所有权或调用路径变化同步更新中英文[架构总览](../architecture.md)、
[架构边界](../architecture-boundaries.md)和[设计索引](../README.md)。
Schema 归属与演进在总览的[规范章节](../architecture.md#schema-归属)维护，其余页面引用。
总览呈现运行流程，编译依赖另列框架与 Provider 表并标明仅测试依赖。
