# AW Rust SDK

[English](README.md)

Rust 应用通过一个 `aw-sdk` 依赖调用 sec-core 代码扫描和 Tokenless。
客户端和 Runtime 由组件维护；SDK 不启动系统服务，也不注册 Agent Hook。

配置、样例和组件 feature 见 [SDK 指南](../../../../docs/developer-guide/zh/aw/sdk.md)。
SDK 使用独立 workspace，不计入 AW 的九个框架 crate。

在本目录执行：

```bash
cargo test --locked
cargo doc --no-deps --locked
```

仓库门禁 `python3 src/aw/scripts/check.py` 还验证各组件的独立选择，
禁止 SDK 引入 AW Service 或具体 Provider 实现。
