# Skill 安全评测

[English](../../en/agent-sec-core/skill-score.md)

可选安全评测组件为一个 Skill 生成探针，在 Docker 中执行并给出有证据支撑的安全分。计划生成和裁决调用所支持的百炼 Anthropic 兼容模型端点，会消耗 Token。预览版使用标准 `sec-core` raw 包，并保留其中的 V2 宿主 CLI 与 daemon。

## 安装可选组件

在 Ubuntu 及兼容的 Linux x86_64 发行版上，下载通过验收的固定 fork release 附件并按可信发布清单核对长度和 SHA-256。将 `index-v2.toml`、`index.toml`、`components-v2.toml` 和标准 raw 包放到本地 `v1/` 目录，再复用 ANOLISA 安装器安装：

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<预览版本号>'
```

版本号取自通过构建验收的 `sec-core` 预览 release。官方安装器会对不以 `/v1` 结尾的根地址追加 `/v1`，因此 GitHub Release 的平铺附件地址不能直接作为仓库根；SkillEval bundle 脚本自动准备本地 v1 布局。正式默认渠道尚未包含此评测扩展。标准 raw 包同时包含 V2 二进制和独立评分 CLI/worker 及锁定的 Python 依赖。公开入口统一为 `/usr/local/bin/agent-sec-cli skillsec score plan/run`。Rust CLI 直接用私有 Python 评分运行时替换当前进程，复用原评分逻辑；不再安装旧预览入口 `agent-sec-skill-score`。评分私有运行时安装到 `/usr/local/lib/anolisa/sec-core/skill-score/{cli,runtime}`，不需要额外运行评分 daemon。安装前核对目标 CPU 与 glibc 基线。

同一 release 提供 `release-manifest.json`（`agent-sec-skill-score/release-v2`）、标准 raw 索引、`skill-score-images.tar.gz`、`images.json`、工具清单及源码归档。发布清单须来自可信发布渠道，并逐项核对制品长度与 SHA-256。清单必须声明 `cli: "agent-sec-cli"` 与 `command_prefix: ["skillsec", "score"]`；消费端拒绝旧预览命令契约。镜像不放入 raw 包；在干净的模板构建环境执行 `docker load -i skill-score-images.tar.gz`，按 `images.json` 核对不可变 image ID。

工作镜像和 ASC 网络侧车缺一不可；运行时不构建或拉取缺失镜像。需要 Linux x86_64、Docker Compose、可用网络 namespace、包内使用的防火墙/conntrack 规则以及 ptrace/strace。内层容器申请 4096 MiB，外层还需为 Docker、CLI、worker 和侧车留出内存。镜像构建成功不代表运行验收通过。

## 生成计划并执行

通过进程环境或平台密钥注入提供 `ANTHROPIC_API_KEY`，按需设置 `ANTHROPIC_BASE_URL`。凭据不要放入命令参数、任务 JSON、模板或发布清单。

```sh
agent-sec-cli skillsec score plan \
  /work/input/skill --output-file /work/output/task.json

agent-sec-cli skillsec score run \
  /work/output/task.json --output-file /work/output/report.json \
  --evidence-dir /work/output/evidence
