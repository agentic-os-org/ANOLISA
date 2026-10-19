# AW Provider 协作目录

[English](README.md)

这里收录具体 Provider，每个目录维护自己的实现、测试、配置示例和分发说明。
AW 框架与协议库留在 `../crates/`。

| Provider | 能力 | 来源 |
| --- | --- | --- |
| [sec-core](sec-core/README_zh.md) | 命令安全检查 | 第一方 sec-core CLI 与后端 |
| [Tokenless](tokenless/README_zh.md) | 工具输出压缩和命令建议 | 第一方 Tokenless CLI 与随包 RTK |

## 协作与分发

收录到本仓库的实现使用 `providers/<provider>/`。Provider 也可以在独立仓库开发、
单独安装，通过 stdio 实现 `aw-provider/v1alpha1`，由配置中的 `transport.argv`
指定程序。运行时接入不依赖 Cargo 包名或源码目录。

提交 Provider 时一并提供以下内容。

- 维护人或联系方式、来源与许可证、支持的后端版本，以及双语 README。
- 实现、离线契约测试和可运行的配置示例。
- 构建安装方法、后端要求、数据位置与清理步骤。

Rust 实现加入 AW workspace 和依赖布局检查。其他语言使用相同进程协议，也需将
契约测试接入 AW 门禁。原生框架适配放在 `aw-service`，后端能力由 Provider 负责。

现有打包器支持 core、sec-core 和 Tokenless。新增目录不会自动加入安装包；
独立 Provider 自带安装方式。若要纳入统一分发，还需补包清单、构建安装逻辑及生命周期测试。

协议、示例和验证入口见 [Provider 开发与分发](../../../docs/developer-guide/zh/aw/providers.md)。
