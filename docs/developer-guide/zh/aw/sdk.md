# 使用 Rust SDK

[English](../../en/aw/sdk.md)

应用自行管理 Agent 循环、需要直接调用安全或压缩能力时，可以使用 `aw-sdk`。
若要为现有 Agent 接入受管 Hook，使用
[`aw run`](../../../user-guide/zh/user-entrypoint/aw/README.md)。

SDK 位于 `src/aw/sdk/rust/`，使用独立于框架的 workspace。
在本地 Cargo 项目中引用该目录：

```toml
[dependencies]
aw-sdk = { path = "/absolute/path/to/ANOLISA/src/aw/sdk/rust" }
```

默认启用两个组件。应用或 Provider 只需要一个组件时，可以单独选择，
避免链接另一个组件的实现：

```toml
[dependencies]
aw-sdk = { path = "/absolute/path/to/ANOLISA/src/aw/sdk/rust", default-features = false, features = ["tokenless"] }
```

| Feature | 操作 | 运行依赖 |
| --- | --- | --- |
| `sec-core` | `scan_code` | 可连接的 sec-core daemon；客户端验证服务端 UID 为 0 |
| `tokenless` | `before_model`、`pre_tool`、`post_tool`、`retrieve` | 显式状态目录；PreTool 需要显式 RTK 可执行文件 |

## 导出公共源码

组件 owner 可以导出已提交的 SDK 和公共 Provider 库，无需包含 AW Service、
具体 Provider 实现或后端可执行文件。在仓库根目录使用 Python 3.11 及以上版本执行：

```bash
python3 src/aw/scripts/export_sdk.py \
  --output src/aw/target/aw-sdk-source.tar.gz
mkdir -p /tmp/aw-sdk-sources
tar -xzf src/aw/target/aw-sdk-source.tar.gz -C /tmp/aw-sdk-sources
cd /tmp/aw-sdk-sources/aw-sdk-0.1.0/src/aw/sdk/rust
cargo test --locked
```

AW CI 通过后，在 `aw-sdk-source-<tested SHA>` 中保留已验证的源码归档和
`result.json`，有效期 14 天。这是候选构建产物，不是注册中心发行版。

导出工具读取已提交的 `HEAD`，可用 `--revision` 选择其他提交；不会覆盖已有归档，
并输出归档 SHA-256。`source.json` 记录组件版本、原始 Git blob 和包内文件摘要。
Workspace 清单保留依赖继承，只收录公共成员；Schema、共享工具分类数据、
测试 fixture 和许可证一并交付。注册中心依赖由 Cargo.lock 固定，构建时需要访问
注册中心或使用已有 Cargo 缓存。

仓库外应用将 `aw-sdk` 依赖指向解压后的 SDK 目录，由自身 Cargo.lock 记录依赖解析。
源码包不安装 RTK 或 sec-core daemon。面向终端用户的二进制安装由
[发行流程](../../../../src/aw/docs/design/provider-distribution_zh.md)负责。

## 配置与调用

通过 `Client::new(Config)` 传入绝对路径。未配置的组件返回
`Error::NotConfigured`，构造客户端不会安装或启动组件。
Tokenless 会创建配置中的状态目录和数据库；客户端释放后保留这些数据。
`stash_error()` 和 `stats_error()` 用于检查存储不可用的原因，
不能把返回结果视为恢复或统计已成功。

[应用样例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/sdk/rust/examples/application.rs)通过 daemon 扫描代码，
再通过 Tokenless 处理 API 结果。启动 sec-core daemon 后，在仓库根目录执行：

```bash
cd src/aw/sdk/rust
cargo run --locked --example application -- /run/agent-sec-core/daemon.sock "$HOME/.local/state/aw-sdk"
```

将 `tokenless::Config::rtk_path` 设为已安装的 RTK 可执行文件。
SDK 不查找用户 PATH。TOON 使用链接的库。

`CallContext` 携带应用、会话和工具调用归属。会话和工具调用 ID 映射为
sec-core 兼容关联标签，可选 W3C carrier 单独传递。这些字段不证明身份，也不授予权限。

## 显式采用结果

- `scan_code` 分别返回类型化扫描结果、daemon 拒绝或本地传输错误。
  `ok` 表示扫描已完成；是否继续执行需要检查 `verdict`。
  调用超时不代表服务端已取消，也不能据此自动重试。
- Tokenless 返回转换结果，由应用决定如何采用。只声明应用确实支持的能力；
  失败工具输出和 RTK 已处理的输出沿用组件的保留规则。
- `retrieve` 要求提供来自可信、当前可见会话状态的 marker，不能相信模型自报可见。
  声明恢复工具意味着应用已经注册可用且执行授权检查的实现。

调用是同步的，不自动重试。SDK 不经过 AW Service，不产生 Host/Core 回执，
也不证明原生效果已经采用。AW Provider 仍遵守自身的效果准入：
PreTool 建议必须通过一次新的调用重新接受安全检查和原生授权。

## 验证改动

在仓库根目录执行：

```bash
python3 src/aw/scripts/check.py
```

门禁编译双组件和各自单独启用的配置，核对必需 SDK 测试清单，
与真实 Tokenless Runtime 对照结果，验证恢复授权、可复现源码导出和仓库外消费者构建，并检查单组件构建不引入另一组件。
本地 RTK fixture 验证调用和结果映射，不能代替真实 RTK 与 root daemon 的发行验收。

验证已安装组件时，提供运行中的 sec-core daemon 和受管 RTK 可执行文件。
在 `src/aw/sdk/rust/` 或源码包解压后的同名目录执行：

```bash
cargo run --locked --example component_check -- \
  /run/agent-sec-core/daemon.sock \
  "$PWD/target/component-check/state" \
  /absolute/path/to/managed/rtk
```

此检查要求两个组件均可用，缺少依赖会失败。它验证当前扫描规则的 pass/warn 结果、
Tokenless 四项操作、未授权恢复被拒绝，以及失败或 RTK 已处理的输出保持原样。
结果输出为 JSON，指定的状态目录保留供检查。危险扫描片段仅作为源码数据，
不会执行；检查也不安装服务、不执行建议的工具命令，不证明原生 Hook 已采用结果。
