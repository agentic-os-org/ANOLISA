# RTK 库接口维护

[English](../../en/tokenless/rtk-library.md)

本文说明通过补丁维护的 RTK v0.49.0 库接口。Tokenless 当前仍调用并分发独立 RTK 程序，其 CLI 尚未链接这个库。

## 公开 API

补丁新增 `lib.rs`，公开 `rewrite(&str) -> RewriteOutcome`、
`execute(&[OsString]) -> Result<i32, RtkError>`、`VERSION` 及必要的结果和错误类型。

`rewrite` 保留原生 `rtk` 命令文本、配置、权限规则及恢复读取回执。调用方将 `Allow`、`Ask`、`Deny` 和 `Passthrough` 映射为输出及退出码。改写不代表获得执行授权。

`execute` 接收不含程序名称的参数，保留命令标准输入输出及退出状态。帮助请求、参数错误和执行错误返回调用方。它不读取宿主命令行，也不替换宿主的信号处理器。期限、进程终止及后代进程清理由宿主负责。管理命令既不能进入解析后的执行入口，也不能进入原生命令 fallback。

`lib.rs` 私有编译 `main.rs`，复用其中的参数定义和原地分发。更新 RTK 时应保留独立 CLI 启动逻辑与库入口的边界，确保库调用不启动遥测或 Hook 提醒。

## 固定源码与验证

库接口维护在 `src/tokenless/third_party/patches/rtk-library.patch` 中。
`src/tokenless/scripts/setup-rtk.sh` 声明固定提交及有序补丁列表。复用准备好的源码树之前，它校验补丁 SHA-256 和应用补丁后的源码摘要；不含 Git 元数据的源码归档同样校验。执行 `just clean-rtk` 并重新准备前，应保存本地修改。

在 Linux 上使用 Rust 1.91 或更高版本构建和测试。首次准备需要 `git`、`patch` 及对 `https://github.com/rtk-ai/rtk.git` 的网络访问；校验通过的源码树可离线复用。

```bash
cd src/tokenless
bash scripts/setup-rtk.sh
RUSTUP_TOOLCHAIN=1.91.0 make test-rtk-integration
```

该目标先执行库与独立 CLI 的格式检查、Clippy、测试及 rustdoc，再运行 Tokenless 到 RTK 的集成测试。仅验证 RTK 时运行 `RUSTUP_TOOLCHAIN=1.91.0 make test-rtk-library`。更新固定版本时，应审查解析器命令清单测试中的公开范围决定，并保留权限结果、显式参数、返回宿主、信号处理器及独立 CLI 兼容性测试。

## 旧 RTK Hook 完整性

旧 Hook 的完整性检查属于独立 RTK CLI。库直接执行 Rust 命令处理函数；`rewrite` 和 `execute` 都不检查或运行 `~/.claude/hooks/rtk-rewrite.sh`，也不校验其哈希基线。该脚本被修改不会阻断库执行、原生命令 fallback 或 `recall`。配置和改写权限规则仍然生效。

独立 CLI 保留上游检查及命令分类：`rtk git status` 检查完整性，而 `rtk rewrite` 和 `rtk recall` 豁免。如果独立 CLI 报告脚本被修改，应先用 `rtk verify` 检查，审阅修改后再运行 `rtk init -g --auto-patch`。这些管理命令仍不属于库 API。不要删除基线或重新信任来源不明的修改来绕过独立 CLI 的检查。

## 回滚

一起回退库补丁及其源码准备、验证改动，然后重新生成准备好的 RTK 源码树。无需迁移用户配置或数据库。
