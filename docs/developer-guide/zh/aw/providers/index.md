# Provider 开发与分发

[English](../../../en/aw/providers/index.md)

Provider 将安全检查、输出压缩等能力接入 AW。它通过进程协议工作，可以独立开发、
分发和升级，也可以收录到 [Provider 协作目录](../../../../../providers/README_zh.md)。

## 目录与职责

```text
ANOLISA/
├── src/aw/crates/           # AW 通用框架
│   └── aw-provider/         # 通用协议与准入
└── providers/               # 各实现独立 workspace
    ├── agent-sec-core/
    └── tokenless/
```

`aw-provider` 是供 Host 和实现方复用的协议库。`providers/` 中的每个目录则对应一项
具体集成，可以包含程序、Schema、配置示例和测试。现有二进制仍叫
`aw-provider-sec-core`、`aw-provider-tokenless`，安装包和启动命令保持兼容。

## OS 分发设计

组件 owner 负责 Provider 的实现、测试、依赖与发布；OS 发行方选择组件版本，
验证来源和组合兼容性，再交付安装包与升级路径。各组件维护自己的发布周期。

一方 Provider 的目标是仅依赖固定版本的公共 AW 库，在仓库外完成构建、测试和发布，
自身后端依赖由 owner 管理。第三方可直接实现公开进程协议，通过 YAML 接入，
无需链接 Rust 库或检出 ANOLISA。分发层消费各组件产物，负责随包依赖的组合验证。

完整职责和迁移范围见[架构边界](../../../../../src/aw/docs/design/zh/architecture-boundaries.md)。

## 独立接入

1. 实现 `describe`、`validate_config` 和 `invoke`，通过 stdin/stdout 交换 JSON。
   协议字段、错误和能力声明见 [Provider 协议](protocol.md)。
2. 用自己的发布方式交付可执行程序及依赖，配置 `transport.argv` 指向安装后的绝对路径。
   后端所需凭据、socket 和数据目录由 Provider 的安装说明约定。
3. 在配置中声明 Provider 和 Hook 步骤，验证允许、阻断、超时、错误与清理行为。
   从[组件接入教程](../integration.md)或[最小 Rust 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs)开始。

Provider 返回候选效果；Adapter 负责在原生框架中采用。自定义实现无需链接 AW，
也不必使用 Rust。用户已有的 Agent 和模型配置继续由原生框架管理。

## 收录到协作目录

按 `providers/<provider>/` 提交代码，附维护人、来源和许可证、双语使用说明及离线测试。
每个 Rust 包自带 workspace 和锁文件，并更新[布局检查](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py)。
新测试应接入 AW 门禁；完整贡献约定见[协作目录](../../../../../providers/README_zh.md)。

以下命令从仓库根目录执行。

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

目前两个 workspace 通过仓库内路径复用 AW 库，Tokenless 组合测试还依赖 sec-core
桥接库，尚不能脱离仓库独立构建。固定版本 SDK、独立发布及产物级组合测试留待后续。
Preview 将 core 与 Provider 作为一批构建产物交付，安装时校验版本和源码提交一致。

`aw-build` 生成内置组件包；社区 Provider 可自行分发，也可按[本地包格式](../reference/packages.md)交给 `aw install` 安装。
独立安装不影响协议接入；加入统一安装包需另补打包清单和安装验收。
