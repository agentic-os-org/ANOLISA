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
歧义。真实 SDK 子 Worker 的 AW 映射仍需核验；SDK 实测见报告，后续见[受限试点](ARCHITECTURE_NOTES_zh.md#小规模受限试点)。

## SDK 边界 fixture

SDK fixture 使用真实分发 API 和本机合成模型端点，无需原生 Hook。
[报告](POC_REPORT_zh.md#不使用原生-hook-的-sdk-验证)分别说明工具入口检查、syscall
拒绝和传输覆盖。

在已授权的 Linux x86-64 实验机准备私有依赖环境，需要 Node 22、`libseccomp.so.2`
及上述 BPF 前提。在独立目录安装 Python 和 SDK 依赖：

```bash
SDK_ROOT="$PWD/providers/observation/target/sdk-deps"
python3 -m venv "$SDK_ROOT/venv"
SDK_PYTHON="$SDK_ROOT/venv/bin/python"
SDK_NPM="$SDK_ROOT/npm"
"$SDK_PYTHON" -m pip install -r providers/observation/fixtures/sdk_transport_requirements.txt
npm install --ignore-scripts --prefix "$SDK_NPM" \
  @qoder-ai/qoder-agent-sdk@1.0.56 @modelcontextprotocol/sdk@1.32.2
```

Qoder 模型 fixture 还需要在进入离线 namespace 前安装 Worker 1.1.78。
`--ignore-scripts` 安装的 npm 包不包含 Worker；官方安装器下载并校验对应平台运行时：

```bash
node "$SDK_NPM/node_modules/@qoder-ai/qoder-agent-sdk/scripts/postinstall.cjs"
```

上述全新安装命令已核对安装器源码，本次实验未执行。实测 Worker JS 来自预装 SDK 树，
其原生平台资产未验收，详见报告。工具分发用例不启动 Worker。`SDK_NPM` 指向含
`package.json` 和 `node_modules` 的项目根。
运行工具用例：

```bash
"$SDK_PYTHON" providers/observation/scripts/sdk_boundary_cases.py \
  --python "$SDK_PYTHON" --npm "$SDK_NPM" \
  --output "$PWD/providers/observation/target/sdk-boundary"
```

预期 51 项通过。Allow/deny 用例同时核验真实副作用和工具体是否进入。Launcher 放行
第一次解释器 exec，随后拒绝 exec 和写方式打开文件。运行时注入覆盖固定 LangChain
版本的同步/异步工具入口，并以直接调用原函数验证绕过边界。脚本为实验 launcher，尚未
调用 AW 策略 RPC；仅跑工具用例时可使用 `sdk_boundary_requirements.txt`。

模型传输使用 `iproute2` 和 `util-linux` 创建仅有 loopback 的临时 namespace。
通过已有 sudo 授权创建后，立即降为当前用户执行：

```bash
SDK_ENTRY="$SDK_NPM/node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"
sudo -n unshare --net -- sh -c '
  ip link set lo up
  exec runuser -u "$1" -- "$2" "$3" --output "$4" --qoder-sdk "$5" \
    --case langchain --case claude
' fixture "$(id -un)" "$SDK_PYTHON" \
  "$PWD/providers/observation/scripts/sdk_transport_cases.py" \
  "$PWD/providers/observation/target/sdk-transport" "$SDK_ENTRY"
```

预期 LangChain 为 `OBSERVED`；Claude 模型调用完成，采集状态为 `UNSUPPORTED`。
脚本会拒绝宿主网络或 root。它启动本机 TLS 服务，核对发送进程的 Socket 和身份，
附着两种 SSL 探针并核验服务端接收。没有认证支持时添加 `--case qoder` 会报告
BLOCKED，使矩阵返回非零。输出包含私有结果及进程诊断，完成清理后只删除选定 fixture
输出目录；依赖包及运行证据不入源码。

Qoder 认证后的直连测试需要账号具备自定义 Provider 能力，fixture 保留 SDK 的真实能力
检查。凭据文件须仅本人可读；helper 接收文件路径，再通过继承 FD 传入，不将凭据值放入
命令行参数或环境变量：

```bash
SDK_AUTH_ROOT="$(mktemp -d)"
(
  umask 077
  TOKEN_FILE="$SDK_AUTH_ROOT/token"
  trap 'rm -f "$TOKEN_FILE"' EXIT
  python3 -c 'import getpass, pathlib, sys; pathlib.Path(sys.argv[1]).write_text(getpass.getpass("Qoder token: "))' "$TOKEN_FILE"
  bash providers/observation/scripts/sdk_transport_auth_demo.sh \
    "$SDK_PYTHON" "$SDK_ENTRY" "$TOKEN_FILE" "$SDK_AUTH_ROOT/run"
)
```

Helper 仅转发精确放行的官方认证和只读初始化路径，拒绝重定向和外部推理，开始采集前
关闭初始化通道，退出时删除本次 SDK HOME/TMPDIR。输入凭据文件归调用者管理；上述子
shell 会删除临时副本。实测账号缺少所需能力，因此该项返回 BLOCKED，本机 TLS 和采集
均未启动；具备能力的账号仍需完整模型/采集验收。检查 `SDK_AUTH_ROOT` 下的私有结果后，
只删除该自有目录。

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
