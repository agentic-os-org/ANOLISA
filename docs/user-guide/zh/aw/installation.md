# 安装与首次运行

[English](../../en/aw/installation.md)

需要 Linux，以及已登录的 Qoder CLI 1.1.64 或更新版本。
其他 Agent 见[配置与 Agent 启动](configuration.md)。

## 获取安装包

AW 当前使用 Preview 包，尚未接入 ANOLISA CLI 或 RPM。
从[通过的 AW Integration 构建](https://github.com/agentic-os-org/ANOLISA/actions/workflows/aw-preview.yml)
下载 `aw-preview-linux-x86_64-…` artifact，解压 ZIP 后得到安装包和 `SHA256SUMS`。
CI 包面向 Linux x86_64，要求 glibc 2.39 或更新版本，例如 Ubuntu 24.04。

需要自行编译或使用 Linux aarch64 时，按[源码构建](../../../../contribute/zh/aw/building.md)
生成安装包，再继续本页。构建过程需要联网下载依赖。

| 安装包 | 内容 |
| --- | --- |
| `aw-core` | AW 启动与配置工具 |
| `aw-provider-tokenless` | Tokenless 与其所需 RTK，需先安装 core |
| `aw-provider-sec-core` | 安全 Provider 和 sec-core 程序，需先安装 core |
| `aw-all-in-one` | 上述三个组件 |

分包必须来自同一次构建。选择 all-in-one 时直接安装到新目录，无需再安装分包。

## 安装

在解压后的 artifact 目录执行。以下版本应与下载包名一致；首次体验使用 core，
需要全部能力时将 `AW_BUNDLE_KIND` 改为 `aw-all-in-one`。

```bash
sha256sum -c SHA256SUMS
export AW_VERSION=0.1.0-preview.1
export AW_BUNDLE_KIND=aw-core
export AW_BUNDLE="$AW_BUNDLE_KIND-$AW_VERSION-linux-$(uname -m)"
export AW_PREFIX="/opt/aw-preview/$AW_VERSION"
tar -xzf "$AW_BUNDLE.tar.gz"
sudo install -d -m 755 /opt/aw-preview
sudo "./$AW_BUNDLE/aw" install --bundle "$PWD/$AW_BUNDLE" --prefix "$AW_PREFIX" --scope system
export PATH="$AW_PREFIX/bin:$PATH"
aw --help
```

校验结果应全部为 `OK`，安装完成会报告已安装的软件包。
安装器拒绝覆盖已有组件；升级请使用[新安装目录](operations.md)。
后续终端也需要设置 `AW_PREFIX` 和 `PATH`，可以写入自己的 shell 配置。

个人安装可将 `AW_PREFIX` 改为自己拥有的绝对路径，去掉 `sudo` 并使用 `--scope user`。
core 与扩展保持同一 scope。包检查和恢复见[运行维护](operations.md)。

## 创建配置

以普通用户创建配置，数据保存在安装目录之外。

```bash
export AW_HOME="$HOME/aw"
export AW_QODER="$(command -v qodercli)"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
qodercli --version
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/aw.yaml"
```

`AW_QODER` 应为真实可执行文件的绝对路径。`config init` 创建完整配置，不覆盖已有文件。
安全与压缩需另行启用，见 [sec-core](security.md)和 [Tokenless](tokenless.md)。

## 启动并验证

```bash
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

应看到 AW 实例信息并进入 Qoder。请它用工具读取当前目录，按原生提示处理权限确认。
确认工具能返回结果后退出 Qoder，在同一终端执行。

```bash
aw status --config "$AW_HOME/aw.yaml"
aw stop --config "$AW_HOME/aw.yaml"
```

启用 Provider 后，按对应页面验证效果。
