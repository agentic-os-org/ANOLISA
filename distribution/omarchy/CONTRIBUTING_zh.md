# Omarchy 集成开发与发布

[English](CONTRIBUTING.md)

先阅读根目录的 `AGENTS.md`、`CONTRIBUTING.md`，以及 `specs/` 中的组件接入和提交规范。
集成源码和测试放在 `distribution/omarchy`；正式用户手册位于 `docs/user-guide/{en,zh}/omarchy/`。
修改共用发布流程时，保留其他组件的目标平台与校验规则。

## 本地检查

在 Linux 上使用 Python 3.11.6，与 CI 一致。开发依赖安装到虚拟环境；
用户笔记本不需要 PySide6。

```bash
python3.11 -m venv .venv-omarchy
.venv-omarchy/bin/python -m pip install -r distribution/omarchy/tests/requirements.txt
.venv-omarchy/bin/ruff check distribution/omarchy
.venv-omarchy/bin/ruff format --check distribution/omarchy
.venv-omarchy/bin/python -m pytest distribution/omarchy/tests -p no:cacheprovider
.venv-omarchy/bin/python distribution/omarchy/tests/run_qml.py
python3 .github/scripts/validate-repository-metadata.py
```

PR CI 在组件、构建 Action 或共用发布流程变化时运行 Python 和离屏 QML 测试。
离屏检查不能证明真实顶栏定位、外部点击关闭或终端焦点正常，这些需要在 Omarchy 验收。

## 构建本地候选包

构建环境需要 Linux x86_64、Rust 1.97.1、Python 3.11+、C 链接器、pkg-config，
以及 OpenSSL、libudev、libseccomp 开发包。CI 使用 Ubuntu 24.04。
从完整仓库根目录执行，并确保能访问 crates.io 和官方 GitHub Releases。

```bash
rustup toolchain install 1.97.1 --profile minimal
python3 distribution/omarchy/release.py \
  --version 0.1.3 --output /tmp/omarchy-candidate \
  --cache /tmp/omarchy-build-cache --allow-dirty
```

`--allow-dirty` 只用于未发布的本地 review。清单记录基线提交、未提交状态和选定源码的摘要。
正常 CI 要求干净工作区；正式构建还会核对 Tag、版本和 HEAD。
构建器不会覆盖已有输出目录。

`release.json` 用 SHA-256 固定官方 cosh-ng、sec-core、Tokenless 安装包及 SBOM。
AW 和两个 Provider 从当前仓库及各自 Cargo.lock 编译。Qoder 不放入安装包。
sec-core 产物只覆盖原生 Bash 代码扫描，不包括 Python 扫描器或 OS 沙箱。

构建器验证输入摘要、原生程序版本、全部文件摘要及共用 CycloneDX 图规则，全部通过才生成输出目录。
失败时不留下可发布产物；输入可使用经过校验的本地缓存。

输出四个二进制发布文件：

```text
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.sha256
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.cdx.json
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.cdx.json.sha256
```

```bash
python3 .github/actions/prebuilt-rust-common/verify-artifacts.py \
  --directory /tmp/omarchy-candidate --component omarchy --version 0.1.3 \
  --os linux --arch x86_64 --layout flat
python3 distribution/omarchy/verify_release.py \
  /tmp/omarchy-candidate/anolisa-omarchy-0.1.3-linux-x86_64.tar.gz --version 0.1.3
```

SBOM 区分上游构建输入清单与实际发货文件的摘要。
上游清单可能包含本包未携带适配器的依赖；实际文件以 `bundle.json` 为准。

## Preview、验收与发布

1. 提交源码和 CI 改动供 review。合入 PR 不会发布 Release。
2. 维护者运行 **Release Preview**，选择组件 `omarchy` 和版本 `0.1.3`。
   Preview 产物保留 14 天，不是公开 Release 附件。
3. 下载四个文件，执行 [Omarchy 验收](USER_GUIDE_zh.md#验收方法)。
   保存源码 SHA、校验值、系统信息、安装记录和结果。
4. 确认维护人和版本，补好更新记录，在评审过的提交上创建批准的 `omarchy/v0.1.3` Tag。
   发布流程会重新构建该 Tag，不会直接转用 Preview 文件。
5. 项目现有的 `release` 环境和 `RELEASE_TOKEN` 控制发布。
   新构建任务使用 `ubuntu-24.04`，需要维护者允许该 runner。
   校验成功后，在官方 Tag 下发布源码、二进制和 SBOM。
6. 验证官方下载安装路径，再更新独立的 `omarchy-pkgs` 配方，使用官方固定产物及其摘要。

提交前与维护者确认 CODEOWNERS 的兜底归属。
首次发布前，需取得维护者对 `.anolisa/component.toml` 要求的明确决定并留存记录：
实现并测试生命周期接入，或批准这个独立集成的明确例外。
在决定记录之前，生命周期注册仍是未完成的发布条件。
当前集成使用自己的安装器，没有接入 ANOLISA CLI 的发现或生命周期管理。
放入占位 contract 不会让 `anolisa install` 可用；如需支持，必须另外设计和测试实际生命周期。

新安装器不使用个人 fork 的 Release 地址。
Preview 源码包包含已提交的集成文件和根许可证；重新构建二进制时要使用完整仓库，
确保 AW 和 Provider 的 lockfile 都在。
