# AW 归档组包

[English](README.md)

通过普通 Make 目标与脚本构建独立的 AW core 和 Provider 归档。后端程序由各组件
独立交付；AW 负责 Provider 的配置和运行。

使用干净的 Linux 工作树、Rust 1.97.1 和 Python 3.9 或更新版本。显式选择产物，
输出目录必须为绝对路径且为空：

```bash
make -C distribution aw-packages VERSION=0.2.2 \
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
├── integration.py                # 构建 sec-core 和 Tokenless
├── templates/                    # 使用显式程序路径的策略数据
└── tests/                        # 构建选择、归档及安装夹具
```

core 交付 `bin/aw`、普通安装 Makefile 和许可证／来源文档，使用
`make PREFIX=... AWPRJ=... DESTDIR=... install` 安装。Provider 归档包含
`package.json`、`payload/` 中的桥接程序和自身资源，以及策略模板。可选的本地
Provider 安装使用已经装好的 AW CLI；Provider 归档不包含后端可执行文件。

模板的 `PROVIDERS_DIR` 是实际包含 `sec-core`、`tokenless` Provider 目录的根。
按所选策略显式传递后端路径 `SEC_CORE_CLI`、`TOKENLESS_CLI` 和 `SOCKET`，因此
支持 `AWPRJ` 命名空间及不同的 core／Provider 前缀。

原生集成验收通过单独入口准备后端，普通组包不调用它：

```bash
make -C distribution aw-integration-backends BACKENDS='sec-core tokenless'
```

参见[源码构建指南](../../../contribute/zh/aw/building.md)和
[安装指南](../../../docs/user-guide/zh/aw/installation.md)。

## 匹配候选 SDK

从同一完整 Git commit 构建核心与可选框架适配 npm 和 wheel 制品：`@anolisa/aw-sdk`、
`@anolisa/aw-sdk-qoder`、`@anolisa/aw-sdk-pi`、`@anolisa/aw-sdk-deepseek-harness`、`anolisa-aw-sdk`、
`anolisa-aw-sdk-claude` 和 `anolisa-aw-sdk-qwenpaw`。构建器使用 Git archive，输入不受本地
修改影响，并显式拒绝任何混入核心包内的框架私有桥接代码。需要 Node 22+、Python 3.11+，并已安装 pip、setuptools 和 wheel；SDK
构建离线执行。收据对七包布局使用 `aw-sdk-candidate/v3` schema，并记录 node、npm、Python、pip、
setuptools 和 wheel 的版本；历史双包 `aw-sdk-candidate/v1` 收据需使用对应的拆包前候选版本
脚本核验。核验时使用收据中的解释器和构建工具版本；校验会比较包文件名与内容，工具版本
是否相同由操作者确认。输出目录必须为空。制品与收据先暂存再发布；失败或取消时
回滚本次构建已发布的文件，输出目录可直接重试。

```bash
python3 distribution/packaging/aw/sdk_candidate.py build \
  --repo "$PWD" --candidate <full-commit-sha> --output "$PWD/target/sdk-candidate"
python3 distribution/packaging/aw/sdk_candidate.py verify \
  --repo "$PWD" --candidate <full-commit-sha> \
  --receipt "$PWD/target/sdk-candidate/sdk-candidate.json"
```

核验时重新构建候选，检查制品文件名并逐项比较 SDK 内的文件，包括原生桥接资源，
忽略归档时间戳。
仅修改收据中的 commit 和校验和无法让旧包通过内容比较。
追加三个 `--rpm /absolute/path/package.rpm` 参数可检查 AW 和两个 Provider RPM，
需要 rpm、rpm2cpio、cpio 和 Bash。包内 `source.json` 通过 `source_commit`（归档格式）
或 `commit`（原生 RPM 格式）声明候选；两个字段同时出现时必须一致。三个 RPM 的
版本、release、架构必须一致。RPM 来源记录属于声明，需另存受控断网构建日志及源码、vendor 摘要。
本检查不证明二进制可复现、安装行为、原生采用或业务性能；交付前在实际候选上完成
对应验收，旧包保留为明确标注的历史产物。
