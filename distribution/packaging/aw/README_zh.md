# AW 归档组包

[English](README.md)

通过普通 Make 目标与脚本构建独立的 AW core 和 Provider 归档。后端程序由各组件
独立交付；AW 负责 Provider 的配置和运行。

使用干净的 Linux 工作树、Rust 1.97.1 和 Python 3.9 或更新版本。显式选择产物，
输出目录必须为绝对路径且为空：

```bash
make -C distribution aw-packages VERSION=0.1.0 \
  COMPONENTS='core sec-core tokenless' OUTPUT="$PWD/target/aw-packages"
cd target/aw-packages
sha256sum -c SHA256SUMS
```

`COMPONENTS` 接受空格分隔的 `core`、`sec-core`、`tokenless`，每个选择独立生成
一个归档。`VERSION` 标记产物版本，不选择源码修订。装配过程检查工作树始终对应
同一个未变更的 Git commit，并记录源码身份。

```text
aw/
├── build.sh, build.py             # 编译选定的 AW/Provider 工作区
├── package.py                    # 装配独立归档和 SHA256SUMS
├── common.py                     # 有界子进程、取消与复制
├── core.Makefile                 # 归档安装入口，复用 AW install.mk
├── build-integration.sh          # 验收时显式构建后端的入口
├── integration.py                # 构建 sec-core、Tokenless 和固定版本 RTK
├── templates/                    # 使用显式程序路径的策略数据
└── tests/                        # 构建选择、归档及安装夹具
```

core 交付 `bin/aw`、普通安装 Makefile 和许可证／来源文档，使用
`make PREFIX=... AWPRJ=... DESTDIR=... install` 安装。Provider 归档包含
`package.json`、`payload/` 中的桥接程序和自身资源，以及策略模板。可选的本地
Provider 安装使用已经装好的 AW CLI；Provider 归档不包含后端可执行文件。

模板的 `PROVIDERS_DIR` 是实际包含 `sec-core`、`tokenless` Provider 目录的根。
按所选策略显式传递后端路径 `SEC_CORE_CLI`、`TOKENLESS_CLI`、`RTK_BINARY`、
`RTK_SHA256` 和 `SOCKET`，因此支持 `AWPRJ` 命名空间及不同的 core／Provider 前缀。

原生集成验收通过单独入口准备后端，普通组包不调用它：

```bash
make -C distribution aw-integration-backends BACKENDS='sec-core tokenless'
```

参见[源码构建指南](../../../contribute/zh/aw/building.md)和
[安装指南](../../../docs/user-guide/zh/aw/installation.md)。
