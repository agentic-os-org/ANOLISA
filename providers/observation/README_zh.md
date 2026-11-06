# AW 传输观测实验

[English](README.md)

这个独立包从授权的 Linux x86-64 进程采集有界 TLS 写入元数据。只读 BPF Source 输出
时间、长度、SHA-256 和采集丢失，独立 importer 关联真实 AW fixture 身份。下一步面向
Qoder/Qoder SDK 的小规模受限观测。[统一 Hook 接入说明](ARCHITECTURE_NOTES_zh.md)
介绍可提供的能力、替代原生 Hook 的限制及试点方案。

本包为 Observation Source 实验，尚无默认安装和正式 Provider 协议登记。
维护者：kongche-jbw；来源和许可证见 [SOURCE.md](SOURCE.md)，
验证结果见[实测报告](POC_REPORT_zh.md)。

## 运行 BPF fixture

使用明确授权的 Linux x86-64 实验机，准备支持 `BPF_TRACE_UPROBE_MULTI` 的内核及
UAPI、Python 3.11+、GCC、支持 BPF target 的 Clang、libbpf 开发库与运行库、OpenSSL
开发库及 `openssl`。Fixture 使用操作者已有的非交互 sudo 权限运行短命 setup helper，
不修改全局安全设置。采集器以普通用户、空 capabilities 运行。实测版本见报告。

从仓库根目录构建真实 AW CLI，再执行矩阵：

```bash
(cd src/aw && cargo build --locked -p aw-cli)
AW_FIXTURE="$PWD/src/aw/target/debug/aw"
python3 providers/observation/scripts/transport_cases.py \
  --output "$PWD/providers/observation/target/transport" --aw "$AW_FIXTURE"
python3 providers/observation/scripts/transport_lifecycle.py \
  --output "$PWD/providers/observation/target/transport-lifecycle"
```

使用新输出目录，绝对路径须满足 Unix Socket 长度限制。矩阵编译 BPF 对象、loader 和
TLS 客户端，创建本机服务端与私有 AW daemon，核对接收字节和摘要并回收进程。
预期矩阵 69 项通过，其中单实例关联 200 条事件，同 PID 多实例歧义 200 条；生命周期
脚本为 21 项。目录包含私有 JSONL、生成的证书及 `results.json`，确认清理后只删除该
输出目录。省略 `--aw` 时运行 67 项且不验证实例关联；`--smoke` 仅运行基本用例。

## Source 输出与限制

直接入口 `scripts/uprobe_transport.py` 必须指定 `--permit`、`--library`、`--object`
和 `--output`。Fixture 生成私有进程 permit、`loader.so` 和 `transport.bpf.o`。
Permit 固定 PID/启动 ticks、UID、PID namespace、boot、可执行文件身份与 hash；
采集器持有 pidfd，每次投影前重新核验身份。当前 loader 由实验操作者控制，持续部署前
需要固定并审核加载资产。

| 选项或资源 | 默认值 / 上限 |
| --- | --- |
| `--function` | `SSL_write`，也支持 `SSL_write_ex` |
| `--duration` | 10 / 30 秒活跃采集 |
| `--max-events` | 4096 / 65536 |
| `--rate` | 每秒 10000 / 100000 条事件 |
| `--privileged-fixture-setup` | 显式启用短命 sudo helper |
| 内核 ring / 记录 | 1 MiB / 4144 字节 |
| 捕获字节 | 最多 4096，临时存于内存，仅持久化 SHA-256 |
| JSONL 输出 | 独占创建、0600、单条 8192 字节、数据 64 MiB 加两条控制记录 |

超限写入摘要为空；拥塞时丢弃事件并计数，目标继续运行。输出上限不限制目标写入速率。
缺少最终 summary（包括采集器 SIGKILL）时，区间丢失为 unknown。采集器暂停或文件系统
阻塞可能超过活跃时限，fixture 负责墙钟期限。共享内存读取不具有原子性，入口摘要不能
证明实际发送采用。

私有 `aw.transport-observation-poc/v0` Schema 保留传输层覆盖。原始事件 AW ID 为空，
独立 importer 核验 fixture 归属后再关联。未知进程保持 Observed，同 PID 多实例保留
歧义。真实 Qoder/SDK TLS 路径和子 Worker 映射仍需核验，见[受限试点](ARCHITECTURE_NOTES_zh.md#小规模受限试点)。

## 开发检查与对照后端

```bash
python3 -B -m unittest discover -s providers/observation/tests -v
(cd providers/observation && cargo fmt --all -- --check)
(cd providers/observation && cargo test --locked)
(cd providers/observation && cargo clippy --locked --all-targets -- -D warnings)
```

`uprobe_cases.py`、`uprobe_coexist.py` 保留只计数对照。Rust ptrace 后端默认拒绝
`observe`，仅在本包显式执行 `cargo build --release --locked --features unsafe-lab`
后开放可丢弃 fixture；强杀 tracer 可导致目标 SIGTRAP，首次写入前也可能发生。
`acceptance.py`、`lifecycle.py`、`races.py`、`demo.py` 保留复现覆盖，需要 AW 的入口用
`--aw` 指定私有 fixture。崩溃用例失败时返回非零；payload 改写尚未开放。
