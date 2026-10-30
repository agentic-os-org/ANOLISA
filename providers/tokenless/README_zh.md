# Tokenless Provider

[English](README.md)

通过现有 Tokenless CLI 提供工具输出压缩和命令建议。本目录维护桥接实现，后端由对应组件提供。

维护人按 [CODEOWNERS](../../.github/CODEOWNERS) 由 @kongche-jbw 负责。
桥接代码使用 [Apache-2.0](../../LICENSE)，后端来源见 [tokenless](../../src/tokenless/README_zh.md)。

- [配置与运行](../../docs/developer-guide/zh/aw/providers/tokenless.md)
- [配置示例](examples/aw.yaml)
- [协作与分发约定](../README_zh.md)

从仓库根目录构建和测试。

```bash
make -C providers/tokenless build
make -C providers/tokenless install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

Cargo 包名与二进制名保留为 `aw-provider-tokenless`。独立归档分发使用
[构建指南](../../contribute/zh/aw/building.md)生成匹配的包。

安装只复制预构建桥接程序、示例、已有私有 schema 和文档，不安装或启动后端。
省略 `DESTDIR` 后，按调用者权限直接安装；`AWPRJ` 可为私有路径增加命名空间。
运行配置使用最终程序的绝对 `transport.argv`。
