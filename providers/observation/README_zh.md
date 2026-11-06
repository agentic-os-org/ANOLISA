# AW ptrace 观测实验

[English](README.md)

这个独立 Sidecar 从明确授权的 Linux x86-64 测试进程中采集有界 TLS 写入元数据。当前支持动态 OpenSSL 的 `SSL_write`、`SSL_write_ex` 和 advise 模式。独立 fixture importer 将存活进程身份与真实 AW Instance、Task Scope 关联。维护者：kongche-jbw；来源与许可证见 [SOURCE.md](SOURCE.md)。

**普通构建默认拒绝观测：** `observe` 在读取 permit、创建输出及附着之前返回 `observation_disabled`。只有显式使用 `--features unsafe-lab` 构建，才可对可丢弃的合成进程运行观测。强杀该实验 tracer 可能导致目标 SIGTRAP 退出，附着完成但尚未发生首次写入时也存在风险。P0 崩溃隔离未解决，payload 改写保持关闭。[实测报告](POC_REPORT_zh.md)与 [Go/No-Go](GO_NO_GO.md)记录结果。

## 只读 uProbe 故障隔离对照

独立的 BPF 计数实验使用按 PID 过滤的 uProbe link，保留一个计数器，不读取 TLS 正文。
原 ptrace 二进制仍默认禁用。该实验尚未实现带摘要的传输事件合同，也未替代真实 AW
关联演示。

在已授权的 Linux x86-64 实验机上，准备支持 `BPF_TRACE_UPROBE_MULTI` 的 Linux
头文件、GCC、Python 3.11，以及实验用户已有的非交互 sudo 权限，然后运行：

```bash
python3 providers/observation/scripts/uprobe_cases.py \
  --output "$PWD/providers/observation/target/uprobe" --repeat 3
python3 providers/observation/scripts/uprobe_cases.py \
  --output "$PWD/providers/observation/target/uprobe-performance" --performance --repeat 3
python3 providers/observation/scripts/uprobe_coexist.py \
  --output "$PWD/providers/observation/target/uprobe-coexist"
```

脚本创建自己的 TLS 实验进程并编译小型 BPF 加载器。短命 helper 核验 permit，创建
PID 过滤 link 和用户态只读 map 句柄，通过匿名 Unix Socket 传出描述符，在采集开始前
退出。采集器以实验用户身份运行，effective/permitted/inheritable/ambient capabilities
均为空，并设置 `no_new_privs`。实验不创建长期特权服务，也不修改安全设置。该 helper
依赖实验机已有的 sudo 授权，尚未按生产权限代理要求加固。

