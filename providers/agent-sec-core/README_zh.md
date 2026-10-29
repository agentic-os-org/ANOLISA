# sec-core Provider

[English](README.md)

通过现有 sec-core CLI 提供命令安全检查。本目录维护桥接实现，后端由对应组件提供。

通过 Hook 步骤的 `match.tools` 选择代码工具，使用 `bash` 等标准名称或精确的
`native:<adapter>:<name>` 选择器。Provider `config.tools` 按规范化的
`event.tool.name` 映射输入。
选中的工具缺少映射时默认返回错误；显式配置 `unmapped_tool: observe` 可只观察而不扫描。
必需的安全步骤使用 `mode: block` 和 `on_error: block`，阻断风险和检查失败。

随附 Qoder 示例在前后两个阶段都选择原生 `Bash`。未列出的工具保留原生行为，
步骤记录为跳过。SDK 映射与输入提取规则见[配置指南](../../docs/developer-guide/zh/aw/providers/sec-core.md#配置-provider)。

维护人按 [CODEOWNERS](../../.github/CODEOWNERS) 由 @kongche-jbw 负责。
桥接代码使用 [Apache-2.0](../../LICENSE)，后端来源见 [agent-sec-core](../../src/agent-sec-core/README_zh.md)。

- [配置与运行](../../docs/developer-guide/zh/aw/providers/sec-core.md)
- [配置示例](examples/aw.yaml)
- [协作与分发约定](../README_zh.md)

从仓库根目录构建和测试。

```bash
make -C providers/agent-sec-core build
make -C providers/agent-sec-core install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
```

Cargo 包名与二进制名保留为 `aw-provider-sec-core`。独立归档分发使用
[构建指南](../../contribute/zh/aw/building.md)生成匹配的包。

安装只复制预构建桥接程序、示例、已有私有 schema 和文档，不安装或启动后端。
省略 `DESTDIR` 后，按调用者权限直接安装；`AWPRJ` 可为私有路径增加命名空间。
运行配置使用最终程序的绝对 `transport.argv`。
