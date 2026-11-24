# C/C++ 编译命令行回归语料

[English](README.md)

本语料用于验证基于 ANOLISA `0172542d25442d94f01c00e72c82a79aec2eab46`（Tokenless `0.9.7`）的 C/C++ 编译命令行（`cc_compile`）Stage 1 BuildLog 改动，属于源码变更验证证据，不代表发布版本或真实 Agent 验收。`provenance.json` 记录了固定的项目提交、不可变 Docker 镜像 ID（`tokenless-cpp-capture:ubuntu24.04`，`sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`）、CMake/Ninja/Make/GCC/Clang 版本、脱敏容器命令、退出码与 SHA-256 摘要。全部八个样本均为完整未修改的 `observed.txt` 原始流。

## 重新采集

从 `src/tokenless` 目录执行，需准备已构建好的 `tokenless-cpp-capture:ubuntu24.04` 工具链镜像（Ubuntu 24.04，内含 `cmake` 3.28.3、`ninja` 1.11.1、`GNU Make` 4.3、GCC 13.3.0 与 Clang 18.1.3）以及 Python 3.11 或更新版本。每次运行在隔离目录中检出未修改仓库，并在 `--network none --cpus 2`、`TZ=UTC`、`LANG=C.UTF-8` 下执行构建采集，不改动任何构建定义。离线环境下可通过 `--sources-dir <dir>` 复用位于 `<dir>/<project>` 的本地克隆仓库；若对应仓库、标签或固定提交缺失或不匹配，脚本将直接报错终止。

```bash
python3 tests/capture_ctest.py --suite verbose --project brotli --output /tmp/tokenless-cpp-captures/brotli
python3 tests/capture_ctest.py --suite verbose --project catch2 --output /tmp/tokenless-cpp-captures/catch2
python3 tests/capture_ctest.py --suite verbose --project fmt --output /tmp/tokenless-cpp-captures/fmt
python3 tests/capture_ctest.py --suite verbose --project zstd --output /tmp/tokenless-cpp-captures/zstd
```

`brotli_clang_ninja_verbose_failure.txt` 原位逐字保留退出码 `1` 的 `-Weverything -Werror` 编译失败输出；`fmt_ninja_verbose.txt` 原位逐字保留全部 5 行真实 GCC `fmt-c.h` ABI `note:` 诊断，并仅对前后编译命令行执行无损压紧；未将任何失败结果记为成功运行。脚本会自动对 JSON 元数据脱敏（记录 `src=/work-host/work`，省略宿主机路径与代理地址）。

## 验证保真与收益

```bash
cargo test --locked -p tokenless-compressors --test cc_compile
cargo test --locked -p tokenless-compressors --lib build_log::cc_compile -- --nocapture
CPP_VERBOSE_CAPTURE_ROOT=/tmp/tokenless-cpp-captures cargo test --locked -p tokenless-compressors --test cc_compile -- --nocapture
```

可选 `CPP_VERBOSE_CAPTURE_ROOT` 全集扫描遍历 `captures/` 与 `slice_c_verbose/` 下共计 16 组完整 CMake（`VERBOSE=1`、`ninja -v`）与原生 GNU Make（`V=1`）GCC/Clang 运行的 `stdout.txt`、`stderr.txt` 与 `observed.txt`（共 48 条流）。仓库内回归样本及 48 条完整捕获流均通过独立解码器 [literal_oracle.rs](../../../common/literal_oracle.rs) 还原（比较前仅剥离独立定义的 ANSI SGR 序列，并原样保留 Markdown 代码围栏内文本）、幂等性、零 Stash 以及字符数与估算 token 严格双降校验（`stderr.txt` 与未压紧失败流保持原样不改）。单元测试同时单独检验每处编辑与安全边界。

两份已审查的 `.expected.txt` golden（`brotli_make_verbose.expected.txt` 与 `brotli_ninja_verbose.expected.txt`）固定了压缩输出格式。仅在有意调整输出格式后重新生成并检查 diff：

```bash
cargo test --locked -p tokenless-compressors --test cc_compile regenerate_cc_compile_goldens -- --ignored --exact
```

入库语料的 `heuristic-v1` 估算结果：

| 语料文件 | 原始 Tokens | SGR 清理后 | 候选 Tokens | 相对输入节省 |
| --- | ---: | ---: | ---: | ---: |
| `brotli_make_verbose.txt` | 4,950 | 4,950 | 2,604 | 47.39% |
| `brotli_ninja_verbose.txt` | 3,585 | 3,585 | 1,610 | 55.09% |
| `brotli_clang_make_verbose.txt` | 5,041 | 5,041 | 2,747 | 45.51% |
| `brotli_clang_ninja_verbose_failure.txt` | 732 | 732 | 732 | 0.00% |
| `catch2_make_verbose.txt` | 46,894 | 46,894 | 11,148 | 76.23% |
| `fmt_ninja_verbose.txt` | 6,109 | 6,109 | 3,457 | 43.41% |
| `zstd_make_lib_v1_gcc.txt` | 5,432 | 5,432 | 1,531 | 71.82% |
| `zstd_make_lib_v1_clang.txt` | 5,619 | 5,619 | 1,542 | 72.56% |

失败样本仅检验压缩器内部对失败诊断的保真；Runtime 仍按 `Tool Error` 原样透传失败结果。手写短 `Makefile` 继续走既有 `Make` 路由，非冗长 CMake/Ninja 进度继续由 `cmake_build` 处理，而同时包含进度与冗长 `-c` 编译命令行的混合日志会在互不重叠的行区间上协同应用两套 Stage 1 无损通道；自定义计数步骤、含 shell 元字符命令、全部采用 CRLF 换行的输出与含非 SGR 控制字符的行在 `cc_compile` 通道下原位保留原文，同时混合日志在开启 Stash 与恢复能力时，仍会在已压紧的编译块、自定义步骤、Markdown 围栏与非 SGR 控制行之外保留可恢复省略（包括 `Make`、`Cargo` 等评分格式以及带命令/退出码锚点的 `Generic` 省略）。Runtime、CLI 与通用 Hook 回归覆盖了路由、元数据保留及宿主门禁保护。本轮未调用真实 Agent。
