# 从源码构建 AW

[English](../../en/aw/building.md)

在 Linux x86_64 或 aarch64 上构建 AW 和 Provider 桥接程序。预构建归档见
[安装 AW](../../../docs/user-guide/zh/aw/installation.md)。

## 构建环境

安装 [rustup](https://www.rust-lang.org/tools/install)、各 workspace 的
`rust-toolchain.toml` 固定的 Rust 工具链、Python 3.11+，并为 AW 检查准备 Node.js。
Ubuntu / Debian 编译依赖如下：

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config libssl-dev python3
```

未缓存的 Cargo 依赖需要联网下载。构建环境须匹配目标架构和兼容的 glibc。
后端组件有各自的构建要求，AW 与 Provider 目标不构建后端。

## 构建和安装组件

从仓库根目录按需构建：

```bash
make -C src/aw build
make -C providers/agent-sec-core build
make -C providers/tokenless build
```

每个目标只从自己的锁定 Cargo workspace 构建对应 release 二进制。
`CARGO_TARGET_DIR` 选择产物目录，默认是各组件自己的 `target/`。
安装只消费预构建文件，不重新编译：

```bash
make -C src/aw install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
make -C providers/agent-sec-core install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
make -C providers/tokenless install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
```

`DESTDIR` 增加临时根目录；省略后按调用者权限直接安装到 `PREFIX`。目标不会自动
调用 `sudo`、创建 AW 包记录、注册 Hook 或启动服务。可通过 `BINARY` 选择已构建程序。

| 组件 | `PREFIX` 下的文件 |
| --- | --- |
| AW | `bin/aw`；`share/doc/aw/` |
| sec-core Provider | `libexec/aw/providers/sec-core/aw-provider-sec-core`；`share/aw/providers/sec-core/{schemas,examples}/`；`share/doc/aw-provider-sec-core/` |
| Tokenless Provider | `libexec/aw/providers/tokenless/aw-provider-tokenless`；`share/aw/providers/tokenless/examples/`；`share/doc/aw-provider-tokenless/` |

`AWPRJ` 默认为空。例如 `AWPRJ=anolisa` 会在私有程序和资源路径的 `aw/` 前、
文档路径的 `doc/` 前增加该命名空间，不改变 `bin/aw`。可按需覆盖 `BINDIR`、
`LIBEXECDIR`、`DATADIR`、`DOCDIR`。运行配置须显式指定最终 Provider 程序路径，
安装路径不会自动被发现。

## 构建独立归档

使用干净源码工作树和空的绝对输出目录：

```bash
make -C distribution aw-packages VERSION=0.1.0 COMPONENTS='core sec-core tokenless' \
  OUTPUT="$PWD/target/aw-packages"
cd target/aw-packages
sha256sum -c SHA256SUMS
```

`COMPONENTS` 显式列表选择独立归档：

| 条目 | 归档 | 内容 |
| --- | --- | --- |
| `core` | `aw-core-0.1.0-linux-<arch>.tar.gz` | `bin/aw`、普通安装 Makefile、来源和许可证文档 |
| `sec-core` | `aw-provider-sec-core-0.1.0-linux-<arch>.tar.gz` | Provider `package.json`、桥接 payload、schema、示例和配置模板 |
| `tokenless` | `aw-provider-tokenless-0.1.0-linux-<arch>.tar.gz` | Provider `package.json`、桥接 payload、示例和配置模板 |

选择一个或多个条目，不使用 `all` 或组合归档。`VERSION` 标记产物版本，不选择
源码 tag。`distribution/packaging/aw/` 负责归档装配与具体模板。后端程序由自己的
core 包交付；原生验收使用独立 `aw-integration-backends` 目标构建所选后端，
这些文件不进入 AW 归档。

## 验证安装

在 Linux 上执行 `python3 src/aw/scripts/check.py`，完成普通离线契约检查。
再按[安装](../../../docs/user-guide/zh/aw/installation.md)配置已安装的 Agent 并启动。
安装后端 core 组件后，分别验证[安全检查](../../../docs/user-guide/zh/aw/security.md)
与 [Tokenless](../../../docs/user-guide/zh/aw/tokenless.md) 的真实效果。
只安装 Provider 文件不会启用它。

其他组件使用[仓库构建指南](../../../docs/user-guide/zh/getting-started/building.md)。
