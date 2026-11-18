# Maven 回归语料

[English](README.md)

本语料验证基于 ANOLISA `442d70487`（Tokenless 0.9.5）的 Maven BuildLog 改动，属于源码变更证据，
不代表发布版本或真实 Agent 验证。`provenance.json` 记录项目固定提交、不可变 Docker
镜像 ID、Maven/JDK 版本、命令、退出状态及原始 SHA-256。完整 fixture 为父进程观测的
stdout/stderr 分块混合流，其顺序不保证 JVM 跨流时序。`downloads_batch.txt` 是未经修改的
连续 stdout 片段，元数据注明了源行范围。

## 重新采集

从 `src/tokenless` 执行，需要 Docker 和 Python 3.11 或更新版本。每个输出目录必须尚不存在；
项目和独立依赖缓存仅创建在该目录内。脚本对未修改的项目执行两次 `clean package`，首次使用
空缓存，第二次复用该缓存。不修改依赖定义、不关闭测试。默认和 batch 模式分别使用独立缓存
并独立测量。

```bash
docker pull maven:3.9.9-eclipse-temurin-21
docker pull maven:3.9.9-eclipse-temurin-17
python3 tests/capture_maven.py --project commons-cli --output /tmp/tokenless-maven-9966-commons
python3 tests/capture_maven.py --project slf4j --output /tmp/tokenless-maven-9966-slf4j
python3 tests/capture_maven.py --project gson --output /tmp/tokenless-maven-9966-gson
python3 tests/capture_maven.py --project jsoup --output /tmp/tokenless-maven-9966-jsoup-default
python3 tests/capture_maven.py --project jsoup --batch --output /tmp/tokenless-maven-9966-jsoup-batch
```

当前脚本同时将 Docker 和 JVM 的处理器数显式限制为二。最初的 jsoup 采集失败是因为 Jetty
测试代理根据本机处理器数计算出的 selector 线程需求超过了配置的线程池上限。仅设置 Docker
CPU 配额未能纠正旧 JDK 的处理器检测，添加 `JAVA_TOOL_OPTIONS=-XX:ActiveProcessorCount=2`
后成功。`jsoup_failure.txt` 保留了原始完整失败，包括错误及堆栈。先前成功的 Commons CLI、
SLF4J 和 Gson 不需要该 JVM 设置，元数据保留了各自的实际命令。不把失败结果记为成功构建。
采集输出保留了 JVM 的选项提示。

## 验证保真与收益

```bash
cargo test --locked -p tokenless-compressors --test maven
cargo test --locked -p tokenless-compressors build_log::maven -- --nocapture
MAVEN_CAPTURE_ROOT=/tmp cargo test --locked -p tokenless-compressors --test maven -- --nocapture
```

可选根目录扫描读取 `tokenless-maven-9966-*` 目录下 `cold` 和 `warm` 的 stdout、stderr、
observed 文件。30 条成功构建流和 9 条保留的失败采集流通过了独立完整还原、CR 记录按序
逐字节保留、幂等性及零 Stash 检查。`tests/common/maven_oracle.rs` 中的解码器不调用生产
解析器，仅按独立定义的 SGR 语法清理比较输入，无需规范化装饰性布局。单元测试还逐个还原
编辑，并要求每次编辑严格节省字符和估算 token。下表通过仅禁用下载编辑进行消融，保持
识别范围逻辑不变。

测试在明确的 Maven 阶段上下文中，向合成下载块以及捕获的 40 条记录片段的每个边界插入
警告、异常及未知文本，共 216 次变异。其他场景覆盖 CR 传输续行、batch 前缀、单条记录、
重复项、畸形和截断输入、控制序列、Unicode、识别范围恢复及 256 条记录／64 KiB 上限的
原子拒绝。

路由回归覆盖 1/2/30/400 条下载、孤立结果标记、散文／Markdown／CSV／源代码中引用的 Maven
阶段、头部采样预算内的结构化启动器／JVM 前导、日志内外的代码围栏、全部采用 CRLF 换行的
日志以及头尾采样间隙。纯下载片段不触发构建日志路由；Cargo 和独立 Generic 检测也不把
Maven 传输标签或 CR 重绘作为进度证据。无法生成 Maven 编辑时，带命令封装的日志仍支持
Generic 的 Shell/Tool 恢复（包括诊断或未知边界之前仍处于有效范围内的已验证传输记录），
并验证 Stash 省略内容的精确还原。256 次编辑上限前后的用例验证未编辑后缀保留与幂等性。

两个已审查 golden 输出固定压缩表示。原始下载片段单独出现时检查其保持原样，再添加明确的
合成 Maven 阶段前缀，检查其与压缩 golden 一致。完整构建流 fixture 输入保持不变；Runtime、
CLI 和 Hook 正例使用捕获的完整 jsoup batch 日志。仅在有意改变输出格式后重新生成，并检查差异：

```bash
cargo test --locked -p tokenless-compressors --test maven regenerate_maven_goldens -- --ignored --exact
```

本次完整观测混合流的 heuristic-v1 估算结果：

| Fixture | 输入 | 候选 | 节省 | 下载贡献 |
| --- | ---: | ---: | ---: | ---: |
| Commons CLI，热缓存 | 3,992 | 3,657 | 8.39% | 0 |
| SLF4J，热缓存 | 6,040 | 5,817 | 3.69% | 0 |
| Gson，默认冷缓存 | 121,636 | 121,281 | 0.29% | 314 |
| jsoup，默认冷缓存 | 122,265 | 121,975 | 0.24% | 290 |
| jsoup，batch 冷缓存 | 58,770 | 48,582 | 17.34% | 10,188 |

每行将输入与自身候选比较，不将默认输入与 batch 输入直接比较。这不是模型账单或整任务
节省。失败命令 fixture 检验压缩器内部的保真；Runtime 仍原样保留 Tool Error。
CLI 与 Common Hook 测试验证本地路由及替换封装，包括元数据保留与不支持宿主的保护。
本次没有调用真实 Agent。

还通过实际 Protocol v2 CLI、恢复方式 `none` 回放了五个成功的完整 fixture：均选择了更小的
BuildLog 结果，Stash key 为空；失败 fixture 返回 `tool_error` 并保持原样。在 Linux x86-64
上，工作区格式、Clippy、测试和 rustdoc，以及 `make test-integration`、`make test-adapters`、
`make test-rtk-integration` 和 L1/L2 独立工作区检查及 release 测试均通过。集成测试使用
Python 3.11.6，与 CI 的 Python 3.11 系列一致。既有 Headroom 递归测试在本机 Python
3.13/3.14 上失败，在 3.11.6 上通过；未修改该测试。
