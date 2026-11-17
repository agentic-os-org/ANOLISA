# Gradle 回归语料

[English](README.md)

这些样例用于在 ANOLISA `442d70487`（Tokenless 0.9.5）基础上验证 Gradle
BuildLog 压缩的变更。它们是源码变更证据，不是发布版本或线上 Agent
验证记录。`provenance.json` 记录了锁定的项目 tag、Gradle Wrapper 版本、
JDK、宿主机平台、命令、退出状态与原始 SHA-256 值。每个 fixture 都是捕获
流的 observed stdout+stderr 拼接（`observed.txt`），与 `capture.json` 中
的哈希逐字节一致。

## 采集环境

已提交的 fixture 来自 mockito/mockito v5.24.0，使用其锁定的 Gradle Wrapper
8.14.2 与 Eclipse Temurin JDK 21.0.13+9，在 Linux x86-64 宿主机上构建。
由于容器镜像仓库的 blob 重定向在本网络被阻断，捕获以原生方式完成（未用
容器）；依赖与 Wrapper 发行包下载通过写入 `gradle.properties` 的站点
HTTP 代理完成（Java 不读取 `http_proxy` 环境变量）。构建脚本未做任何
修改，未使用 `-q`、`--console=plain` 或 `-x`。

可选的 `GRADLE_CAPTURE_ROOT` 扫描还覆盖 caffeine 3.3.1（Gradle Wrapper
9.8.1）与 resilience4j 2.4.0（Gradle Wrapper 8.10）的本地捕获；它们的
Wrapper 下载进度行、重试退避提示与 `Welcome to Gradle` 横幅条目共同塑造了
识别前言。其中一次 caffeine 采集因宿主环境原因（Pax Exam 的 bundle 锁）
OSGi 测试失败；任何失败结果都不会被表述为成功构建。

## 复现采集

在新的输出目录中，使用相同的 JDK，并让 `GRADLE_USER_HOME` 下的
`gradle.properties` 携带代理配置：

```bash
git clone --branch v5.24.0 https://github.com/mockito/mockito.git source
cd source
./gradlew build                          # cold
./gradlew build                          # incremental
./gradlew clean build                    # clean_rebuild
./gradlew build buid                     # failure_unknown_task（拼错的任务名）
```

`mockito_cold.txt`、`mockito_incremental.txt`、`mockito_clean_rebuild.txt`
与 `mockito_unknown_task.txt` 分别对应上述四种模式。由于 javadoc 任务在
该 JDK 下失败，四种模式均以非零状态退出——失败记录及其诊断被原样保留，
恰好覆盖 Gradle 压缩必须保证的 FAILED 邻接不变量。任何失败结果都不会被
表述为成功构建。

## 验证保留与节省

```bash
cargo test --locked -p tokenless-compressors --test gradle
GRADLE_CAPTURE_ROOT=/tmp cargo test --locked -p tokenless-compressors --test gradle captured_streams -- --nocapture
```

可选的根目录扫描会读取四种模式下 `cold`、`incremental`、`clean_rebuild`
与 `failure_unknown_task` 的 stdout/stderr/observed 文件。所有提交的
fixture 与捕获流均通过独立完整重建、幂等与零 Stash 检查。
`tests/common/gradle_oracle.rs` 中的解码器不调用生产解析器。observed 流
的启发式估算（heuristic-v1）：cold 5,995 → 4,899 tokens（18.3%），
incremental 8,165 → 6,237（23.6%），clean rebuild 7,097 → 7,063，
unknown-task 失败 410 → 377。

警告、测试输出、堆栈跟踪与仅含 SGR 的记录永远不会进入模板块。已评审的
golden `mockito_incremental.expected.txt` 冻结了 incremental 捕获的紧凑
表示；仅在有意变更输出格式后重新生成：

```bash
cargo test --locked -p tokenless-compressors --test gradle regenerate_gradle_goldens -- --ignored --exact
```
