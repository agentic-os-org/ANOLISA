# sec-core Provider

[English](README.md)

通过现有 sec-core CLI 提供命令安全检查。本目录维护桥接实现，后端由对应组件提供。

通过 Hook 步骤的 `match.tools` 选择代码工具，使用 `bash` 等标准名称或精确的
`native:<adapter>:<name>` 选择器。Provider `config.tools` 按规范化的
`event.tool.name` 映射输入。
选中的工具缺少映射时默认返回错误；显式配置 `unmapped_tool: observe` 可只观察而不扫描。
必需的安全步骤使用 `mode: block` 和 `on_error: block`，阻断风险和检查失败。

维护人按 [CODEOWNERS](../../.github/CODEOWNERS) 由 @kongche-jbw 负责。
桥接代码使用 [Apache-2.0](../../LICENSE)，后端来源见 [agent-sec-core](../../src/agent-sec-core/README_zh.md)。

- [配置与运行](../../docs/developer-guide/zh/aw/providers/sec-core.md)
- [配置示例](examples/aw.yaml)
- [协作与分发约定](../README_zh.md)

从仓库根目录构建和测试。

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
```

Cargo 包名与二进制名保留为 `aw-provider-sec-core`。随 AW Preview 分发时使用
[构建指南](../../contribute/zh/aw/building.md)生成匹配的包。
