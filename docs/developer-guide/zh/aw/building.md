# 从源码构建 AW

[English](../../en/aw/building.md)

在自己的 Linux 机器上编译 AW，生成可复制到客户环境的安装包。只使用 AW 时构建
core；需要安全检查或输出压缩时构建 all-in-one。现成安装包见
[安装 AW](../../../user-guide/zh/user-entrypoint/aw/installation.md)。

## 准备构建环境

支持 Linux x86_64 和 aarch64 本机编译。准备 Git、rustup、C 编译器、pkg-config
和 OpenSSL 开发头文件。Ubuntu / Debian 可先安装系统依赖，再按
[Rust 官方说明](https://www.rust-lang.org/tools/install)安装 rustup。

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config libssl-dev
```

构建时需要联网下载源码、Rust 工具链和 Cargo 依赖；包含 Tokenless 的包还会获取
固定版本 RTK。本页说明客户本地编译，尚未提供断网环境所需的依赖镜像和离线包。
目标机器须有与构建环境兼容的 glibc；为旧系统分发时应在对应系统上构建。

## 构建安装包

以下命令构建 core。保留干净且已提交的源码工作树，输出目录必须为空。
进入 `src/aw` 后，rustup 会按仓库的 `rust-toolchain.toml` 选择工具链。

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa/src/aw
cargo run --locked --release -p aw-package --bin aw-build -- \
  --version 0.1.0-preview.1 --component core \
  --output "$PWD/target/aw-preview-packages"
cd target/aw-preview-packages
sha256sum -c SHA256SUMS
```

检查结果应全部为 `OK`。目录内有 `aw-core-0.1.0-preview.1-linux-<arch>.tar.gz`
和 `SHA256SUMS`，其中 `<arch>` 是 `x86_64` 或 `aarch64`。
`--version` 是本次打包使用的 Preview 标识，不是源码 tag。

需要安全检查和 Tokenless 时，将上面的 `--component core` 改为
`--component all`，并使用新的空输出目录。它会同时生成 core、两个 Provider 扩展包
和 `aw-all-in-one`。Tokenless 包已包含 RTK，TOON 编码也随包提供，客户无需单独
安装它们。all-in-one 包含可选组件，启用哪些能力仍由配置决定。

`sec-core` 和 `tokenless` 选项只生成相应扩展包，安装时必须搭配同一源码提交和
打包版本的 core。向客户交付时应选同一次构建的产物。

## 安装、运行和验收

将选定的压缩包及 `SHA256SUMS` 交给客户。若只复制部分包，校验清单也只保留对应
条目，避免因未交付的包报错。

从[解压并安装](../../../user-guide/zh/user-entrypoint/aw/installation.md#安装)
开始，依次完成安装、生成配置和启动 Qoder。选择 all-in-one 时将安装示例中的
`AW_BUNDLE_KIND` 设为 `aw-all-in-one`。

完成后应能看到 Agent 正常启动、`aw status` 返回运行状态，并通过 `aw stop`
停止本地服务。core 的初始配置不含策略；验证可选能力请继续阅读
[Tokenless](../../../user-guide/zh/user-entrypoint/aw/tokenless.md)或
[安全检查](../../../user-guide/zh/user-entrypoint/aw/security.md)。

需要从源码构建 Agentic OS 的其他组件时，使用[仓库构建指南](../../../BUILDING_zh.md)。
AW 包不包含 Agent 本身、模型凭据或 AgentSight。
