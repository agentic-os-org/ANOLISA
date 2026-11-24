# 参与 AW 开发

[English](CONTRIBUTING.md)

在 Linux 上修改 AW 框架、Adapter 或门禁。应用接入与部署见[开发者指南](../../docs/developer-guide/zh/aw/index.md)，通用 PR 规则见[仓库贡献指南](../../CONTRIBUTING_zh.md)。

## 构建与检查

准备 rustup、Python 3.11+、Node.js 22+（CI 使用24.15.0）；Rust、rustfmt 和 Clippy 版本由 [rust-toolchain.toml](rust-toolchain.toml) 固定。
从仓库根目录运行。

```bash
python3 src/aw/scripts/check.py
```

门禁执行 Python 行为测试、格式、Clippy、locked Rust 测试、摘要向量、rustdoc、布局与能力清单检查。
日志标出失败命令，每条命令有期限和所属进程清理。修改 AW 实现需通过完整 Linux 门禁；打包流程见[源码构建](../../contribute/zh/aw/building.md)。

| 改动范围 | 最小本地验证命令 |
| --- | --- |
| 文档与链接 | `bash scripts/docs-lint.sh && python3 scripts/docs-link-check.py && python3 -B -m unittest src/aw/tests/test_ci_scope_docs.py` |
| Python 门禁 | `python3 -B -m unittest discover -s src/aw/tests -p 'test_ci_*.py'` |
| Python / TypeScript SDK | 从 `src/aw` 运行 `cargo test --locked -p aw-cli --test sdk_protocol` |
| Provider 协议（`aw-provider`） | 从 `src/aw` 运行 `cargo test --locked -p aw-provider` |
| Host / service / CLI | 从 `src/aw` 运行 `cargo test --locked -p aw-host -p aw-service -p aw-cli` |
| 包执行 | 从 `src/aw` 运行 `cargo test --locked -p aw-exec --test packages` |
| 包数据库与管理 | 从 `src/aw` 运行 `cargo test --locked -p aw-package --lib store::tests` 和 `cargo test --locked -p aw-package --lib provider_tests` |
| 事务恢复 | 从 `src/aw` 运行 `cargo test --locked -p aw-package --lib store::transaction::tests` |
| 一方 Provider workspace | `cargo test --locked --manifest-path providers/PROVIDER/Cargo.toml` |

## 首批可领取贡献方向

外部贡献者可以从以下三个边界清晰的方向快速切入：

| 方向 | 推荐起步任务 | 入口文档 |
| --- | --- | --- |
| 新框架适配与 SDK 桥接 | 用 `LanguageSource`（`aw_sdk.sources` / `@anolisa/aw-sdk/sources`）接入新的 Python/Node.js Agent 并补充示例；新增可选 SDK 适配包或薄层原生 Adapter 并补齐 `regression-matrix.json` 覆盖。 | [双语言 Runtime SDK](../../docs/developer-guide/zh/aw/deployment/native-sdk.md) · [Agent 接入索引](../../docs/developer-guide/zh/aw/native-agents.md) |
| 轻量外部 Provider | 基于 `aw-provider/v1alpha1`（`describe` / `validate_config` / `invoke`）编写独立 Provider，例如工作区／文件快照、规则或语义命令／提示词检查、自定义审计输出或结果过滤。 | [Provider 开发与分发](../../docs/developer-guide/zh/aw/providers/index.md) · [Provider 协议](../../docs/developer-guide/zh/aw/providers/protocol.md) |
| 文档与示例完善 | 补充可运行的端到端示例、新版本 Agent 排错指南，或完善中英文（`en/` + `zh/`）文档一致性与导航。 | [快速入门](../../docs/user-guide/zh/aw/index.md) · [文档规范](../../specs/documentation-standard.md) |

向本仓库提交新框架或新 Provider PR 前，建议先开 Issue 说明目标框架／协议边界与本地验证方式，并遵循[仓库贡献指南](../../CONTRIBUTING_zh.md)。

## 修改能力支持

1. 更新 Adapter catalog、配置 Schema 及实际接线。
2. 在 [regression-matrix.json](tests/regression-matrix.json) 填入每个 event/effect 的真实用例，以及安全、Tokenless、生命周期组合用例。
3. 从 `src/aw` 运行以下命令，同步 README 和能力页中的两张生成表（Hook 支持矩阵与按 Agent 的 16 Hook 覆盖表）。

```bash
python3 scripts/check_matrix.py --write-docs
python3 scripts/check.py
```

缺失、被忽略或空的必跑用例会使门禁失败。原生效果另外通过 [native-matrix.json](tests/native-matrix.json) 记录固定版本、命令和报告。
执行步骤与证据要求见[原生与发行验收](../../contribute/zh/aw/testing.md)。

## 目录与代码边界

通用框架位于 `crates/`，具体 Provider 位于仓库根 `providers/` 的独立 workspace。
`make -C src/aw build` 只构建 AW CLI；`make -C src/aw install` 使用 `PREFIX`、`AWPRJ` 和可选 `DESTDIR` 复制预构建文件。Provider 通过各自的 Makefile 构建和安装。后端组合与源码归档由发行方 Makefile 和 `distribution/packaging/aw/` 脚本负责。
`aw install --bundle` 管理本地 Provider 扩展，不安装 AW core 或后端软件。原生 RPM/DEB 继续由原生包管理器拥有；外部安装的 Provider 通过显式 `transport.argv` 接入，无需 AW 包记录。
Adapter 管理原生方言；CLI 管理参数、信号和资源；service 管理状态、预算与审计。
架构图见[设计索引](docs/design/zh/README.md)，文档读者与写作要求见 [AGENTS.md](AGENTS.md#documentation-audiences-and-writing)。

Rust、Python、JavaScript 文件上限为 699 行，600 行时提示拆分；既有 validation.rs 的 711 行例外保持固定。
修改边界时更新 `scripts/check_layout.py` 及行为测试。测试输出放在 `target/`，显式清理所属进程、容器和临时目录。

## CI 结果

`AW / required` 汇总同一候选 SHA 的 AW 与选中的 Tokenless 兼容检查。缺失证据、失败和取消都会阻断。
普通文档改动使用 AW fixture 门禁；Tokenless 消费源码、协议、执行器和打包输入变化还会触发真实后端兼容检查。
固定原生 SDK、包安装与真实后端组合由[集成验收](../../contribute/zh/aw/testing.md)单独运行。
