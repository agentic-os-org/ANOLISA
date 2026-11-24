# CMake 与 Ninja 构建进度回归语料

[English](README.md)

本语料用于验证基于 ANOLISA `6fc582a4ecd4244707e2e14597d9665b5e09db9c`（Tokenless `0.9.6`）的 CMake Unix Makefiles 与 Ninja BuildLog 改动，属于源码变更验证证据，不代表发布版本或真实 Agent 验收。`provenance.json` 记录了固定的项目提交、不可变 Docker 镜像 ID（`tokenless-cpp-capture:ubuntu24.04`，`sha256:0d0b8bc917a614456fd58ff2c71dd1c6d036472c6ef9de6aa503ad70ae64dd46`）、CMake/Ninja/Make/GCC 版本、脱敏容器命令、退出码与 SHA-256 摘要。全部八个样本均为完整未修改的 `observed.txt` 原始流。

## 重新采集

从 `src/tokenless` 目录执行，需准备已构建好的 `tokenless-cpp-capture:ubuntu24.04` 工具链镜像（Ubuntu 24.04，内含 `cmake` 3.28.3、`ninja` 1.11.1、`GNU Make` 4.3 与 GCC 13.3.0）以及 Python 3.11 或更新版本。每次运行在隔离目录中检出未修改仓库，并在 `--network none --cpus 2`、`TZ=UTC`、`LANG=C.UTF-8` 下顺序采集 `ninja_cold`、`ninja_incremental`、`make_cold`（`-j 2`）、`make_incremental`（`-j 2`）与 `failure_unknown_target`，不改动任何 CMake 定义。离线环境下可通过 `--sources-dir <dir>` 复用位于 `<dir>/<project>` 的本地克隆仓库；若对应仓库、标签或固定提交缺失或不匹配，脚本将直接报错终止。

```bash
python3 tests/capture_ctest.py --suite build --project brotli --output /tmp/tokenless-cpp-captures/brotli
python3 tests/capture_ctest.py --suite build --project catch2 --output /tmp/tokenless-cpp-captures/catch2
python3 tests/capture_ctest.py --suite build --project fmt --output /tmp/tokenless-cpp-captures/fmt
```

`brotli_unknown_target_failure.txt` 保留退出码 `1` 的完整失败输出（`ninja: error: unknown target 'tokenless_nonexistent_target'`）；`fmt_ninja_cold.txt` 与 `fmt_make_cold.txt` 原位逐字保留全部 5 行真实 GCC `fmt-c.h` ABI `note:` 诊断，并仅对前后构建进度块执行无损压紧；未将任何失败结果记为成功运行。脚本会自动对 JSON 元数据脱敏（记录 `src=/work-host/work`，省略宿主机路径与代理地址）。

## 验证保真与收益

```bash
cargo test --locked -p tokenless-compressors --test cmake_build
cargo test --locked -p tokenless-compressors --lib build_log::cmake_build -- --nocapture
CPP_CAPTURE_ROOT=/tmp/tokenless-cpp-captures cargo test --locked -p tokenless-compressors --test cmake_build -- --nocapture
```

可选 `CPP_CAPTURE_ROOT` 全集扫描遍历 `brotli`、`catch2`、`fmt` 在 `ninja_cold`、`ninja_incremental`、`make_cold`、`make_incremental` 与 `failure_unknown_target` 下的 `stdout.txt`、`stderr.txt` 与 `observed.txt`（共 45 条流）。仓库内回归样本及 45 条完整捕获流均通过独立解码器 [literal_oracle.rs](../../../common/literal_oracle.rs) 还原（比较前仅剥离独立定义的 ANSI SGR 序列，并原样保留 Markdown 代码围栏内文本）、幂等性、零 Stash 以及字符数与估算 token 严格双降校验（`stderr.txt` 与失败/单行流保持原样不改）。单元测试同时单独检验每处编辑与安全边界。

两份已审查的 `.expected.txt` golden（`brotli_ninja_cold.expected.txt` 与 `brotli_make_cold.expected.txt`）固定了压缩输出格式。仅在有意调整输出格式后重新生成并检查 diff：

```bash
cargo test --locked -p tokenless-compressors --test cmake_build regenerate_cmake_build_goldens -- --ignored --exact
```

入库语料的 `heuristic-v1` 估算结果：

| 语料文件 | 原始 Tokens | SGR 清理后 | 候选 Tokens | 相对输入节省 |
| --- | ---: | ---: | ---: | ---: |
| `brotli_ninja_cold.txt` | 769 | 769 | 332 | 56.83% |
| `brotli_make_cold.txt` | 735 | 735 | 317 | 56.87% |
| `catch2_ninja_cold.txt` | 4,270 | 4,270 | 1,154 | 72.97% |
| `catch2_make_cold.txt` | 4,189 | 4,189 | 1,195 | 71.47% |
| `fmt_ninja_cold.txt` | 1,369 | 1,369 | 838 | 38.79% |
| `fmt_make_cold.txt` | 1,571 | 1,571 | 903 | 42.52% |
| `fmt_make_incremental.txt` | 207 | 207 | 145 | 29.95% |
| `brotli_unknown_target_failure.txt` | 15 | 15 | 15 | 0.00% |

失败样本仅检验压缩器内部对失败诊断的保真；Runtime 仍按 `Tool Error` 原样透传失败结果。手写短 `Makefile` 继续走既有 `Make` 路由，而 `Make VERBOSE=1` 与 `ninja -v` 编译命令行会由同级 `cc_compile` 无损布局通道处理并在互不重叠的行区间上与 `cmake_build` 协同组合；自定义 `[k/N]` / `[PPP%]` 计数步骤与含非 SGR 控制字符的行在 `cmake_build` 通道下原位保留原文，同时混合日志在开启 Stash 与恢复能力时，仍会在已压紧的 CMake/Ninja 块、自定义计数步骤、Markdown 围栏与非 SGR 控制行之外保留可恢复省略（包括 `Make`、`Cargo` 等评分格式以及带命令/退出码锚点的 `Generic` 省略；未命中 `CmakeBuild` 头部检测的合并配置与构建日志亦继续支持带锚点的 `Generic` 回退）。Runtime、CLI 与通用 Hook 回归覆盖了路由、元数据保留及宿主门禁保护。本轮未调用真实 Agent。
