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
与真实 Tokenless Runtime 对照结果，验证恢复授权，并检查单组件构建不引入另一组件。
本地 RTK fixture 验证调用和结果映射，不能代替真实 RTK 与 root daemon 的发行验收。
