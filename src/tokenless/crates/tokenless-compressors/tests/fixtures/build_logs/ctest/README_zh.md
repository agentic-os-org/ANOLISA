# CTest 回归语料

[English](README.md)

本语料用于验证基于 ANOLISA `5768780b0e4887949765d53314070d7935012351`（Tokenless 0.9.5）的 CTest BuildLog 改动，属于源码变更验证证据，不代表发布版本或真实 Agent 验收。`provenance.json` 记录了固定的项目提交、不可变 Docker 镜像 ID（`tokenless-cpp-capture:ubuntu24.04`，`sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`）、CMake/CTest/Ninja/GCC 版本、脱敏容器命令、退出码与 SHA-256 摘要。全部六个样本均为完整未修改的 `observed.txt` 原始流。

## 重新采集

从 `src/tokenless` 目录执行，需准备已构建好的 `tokenless-cpp-capture:ubuntu24.04` 工具链镜像（Ubuntu 24.04，内含 `cmake` 3.28.3、`ninja` 1.11.1 与 GCC 13.3.0）以及 Python 3.11 或更新版本。每次运行在隔离目录中检出未修改仓库并使用 CMake + Ninja 构建，随后在 `--network none --cpus 2`、`TZ=UTC`、`LANG=C.UTF-8` 下顺序执行 `ctest_serial`（`ctest --test-dir /work/build-ninja --output-on-failure`）与 `ctest_parallel`（`ctest --test-dir /work/build-ninja -j 2 --output-on-failure`），不改动任何 CMake 定义。离线环境下可通过 `--sources-dir <dir>` 复用位于 `<dir>/<project>` 的本地克隆仓库；若对应仓库、标签或固定提交缺失或不匹配，脚本将直接报错终止。

```bash
python3 tests/capture_ctest.py --project brotli --output /tmp/tokenless-ctest-captures/brotli
python3 tests/capture_ctest.py --project catch2 --output /tmp/tokenless-ctest-captures/catch2
python3 tests/capture_ctest.py --project fmt --output /tmp/tokenless-ctest-captures/fmt
python3 tests/capture_ctest.py --project libgit2 --output /tmp/tokenless-ctest-captures/libgit2
```

`libgit2_offline_failure.txt` 保留退出码 `8` 的完整失败输出（包含 4 个离线失败的 `online::customcert` 测试及 `The following tests FAILED:` 汇总诊断），同时对其后继续运行的 8 个通过套件执行无 Stash 无损压紧，且未将任何失败结果记为成功运行。脚本会自动对 JSON 元数据脱敏（记录 `src=/work-host/work`，省略宿主机路径与代理地址）。

## 验证保真与收益

```bash
cargo test --locked -p tokenless-compressors --test ctest
cargo test --locked -p tokenless-compressors --lib build_log::ctest -- --nocapture
CTEST_CAPTURE_ROOT=/tmp/tokenless-ctest-captures cargo test --locked -p tokenless-compressors --test ctest -- --nocapture
```

可选 `CTEST_CAPTURE_ROOT` 全集扫描遍历 `brotli`、`catch2`、`fmt`、`libgit2` 在 `ctest_serial` 与 `ctest_parallel` 下的 `stdout.txt`、`stderr.txt` 与 `observed.txt`（共 24 条流）。仓库内回归样本及 24 条完整捕获流均通过独立解码器 [ctest_oracle.rs](../../../common/ctest_oracle.rs) 还原（比较前仅剥离独立定义的 ANSI SGR 序列，并原样保留 Markdown 代码围栏内文本）、幂等性、零 Stash 以及字符数与估算 token 严格双降校验（`stderr.txt` 保持原样不改）。单元测试同时单独检验每处编辑。

边界变异测试向合成测试块以及 `brotli_parallel.txt` 真实并发交错片段的逐行边界插入 `***Failed`、`***Timeout`、`***Exception: SegFault`、`***Not Run`、断言失败、`\r` 覆盖行、`\x1b[0K`、`\t` 缩进的 `The following tests FAILED:` 行、围栏引用示例与未知文本（单元测试 330 次合成变异 + 集成测试 216 次真实片段变异）。其余回归覆盖单行/散文拒绝识别、仅含 CRLF 拒绝识别、跨采样缺口与有效日志内部的代码围栏隔离、`UpdateCTestConfiguration` / `Site:` / `BuildName:` 前导序言、`4 KiB`（`4096` 字节）单行局部停用与 `64 KiB` 全局熔断边界、`256` 行 / `64 KiB` 分块、`256` 次编辑上限、超过 `4 KiB` 生成模板头的二次压缩幂等性、无可编辑块时锚定 `Generic` 恢复回退，以及禁止 CTest `Start`/结果行进入 `Generic` 进度模板。

两份已审查的 `.expected.txt` golden（`brotli_serial.expected.txt` 与 `brotli_parallel.expected.txt`）固定了压缩输出格式。仅在有意调整输出格式后重新生成并检查 diff：

```bash
cargo test --locked -p tokenless-compressors --test ctest regenerate_ctest_goldens -- --ignored --exact
```

入库语料的 heuristic-v1 估算结果：

| 语料文件 | 原始 Tokens | SGR 清理后 | 候选 Tokens | 相对输入节省 |
| --- | ---: | ---: | ---: | ---: |
| `brotli_serial.txt` | 2,984 | 2,984 | 680 | 77.21% |
| `brotli_parallel.txt` | 2,984 | 2,984 | 773 | 74.10% |
| `catch2_serial.txt` | 3,164 | 3,164 | 916 | 71.05% |
| `catch2_parallel.txt` | 3,164 | 3,164 | 942 | 70.23% |
| `fmt_serial.txt` | 572 | 572 | 217 | 62.06% |
| `libgit2_offline_failure.txt` | 5,344 | 5,344 | 5,245 | 1.85% |

失败样本仅检验压缩器内部对失败诊断、状态行与摘要尾注的保真；Runtime 仍按 `Tool Error` 原样透传失败结果。Runtime、CLI 与通用 Hook 回归覆盖了路由、元数据保留及宿主门禁保护。本轮未调用真实 Agent。
