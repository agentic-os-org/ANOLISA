# 参与 AW 开发

[English](CONTRIBUTING.md)

在 Linux 上修改 AW 框架、Adapter 或门禁。应用接入与部署见[开发者指南](../../docs/developer-guide/zh/aw/index.md)，通用 PR 规则见[仓库贡献指南](../../CONTRIBUTING_zh.md)。

## 构建与检查

准备 rustup、Python 3.11+、Node.js；Rust、rustfmt 和 Clippy 版本由 [rust-toolchain.toml](rust-toolchain.toml) 固定。
从仓库根目录运行。

```bash
python3 src/aw/scripts/check.py
```

门禁执行 Python 行为测试、格式、Clippy、locked Rust 测试、摘要向量、rustdoc、布局与能力清单检查。
日志标出失败命令，每条命令有期限和所属进程清理。修改 AW 实现需通过完整 Linux 门禁；打包流程见[源码构建](../../contribute/zh/aw/building.md)。

| 改动范围 | 开发中可先运行 |
| --- | --- |
| 文档、导航 | `bash scripts/docs-lint.sh`、`python3 scripts/docs-link-check.py` |
| Python 门禁 | `python3 -B -m unittest discover -s src/aw/tests -p 'test_ci_*.py'` |
| Host / service / CLI | 从 `src/aw` 运行 `cargo test --locked -p aw-host -p aw-service -p aw-cli` |
| 包执行 | 从 `src/aw` 运行 `cargo test --locked -p aw-exec --test packages` |
| 包数据库与管理 | 从 `src/aw` 运行 `cargo test --locked -p aw-package --test management --test database` |
| 事务恢复 | 从 `src/aw` 运行 `cargo test --locked -p aw-package transaction::tests` |
| Provider | 对应独立 workspace 的 `cargo test --locked --manifest-path providers/PROVIDER/Cargo.toml` |

## 修改能力支持

1. 更新 Adapter catalog、配置 Schema 及实际接线。
2. 在 [regression-matrix.json](tests/regression-matrix.json) 填入每个 event/effect 的真实用例，以及安全、Tokenless、生命周期组合用例。
3. 从 `src/aw` 运行以下命令，同步 README 和能力页的可见矩阵。

```bash
python3 scripts/check_matrix.py --write-docs
python3 scripts/check.py
```

缺失、被忽略或空的必跑用例会使门禁失败。原生效果另外通过 [native-matrix.json](tests/native-matrix.json) 记录固定版本、命令和报告。
执行步骤与证据要求见[原生与发行验收](../../contribute/zh/aw/testing.md)。

## 目录与代码边界

通用框架位于 `crates/`，具体 Provider 位于仓库根 `providers/` 的独立 workspace。
Adapter 管理原生方言；CLI 管理参数、信号和资源；service 管理状态、预算与审计。
架构图见[设计索引](docs/design/zh/README.md)，文档读者与写作要求见 [AGENTS.md](AGENTS.md#documentation-audiences-and-writing)。

Rust、Python、JavaScript 文件上限为 699 行，600 行时提示拆分；既有 validation.rs 的 711 行例外保持固定。
修改边界时更新 `scripts/check_layout.py` 及行为测试。测试输出放在 `target/`，显式清理所属进程、容器和临时目录。

## CI 结果

`AW / required` 汇总同一候选 SHA 的 AW 与选中的 Tokenless 兼容检查。缺失证据、失败和取消都会阻断。
普通文档改动使用 AW fixture 门禁；Tokenless 消费源码、协议、执行器和打包输入变化还会触发真实后端兼容检查。
固定原生 SDK、包安装与真实后端组合由[集成验收](../../contribute/zh/aw/testing.md)单独运行。
