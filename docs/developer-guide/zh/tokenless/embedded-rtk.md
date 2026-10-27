# 内置 RTK 维护

[English](../../en/tokenless/embedded-rtk.md)

Tokenless 将 RTK v0.49.0 链接进 CLI。npm、raw/RPM、Python wheel、ANOLISA 和 AW
发布包交付 `tokenless`（Windows 为 `tokenless.exe`）。独立 RTK CLI 仍可构建，用于
验证上游兼容性。

## 库接口边界

`third_party/patches/rtk-library.patch` 为 RTK 新增 `lib.rs`。公开接口限于
`rewrite(&str) -> RewriteOutcome`、`execute(&[OsString]) -> Result<i32, RtkError>`、
`VERSION` 及对应结果和错误类型。执行参数不含程序名；库返回结果，不显式退出宿主进程，
并保留命令标准输入输出。

独立 `main.rs` 保留管理命令分发、遥测和 Hook 提醒。两个入口共享参数定义和执行分发，
不需要 `CliMode`。Vitest 使用模块自身的执行选项。参数恢复接收调用者提供的参数列表，
不读取宿主进程的命令行。

只有 `tokenless-cli` 链接 RTK。运行时和 Python 原生扩展通过选定的 Tokenless 程序执行
`rtk rewrite` 参数，保留既有进程超时和后代进程清理。RTK 库的 rewrite 保留原生 `rtk`
文本；Tokenless 集成层只转换确认由本次改写插入的包装位置。POSIX 绑定 Tokenless
绝对路径；PowerShell 使用局部转发函数，保留原生参数、管道输入、退出状态及环境恢复。

内置入口不开放上游管理命令。受支持入口和 rewrite 退出码见
[CLI 参考](../../../user-guide/zh/tokenless/cli-reference.md#随包提供的-rtk-命令)。

## 源码准备与验证

使用 Linux 和 Rust 1.91 或更高版本构建。在 Cargo 解析工作区前准备 RTK 源码。
准备脚本同时校验固定的上游提交、按顺序排列的补丁 SHA-256 和应用补丁后的源码树摘要。
源码修改或新增未跟踪文件会阻止缓存复用，没有 Git 元数据的源码包也会校验。补丁集变化时会拒绝复用
旧生成目录；删除并重新准备前应先保留其中的本地修改。

```bash
cd src/tokenless
bash scripts/setup-rtk.sh
cargo +1.91.0 build --locked -p tokenless-cli
cargo +1.91.0 test --locked --manifest-path third_party/rtk/Cargo.toml
cargo +1.91.0 test --locked -p tokenless-cli -p tokenless-runtime
make test-rtk-integration
```

Windows 最终 CLI 保留 RTK 所需的栈大小设置。发布内容包含上游许可、源码提交、补丁摘要
和本地补丁。AW 复制受版本管理的 Tokenless 源码，并在本次独立目录内准备固定 RTK 依赖。

`make fmt` 和 `make lint` 也会准备这一工作区依赖。首次运行需要 `git`、`patch` 和
访问 `https://github.com/rtk-ai/rtk.git` 的网络；已通过校验的源码目录可离线复用。
准备脚本会明确报告拉取失败。

## 旧版 Hook 完整性

`rewrite` 仅依据配置和权限规则转换文本，不执行或授权命令，因此不检查旧版 RTK Hook。
`execute` 会在解析后的执行路径和原生命令 fallback 前检查旧版 Hook 完整性。
有基准摘要的 Hook 被修改时，执行会被阻止，并向宿主返回错误。独立 RTK CLI 保留分发前的完整性检查。

如果校验报告 `rtk-rewrite.sh` 被修改，应先检查文件及其来源。Hook 检查和修复需要另行安装或
构建上游独立 RTK CLI（先执行 `rtk verify`，审查后再运行 `rtk init -g --auto-patch`）。
Tokenless 不开放这些管理命令。不要删除基准或信任来源不明的修改来绕过校验。

## 升级与回滚

已有显式 `rtk ...` 命令会保留为对独立 RTK 的调用，改写不会接管或重定向这些命令。
如果此前依赖 Tokenless 随包交付的启动程序，请将这些命令改成 `tokenless rtk ...`；
未安装独立 RTK 时，旧命令将无法找到程序。

Python 公共 SDK 和 `rtk_enabled` 不变，内部辅助程序改为 `_bin/tokenless` 或
`_bin/tokenless.exe`。运行时协议值、`rewrite-command` 统计、归因字段、执行回执和数据库格式不变。

AW 配置使用 `--cli-sha256` 校验 `--cli` 指向的同一个程序，升级 AW 时安装到新前缀，通过 `aw configure` 生成新受管理配置，并使用新配置重启会话。
新配置使用 `--cli-sha256`；不再接受旧 `--rtk` 和 `--rtk-sha256` 参数。
保留旧前缀和配套配置用于回滚，确认不再需要回滚后通过 AW 卸载旧前缀。
PreTool 和 PostTool 在执行前校验所有者、权限、摘要、内置 RTK 版本及请求剩余期限。
重试建议仍需经过新一次原生授权。

旧 RTK 文件仅通过原安装器的所有权记录或其管理的辅助程序目录清理，用户另行安装的 RTK
应保留。回滚时恢复旧版程序或包及配套配置，数据库无需迁移。