```

`plan` 接受一个含 `SKILL.md` 的目录，输出 `agent-sec-skill-score/task-v1`，记录 Skill 的绝对路径、内容 hash、审计结果和探针。执行 `run` 时必须保留相同内容与绝对路径；内容发生变化会在调用 worker 前拒绝。也可通过 `run -` 从 stdin 读取任务。

输出文件必须尚不存在，证据目录必须不存在或为空。输出须位于被测 Skill 目录之外，不能占用内部证据路径。每次尝试使用独立输出目录。stdout 同样包含公开 JSON；平台有命令输出上限时，建议通过文件接口传输。

评分在本地执行，不经过 daemon；显式传入 `--socket`、`--timeout-ms`、`--otel-context` 或 `--trace-context` 会被拒绝。评测期限和取消由调用端控制。`skill-ledger` 保留为 `skillsec` 的兼容别名；软件包缺少可选评分运行时时，执行评分阶段会明确报告安装错误。

## 理解结果

| 退出码 | 含义 |
|---|---|
| `0` | 当前阶段未报告执行或持久化失败；仍须检查报告中的动态覆盖。 |
| `1` | 依赖、执行、模型或持久化失败；可能已有部分报告，应予保留。 |
| `2` | 输入或契约无效，包括内容发生变化。 |

公开报告使用 `agent-sec-skill-score/report-v1`，通过 `content_hash` 绑定任务。`score` 和 `decision` 表达安全评测结论，不证明动态执行完整。检查 `execution` 是否覆盖全部计划探针，且每项均为 `completed: true`；非空 `limitations` 表示覆盖不完整。证据缺失或环境不支持应明确显示。高安全分加上不完整执行，不等于动态评测通过。

可选的 `planning_notes` 字符串数组保留任务 `audit.limitations` 中的非阻断说明，例如不存在某类输入。应将其展示为未经验证的计划背景；这些说明本身既不证明，也不阻止动态执行完整。真实的规划错误、环境不可用、超时、缺少观测和裁决失败仍属于不完整情况。`internal-report.json` 同样保留 `planning_notes`，任务格式不变。

一起保存公开 task/report 和证据目录，包括 `environment.json`、`internal-report.json`、`evidence-catalog.json`。失败时先收集部分证据，再删除沙箱。证据可能包含敏感 Skill 内容，访问和留存由平台控制。

## 运行职责

直接使用 Harbor 0.22.0，不修改其依赖源码。ASC 通过公开环境 API 提供任务 Compose 和自有网络侧车镜像。侧车对模型主机执行静态 TCP 白名单；本地目标和 ICMP 保持直连，其余流量拒绝。这不是通用域名防火墙，也不自动实现 Harbor 的分阶段网络策略。

侧车只读策略包含白名单及解析出的 IPv4 地址。GOST 使用该映射与显式空的备用 resolver，工作容器修改共享 `/etc/hosts` 不会改变代理解析。大小写不同、无法命中映射的 TLS 名称保守拒绝。代理失效时保留过滤规则；侧车启动失败时工作容器不得启动。IPv6 验收需要另外提供已启用 IPv6 的测试环境。

部署平台创建外层沙箱、上传不可变 Skill 快照、调用以上命令、下载有界产物并删除精确沙箱。ASC 不接收云管理凭据，也不实现 FC 生命周期 API。采用 FC 时，标准 raw 包、Docker 和镜像均安装在 FC 模板内，SkillEval 只负责编排。制品下载和镜像加载发生在模板构建阶段，无须每次评测重新下载。

## 构建与验证

开发者在 Linux x86_64 上准备 Rust 1.93.1、uv、原生编译依赖、Docker 和 Compose。`CI / Skill score raw preview` 工作流在 Ubuntu 构建并安装完整 raw 包，检查已安装的 CLI/worker 和 Docker 网络行为，通过后才发布 fork 预览 release。该工作流不发布 FC 模板，也不证明真实模型评分已完成验收。

预览构建通过 `packaging/skill-score/build-payload.sh` 生成评分内容，再用 `RAW_SKILL_SCORE_PAYLOAD` 纳入标准 `make package-raw` 构建。`RAW_PREVIEW_VERSION` 只设置暂存包的预览版本，不改源码产品版本。默认 raw 构建仍仅含 V2。预览发布要求干净源码提交并绑定制品摘要；`build-rpms.sh` 保留为独立的维护者 RPM 构建路径。

安装 worker 并构建最新镜像后，执行 Linux 网络回归：

```sh
PYTHONPATH=src/agent-sec-core/agent-sec-cli/src:src/agent-sec-core/skill-score-runtime/src \
python src/agent-sec-core/tests/integration-test/skill-score/validate_egress.py \
  --output /absolute/new/egress-validation
```

该网络回归不调用真实模型，不能替代正常、违规、环境失败和超时样本的完整 `plan → run → report` 验收。

构建机的 Docker bridge 无法访问包源时，可为构建命令显式设置 `ASC_IMAGE_BUILD_NETWORK=host`。它只影响可信的 Docker 构建步骤；评测容器仍使用独立网络策略。