预期结果及范围见[实测报告](POC_REPORT_zh.md#只读-uprobe-对照实验)。
`aw.uprobe-counter-poc/v0` 仅输出累计入口计数和元数据；没有 payload 摘要、逐次写入
长度和时间戳、返回值或 AW 实例证明。输出最多 512 条，每条最多 8192 字节，活跃采集
最长 10 秒；进程被暂停或输出阻塞时退出可能延后。Fixture 脚本负责期限及进程回收。
生成的证书、二进制和日志全部归属于指定输出目录。

## 构建与演示

需要 Linux x86-64、Rust 1.97.1、Python 3.11+、GCC、OpenSSL 开发库及 `openssl`。Sidecar 与目标必须同 UID，现有 ptrace 策略须允许附着。实测机器原有 `ptrace_scope=0`。在 `ptrace_scope=1` 下，此演示的兄弟进程附着预计会被拒绝；应使用另外授权的隔离实验环境，或后续开发的父进程监督入口。脚本不会修改该设置。

从仓库根目录构建默认拒绝入口，并显式构建用于以下可丢弃 fixture 的风险实验入口：

```bash
(cd providers/observation && cargo build --release --locked --target-dir target/safe)
(cd providers/observation && cargo build --release --locked --features unsafe-lab)
(cd src/aw && cargo build --locked -p aw-cli)
python3 providers/observation/scripts/demo.py \
  --aw "$PWD/src/aw/target/debug/aw" \
  --output "$PWD/providers/observation/target/demo"
```

每次使用新的输出目录。演示启动自己的本机 TLS 服务端、客户端和私有 AW daemon。预期结果为 400 次已登记进程写入、100 次未登记进程写入、服务端字节摘要一致、唯一 fixture 归属关联，以及 owner 退出后 AW binding 数归零。`registered/correlated.jsonl` 包含实例与 Scope ID；Sidecar 原始事件中的 AW ID 始终为空。演示通过现有 Qoder SDK 登记 profile 使用空原生路由，验证范围为生命周期合同，不代表真实 SDK 接入验收。

生成数据均放入指定目录。脚本回收自己的进程并关闭私有 daemon；TLS 私钥仅为临时合成测试材料。核对结果并确认脚本退出后，可删除这一个输出目录。实验不安装 daemon，也不默认登记 Provider。

## 显式 CLI

对自己创建且已授权的实验进程执行：

```bash
OBSERVER="$PWD/providers/observation/target/release/aw-observation-ptrace-poc"
"$OBSERVER" authorize --pid "$FIXTURE_PID" --output "$PRIVATE_DIR/permit.json"
"$OBSERVER" observe --permit "$PRIVATE_DIR/permit.json" \
  --output "$PRIVATE_DIR/events.jsonl" --mode advise \
  --duration 10 --max-events 4096 --rate 10000
```

`authorize` 固定 PID、启动 ticks、UID、PID namespace、boot 身份、可执行文件设备/inode/路径及 SHA-256。这是操作者的实验授权声明；同 UID 恶意程序不在本 POC 的隔离能力内。`observe` 核对 permit 并打开 pidfd，再执行附着。SIGINT/SIGTERM 请求有界 Detach。已有输出文件会被拒绝，新文件权限为 0600。Sidecar 不接收 AW socket 或生命周期句柄。

| 限制 | 默认值 / 硬上限 |
| --- | --- |
| 运行时间 | 10 / 60 秒 |
| Payload 留存 | 仅计算最多 4096 字节的 SHA-256，不输出正文 |
| 事件数 | 4096 / 65,536 |
| 输出速率 | 每秒 10,000 / 100,000 条 |
| 编码后单条记录 | 8192 字节 |
| 输出数据预算 | 64 MiB 阈值，额外最多一条有界事件及生命周期记录 |
| 跟踪线程数 / 暂停样本数 | 256 / 65,536 |
| 附着 / Detach | 各 3 秒；清理失败明确报告 |

超限写入的摘要为空，原因标为 `payload_over_limit`；丢弃事件计入计数。`coverage.status` 保持 `unknown`，传输入口事件无法证明对端接收，也无法识别完整模型请求。`--offsets` 参数可达并明确返回 `unsupported_offsets`；静态、stripped 二进制及其他 TLS ABI 尚不支持。入口已有 `int3` 时返回 `unsupported_live_uprobe_or_int3`，保留原断点。真实 uProbe 共存未验证。`--mode enforce` 会被拒绝。

## 验证

```bash
(cd providers/observation && cargo fmt --all -- --check)
(cd providers/observation && cargo test --locked)
(cd providers/observation && cargo test --locked --features unsafe-lab)
(cd providers/observation && cargo clippy --locked --all-targets -- -D warnings)
(cd providers/observation && cargo clippy --locked --all-targets --features unsafe-lab -- -D warnings)
python3 -B -m unittest discover -s providers/observation/tests -v
python3 providers/observation/scripts/lifecycle.py \
  --output "$PWD/providers/observation/target/lifecycle" --repeat 3
python3 providers/observation/scripts/races.py \
  --output "$PWD/providers/observation/target/races"
python3 providers/observation/scripts/acceptance.py \
  --aw "$PWD/src/aw/target/debug/aw" \
  --output "$PWD/providers/observation/target/acceptance"
```

lifecycle 和 acceptance 脚本在崩溃安全用例失败时返回 **1**。lifecycle 分别记录确定性同步点、注入错误的清理结果及真实 SIGKILL 后果。`AW_PTRACE_TEST_PHASE` 仅在 unsafe-lab 构建中生效，其 stdin 同步等待最长五秒。races 覆盖四轮线程创建、忙进程附着、SIGSTOP/SIGCONT 和组停止期间的 Detach。架构不支持时返回 **77** 并记录 SKIP。单元测试通过不能代表原生验收通过。该实验 workspace 无 AW 依赖，不加入正式布局门禁及安装器。[架构说明](ARCHITECTURE_NOTES.md)单独说明后续查询桥与常驻 worker 的职责。
