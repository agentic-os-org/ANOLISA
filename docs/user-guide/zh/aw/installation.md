# 安装

[English](../../en/aw/installation.md)

在 Linux 上安装 AW，以可选 Provider 策略启动已有 Agent。目前尚未提供
`anolisa install aw` 或公开 AW RPM 渠道。Agent 应用、模型凭据和后端组件单独安装。

使用 Qoder 时，需要已登录、支持原生命令 Hook 的 CLI；已验证基线为 1.1.64。
其他 Agent 见[配置与 Agent 启动](configuration.md)。

## 获取安装包

使用目标源码提交中[已成功的 AW Integration run](https://github.com/agentic-os-org/ANOLISA/actions/workflows/aw-integration.yml) 的产物，并核对配套
`SHA256SUMS`。校验和用于发现损坏，不是发布者签名。开发交付入口见
[源码构建](../../../../contribute/zh/aw/building.md)。

CI 归档面向 Linux x86_64，要求 glibc 2.39 或更高版本，例如 Ubuntu 24.04。
Linux aarch64 或其他构建环境请按源码构建指南交付。

| 归档 | 内容 |
| --- | --- |
| `aw-core` | AW 命令行工具 |
| `aw-provider-sec-core` | 工具安全检查 Provider |
| `aw-provider-tokenless` | 工具输出压缩与命令重写 Provider |

Provider 归档只包含桥接程序。Tokenless CLI 与 sec-core CLI/daemon 由后端
owner 的发行件或原生 core 包安装。

## 安装

在下载产物的目录中，以当前用户安装预构建 AW CLI：

```bash
export AW_VERSION=0.1.0
export AW_BUNDLE_KIND=aw-core
export AW_BUNDLE="$AW_BUNDLE_KIND-$AW_VERSION-linux-$(uname -m)"
sha256sum -c SHA256SUMS
tar -xzf "$AW_BUNDLE.tar.gz"
export AW_BUNDLE_DIR="$PWD/$AW_BUNDLE"
export AW_PREFIX="$HOME/.local"
make -C "$AW_BUNDLE_DIR" install PREFIX="$AW_PREFIX"
export PATH="$AW_PREFIX/bin:$PATH"
aw --version
```

所有校验和应报告 `OK`，`aw --version` 应显示已安装版本。

可修改 `AW_PREFIX` 选择安装目录。系统安装可由管理员执行
`sudo make -C "$AW_BUNDLE_DIR" install PREFIX=/usr/local`。

需要本地 Provider 扩展时，安装到独立的扩展 prefix：

```bash
export AW_PROVIDER_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_PROVIDER_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_PROVIDER_BUNDLE"
aw list --installed --prefix "$AW_EXTENSION_PREFIX"
```

需要安全检查时，使用相同方式安装独立 sec-core 归档。

这些扩展命令使用 user scope。root 管理的扩展写入需要 `sudo` 和显式
`--scope system`。每个扩展 prefix 的 scope 应保持一致；单独使用 `sudo` 不选择
scope。`aw install` 用于安装本地 Provider bundle。

由 RPM 或其他部署管理者安装的 Provider 也可直接通过绝对 `transport.argv`
接入。可选 raw 扩展管理见
[Provider bundle](../../../developer-guide/zh/aw/reference/packages.md)。

<a id="创建配置"></a>
<a id="启动并验证"></a>

## 配置并开始使用

接着按[快速入门](index.md#2-生成配置)生成首次配置、启动 Qoder、验证工具调用并停止服务。
首次运行使用空策略，只需安装 AW core。

启用 Provider 时，使用其扩展 prefix，并按 [Tokenless](tokenless.md) 或
[安全检查](security.md)提供显式模板与后端路径。
其他 Agent 入口见[配置与 Agent 启动](configuration.md)。

完整子命令、参数、默认值及使用约束见 [CLI 命令参考](cli-reference.md)。
