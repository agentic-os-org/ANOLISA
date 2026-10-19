# Tokenless Provider

[English](README.md)

通过现有 Tokenless CLI 提供工具输出压缩和命令建议。本目录维护桥接实现，后端由对应组件提供。

维护人按 [CODEOWNERS](../../.github/CODEOWNERS) 由 @kongche-jbw 负责。
桥接代码使用 [Apache-2.0](../../LICENSE)，后端来源见 [tokenless](../../src/tokenless/README_zh.md)。

- [配置与运行](../../docs/developer-guide/zh/aw/tokenless.md)
- [配置示例](examples/aw.yaml)
- [协作与分发约定](../README_zh.md)

从仓库根目录构建和测试。

```bash
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

Cargo 包名与二进制名保留为 `aw-provider-tokenless`。随 AW Preview 分发时使用
[构建指南](../../docs/developer-guide/zh/aw/building.md)生成匹配的包。
