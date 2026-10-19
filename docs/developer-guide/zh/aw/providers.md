# Provider 开发与分发

[English](../../en/aw/providers.md)

Provider 将安全检查、输出压缩等能力接入 AW。它通过进程协议工作，可以独立开发、
分发和升级，也可以收录到 [Provider 协作目录](../../../../src/aw/providers/README_zh.md)。

## 目录与职责

```text
src/aw/
├── crates/                  # AW 通用框架
│   └── aw-provider/         # 协议与能力准入库
└── providers/               # 具体实现与协作入口
    ├── sec-core/            # 安全 Provider
    └── tokenless/           # 压缩 Provider
```

`aw-provider` 是供 Host 和实现方复用的协议库。`providers/` 中的每个目录则对应一项
具体集成，可以包含程序、Schema、配置示例和测试。现有二进制仍叫
`aw-provider-sec-core`、`aw-provider-tokenless`，安装包和启动命令保持兼容。

## 独立接入

1. 实现 `describe`、`validate_config` 和 `invoke`，通过 stdin/stdout 交换 JSON。
   协议字段、错误和能力声明见 [Provider 协议](../../../../src/aw/docs/design/provider-protocol_zh.md)。
2. 用自己的发布方式交付可执行程序及依赖，配置 `transport.argv` 指向安装后的绝对路径。
   后端所需凭据、socket 和数据目录由 Provider 的安装说明约定。
3. 在配置中声明 Provider 和 Hook 步骤，验证允许、阻断、超时、错误与清理行为。
   从[组件接入教程](integration.md)或[最小 Rust 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs)开始。

Provider 返回候选效果；Adapter 负责在原生框架中采用。自定义实现无需链接 AW，
也不必使用 Rust。用户已有的 Agent 和模型配置继续由原生框架管理。

## 收录到协作目录

按 `providers/<provider>/` 提交代码，附维护人、来源和许可证、双语使用说明及离线测试。
Rust 包加入 workspace，并更新[布局检查](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py)。
新测试应接入 AW 门禁；完整贡献约定见[协作目录](../../../../src/aw/providers/README_zh.md)。

现有第一方 Provider 可分别构建、测试。以下命令从 `src/aw` 执行。

```bash
cargo build --locked -p aw-provider-sec-core -p aw-provider-tokenless
cargo test --locked -p aw-provider-sec-core -p aw-provider-tokenless
```

`aw-build` 和 `aw-package` 当前只打包、安装内置组件，不能自动安装任意社区 Provider。
独立安装不影响协议接入；加入统一安装包需另补打包清单和安装验收。
