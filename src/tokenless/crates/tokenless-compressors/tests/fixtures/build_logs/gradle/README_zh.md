# Gradle 回归语料

[English](README.md)

本语料用于验证基于 ANOLISA `81e8476d4ee2a5370583fd65d48dc6a7f1f00db0`（Tokenless 0.9.5）的 Gradle BuildLog 改动，属于源码变更验证证据，不代表发布版本或真实 Agent 验收。`provenance.json` 记录了固定的项目提交、不可变 Docker 镜像 ID、Gradle/JDK 版本、脱敏容器命令、退出码与 SHA-256 摘要。完整样本为未修改原始流；`mockito_cold_excerpt.txt` 为 `mockito` 冷构建 `stdout` 的未修改连续行切片（第 `1`–`789` 行，仅省略末尾含 Develocity Build Scan 发布链接的第 `790`–`793` 行）。

## 重新采集

从 `src/tokenless` 目录执行，需准备 Docker 与 Python 3.11 或更新版本。每次运行在 `--output` 创建新的项目目录；未修改的仓库检出与隔离 Gradle 缓存均局限在该目录内。携带 `--failure-probe` 时，脚本以 `--cpus 2`、`TZ=UTC`、`LANG=C.UTF-8` 顺序运行 `cold`、`incremental`、`clean_rebuild` 与 `failure_unknown_task`（`buid`），不改动构建脚本，也不注入 `-q`、`--console=plain` 或 `-x test`。`caffeine` 将目标收敛至 `:simulator:build`，以避开耗时数小时的并发模型检查（`lincheckTest` / `frayTest`），同时保留插件构建、编译、静态检查、配置缓存与工具链解析覆盖。

```bash
docker pull eclipse-temurin:21-jdk
python3 tests/capture_gradle.py --project mockito --failure-probe --output /tmp/tokenless-gradle-captures/mockito
python3 tests/capture_gradle.py --project caffeine --failure-probe --output /tmp/tokenless-gradle-captures/caffeine
python3 tests/capture_gradle.py --project resilience4j --failure-probe --output /tmp/tokenless-gradle-captures/resilience4j
```

`caffeine_failure.txt` 保留退出码 `1` 的完整失败输出，未将失败结果记为成功构建。脚本会自动对 JSON 元数据脱敏（记录 `src=/work-host/work`，省略宿主机 `uname` 与代理地址）。

## 验证保真与收益

```bash
cargo test --locked -p tokenless-compressors --test gradle
cargo test --locked -p tokenless-compressors build_log::gradle -- --nocapture
GRADLE_CAPTURE_ROOT=/tmp/tokenless-gradle-captures cargo test --locked -p tokenless-compressors --test gradle -- --nocapture
```

可选 `GRADLE_CAPTURE_ROOT` 全集扫描遍历 `mockito`、`caffeine`、`resilience4j` 四个轮次的 `stdout.txt`、`stderr.txt` 与 `observed.txt`（共 36 条流）。仓库内回归样本及 36 条完整捕获流均通过独立解码器 [gradle_oracle.rs](../../../common/gradle_oracle.rs) 还原（比较前仅剥离独立定义的 ANSI SGR 序列，并原样保留 Markdown 代码围栏内文本）、幂等性、零 Stash 以及字符数与估算 token 严格双降校验。单元测试同时单独检验每处编辑。

边界变异测试向 `mockito_cold_excerpt.txt` 真实任务块及合成任务块的逐行边界插入告警、Kotlin `w:` 提示、Java 异常堆栈、`> Task :... FAILED`、`\r`、`\x1b[0K`、`\t` 与未知文本（单元测试 310 次合成变异 + 集成测试 232 次真实片段变异）。其余回归覆盖仅含 CRLF 拒绝识别、跨采样缺口与有效构建日志内部的代码围栏隔离（原样保留围栏内任务示例）、`exit code:` / `exit status:` 及 `> Configure project :` 头部锚定配自定义配置输出、`4 KiB`（`4096` 字节）单行局部停用与 `64 KiB` 全局熔断边界、`256` 行 / `64 KiB` 分块、`256` 次编辑上限、超过 `4 KiB` 生成模板头的二次压缩幂等性、无可编辑块时锚定 `Generic` 恢复回退，以及禁止 `> Task :...` 进入 `Generic` 进度模板。

两份已审查的 `.expected.txt` golden 固定了压缩输出格式。仅在有意调整输出格式后重新生成并检查 diff：

```bash
cargo test --locked -p tokenless-compressors --test gradle regenerate_gradle_goldens -- --ignored --exact
```

入库语料的 heuristic-v1 估算结果：

| 语料文件 | 原始 Tokens | SGR 清理后 | 候选 Tokens | 相对输入节省 |
| --- | ---: | ---: | ---: | ---: |
| `caffeine_cold.txt` | 953 | 953 | 899 | 5.67% |
| `caffeine_incremental.txt` | 795 | 795 | 632 | 20.50% |
| `caffeine_failure.txt` | 511 | 511 | 483 | 5.48% |
| `resilience4j_incremental.txt` | 5,266 | 5,266 | 1,963 | 62.72% |
| `resilience4j_clean_rebuild.txt` | 5,331 | 5,331 | 2,096 | 60.68% |
| `mockito_cold_excerpt.txt` | 10,243 | 9,852 | 7,865 | 23.22% |

失败样本仅检验压缩器内部保真；Runtime 仍按 `Tool Error` 原样透传失败结果。Runtime、CLI 与通用 Hook 回归覆盖了路由、元数据保留及宿主门禁保护。本轮未调用真实 Agent。
