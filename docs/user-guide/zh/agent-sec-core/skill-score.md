# Skill 安全评测

[English](../../en/agent-sec-core/skill-score.md)

Skill Score 为一个 Skill 规划任务，在受控 Docker 环境中执行，并给出有证据支撑的安全分。标准 ASC CLI 提供 Rust 原生规划、裁决和评分；动态执行依赖必须显式安装。模型辅助阶段会消耗 Token。

## 安装

### 安装基础 CLI

**raw 预览版（Linux x86_64）。** 校验发布方提供的 `sec-core-bundle-<version>-linux-x86_64.tar.gz` 摘要，解压到空目录。再解压其中的 `v1/anolisa-cli-<installer-version>-linux-x86_64.tar.gz`，将 `anolisa` 可执行文件安装到 `/usr/local/bin`。在 `/etc/anolisa/repo.toml` 中把 raw 后端指向这个可信仓库。如果已有其他后端配置，更新现有的 raw 配置段即可。

```toml
schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "file:///absolute/path/to/preview/v1"
```

```sh
sudo anolisa --install-mode system install sec-core --backend raw --version '<preview-version>'
```

**V2 RPM（Linux x86_64）。** 从包含目标 ASC 版本的软件源安装现有软件包。

```sh
sudo dnf install agent-sec-core
```

基础 CLI 配好模型后即可执行 `plan`。动态评分用到的 Python/Harbor 环境和容器镜像在显式启用时准备。

### 启用动态评分

raw 用户先校验同版本 `skill-score-bundle-<version>-linux-x86_64.tar.gz` 的发布摘要，再解压到基础 bundle 所在目录，两份归档共同组成一个仓库。现有 RPM 已随包提供运行时配方。准备好本机 Docker、BuildKit/Compose、网络 namespace、防火墙及 conntrack、ptrace/strace 后，执行：

```sh
sudo agent-sec-cli skillsec score install-runtime
```

raw 路径由 ASC 读取已装主包版本，通过配置的 raw 仓库取得可选配方。RPM 路径由 ASC 核对包内配方的归属。两者共用 ASC 的准备代码，构建或加载镜像并验证 worker。同一台机器应选定一种系统安装渠道。

安装阶段从已校验的源码构建工作镜像和 ASC 网络侧车，获取锁定的 Python 3.12/Harbor 依赖，并验证生成的运行环境。基础镜像固定 digest；Dockerfile 内的包管理器仍需要联网。系统包和部分传递依赖并未全部锁定，因此不同构建可能得到不同 image ID。安装侧生成 `image-build.json`，绑定发布清单摘要、实际基础镜像、工具清单和两个镜像的不可变 config ID，以及 OCI 导出中存在的 manifest ID；就绪检查和执行都核验该回执，不承诺预先知道输出 digest。经典 Docker 导出只绑定已核验的 config ID，不生成虚假的 manifest ID。构建失败、超时或取消时提示受限诊断日志路径；成功后删除日志。

当前执行后端需要这两个镜像。镜像构建子进程上限为 25 分钟。磁盘需容纳镜像层及临时本地导出；除工作容器的 4096 MiB 外，还需为控制器和侧车预留内存。构建或网络失败会中止安装，不激活不完整环境。`ANOLISA_SKILL_SCORE_SOURCE_DIR` 可提供已校验的 Python/uv 归档与 wheel；`ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE=1` 禁止依赖下载回退，但镜像构建仍需可联网的构建环境；`ANOLISA_SKILL_SCORE_OFFLINE=1` 则拒绝全新镜像构建。已经准备完成的 FC 模板恢复时不下载、不重建。

raw 升级时，先让主包和可选配方对齐到同一发布版本，再执行 `sudo agent-sec-cli skillsec score install-runtime`。RPM 用户升级主包后执行同一命令。ASC 按清单摘要准备环境，验证成功后切换 `active-manifest.json`。如果新版准备失败，已有的兼容 worker 仍可使用。排除故障或发现生成文件损坏时，重新执行这条命令即可重试或修复。`run` 使用已就绪且兼容的环境。

raw 用户用 `sudo anolisa --install-mode system uninstall sec-core-skill-score` 卸载可选配方和其管理的闲置环境。RPM 用户卸载现有 RPM 时一并清理。存在活动任务或未确认清理的记录时，先确认对应请求资源已回收，再卸载。共享缓存和 Docker 镜像可以继续复用。

## 配置模型服务

将以下不含密钥的配置写入 `/etc/agent-sec/model-service.json`，也可通过 `AGENT_SEC_MODEL_CONFIG` 指定其他文件：

```json
{
  "schemaVersion": 1,
  "remote": {
    "provider": "bailian-payg-anthropic",
    "baseUrl": "https://dashscope.aliyuncs.com/apps/anthropic",
    "defaultModel": "qwen3.8-max",
    "apiKeyEnv": "AGENT_SEC_MODEL_API_KEY"
  }
}
```

通过调用进程环境或平台密钥管理注入 `AGENT_SEC_MODEL_API_KEY`。不要把实际值放入参数、任务 JSON、模板或发布清单。Judge 和被测 Agent 复用解析后的 provider 配置，worker 只获得 Agent 必需的模型凭据。已有本地/Ollama 扫描器默认行为保持不变。

接入运维方管理的 Anthropic Messages 代理时，将 `provider` 设为 `anthropic-compatible`，`baseUrl` 设为可信代理前缀，例如 `https://models.example.invalid/anthropic`。ASC 会追加 `/v1/messages`，前缀不要包含这个后缀，其余字段不变。URL 必须使用 HTTP(S) 和 DNS 主机名或 IPv4 地址，不包含内嵌凭据、查询参数、片段、空白或反斜线；支持显式端口。HTTP 用于可信内网，跨不可信网络使用 HTTPS。动态执行要求能解析到 IPv4，继续由现有网络侧车放行配置的主机；这是主机策略，不是 URL 路径策略。

ASC 通过已有 Anthropic 凭据通道原样传递引用的值，其中 Rust 请求使用 `x-api-key`。ASC 不生成或解析用户／任务身份，也不上报账单。调用平台负责授权代理地址并生成访问上下文，代理负责鉴权与计量。同一配置用于规划、判分和 Claude Code 动态执行。代理支持时应限制凭据的作用范围与有效期：被测 Agent 会获得该值，允许访问模型端点不构成数据防泄漏边界。

代理须支持现有 Messages 请求参数（包括 `thinking` 和 `output_config`）、token usage 字段及 Claude Code 流式／工具调用。不兼容时明确失败，不静默删除参数或切换服务。离线兼容测试不能证明模型质量、网络可达性或计费正确性。

配置文件存在但无效时直接失败。文件不存在时，迁移期接受旧的 `ANTHROPIC_API_KEY` 与 `ANTHROPIC_BASE_URL` 成对配置，并输出弃用提示；两个旧变量都必须提供且保留百炼端点约束，自定义代理必须显式使用配置文件。回滚代理路由时恢复上面的百炼配置。

## 生成计划并执行

```sh
agent-sec-cli skillsec score plan \
  /work/input/skill --output-file /work/output/task.json

agent-sec-cli skillsec score run \
  /work/output/task.json --output-file /work/output/report.json \
  --evidence-dir /work/output/evidence
```

`plan` 接受含 `SKILL.md` 的目录，生成 `agent-sec-skill-score/task-v1`。内容摘要覆盖完整执行输入，包括二进制、依赖目录、路径和可执行权限。符号链接、特殊文件和超出支持边界的输入会被拒绝；模型文本审阅范围另行记录。`run` 创建并核验新的私有快照，再执行该快照。旧预览版使用部分内容摘要的任务须重新规划，不会静默升级。两个阶段之间保留 Skill 路径和内容。

`run -` 从 stdin 读取。输出文件必须不存在；证据目录必须不存在或为空。输出位于 Skill 之外，不能与内部证据文件重叠。stdout 输出一个公开 JSON，进度写入 stderr；平台会截断命令输出时使用文件传输。

评分是显式的本地操作，独立于 ASC daemon，不对 Ledger 状态进行认证。显式 `--socket`、`--timeout-ms`、`--otel-context` 和 `--trace-context` 参数会被拒绝。`skill-ledger` 仍是 `skillsec` 的兼容别名。

## 理解结果

| 退出码 | 含义 |
| --- | --- |
| `0` | 当前阶段完成；`run` 已完成执行、裁决及资源回收。安全结论仍可能阻断该 Skill。 |
| `1` | 依赖、执行、模型、证据、取消或回收失败；保留已有部分报告。 |
| `2` | 输入、schema 或内容绑定无效，包括 Skill 改变或使用旧摘要。 |

任务文件、Skill 路径、运行时文件、worker 启动及产物 I/O 失败时，stderr
包含 `stage=... path="..."` 和原始操作系统错误。例如，
`stage=task.resolve path="/work/input/task.json": No such file or directory (os error 2)`
表示缺少任务输入；`runtime.base-manifest.read` 定位安装记录，
`runtime.image-receipt.read` 定位已准备的镜像回执。路径加引号并转义控制字符，
不包含任务正文或 worker 参数。退出码和报告 schema 保持不变。
即使没有生成报告，也应保留 stderr。使用 SkillEval 时，日志采集成功后可在返回的
`artifact_dir` 中查看 `cli.log` 和 `runtime.log`。

Report-v1 通过 `content_hash` 绑定任务，将 `decision`/`score` 与 `execution_status`、`dynamic_complete`、`cleanup_status` 分开。消费方要求执行与回收完整，不能只看安全分。`execution[].completed` 表示任务完成，与观测完整、探针激活及风险确认不同。原 Skill 的业务任务即使失败，有充分证据时仍可评估；Critical 提前停止时，未执行的探针明确记为不完整。

单个探针超时且资源回收成功后，必需探针最多再试一次。只要整轮评测还有执行预算，后续探针就能继续。重试耗尽的探针在报告中标为未完成。

`planning_notes` 保留未经验证的非阻断计划背景。运行限制、缺失证据和环境不支持仍须展示，不通过过滤限制文案推断成功。一起保存 task、report、`internal-report.json`、`evidence-catalog.json` 和回收产物。证据按大小限制且不跟随链接读取；采集摘要证明的是采集后的完整性，不独立证明被测容器可写文件的真实性。部署方控制证据访问和留存。

每项 `execution` 分开记录 `execution_complete`（执行与采证完整）和 `completed`（业务任务成功）。Agent 拒绝或 Skill 自身报错可以被完整评测，但不等于业务任务成功。

## 平台接入与正式发布迁移

ASC 通过公开任务 Compose API 使用未修改的 Harbor 0.22.0。工作容器共享 ASC 侧车的网络 namespace，不持有 Docker socket 或网络管理权限。侧车采用固定模型主机 TCP 策略，保留本地目标/ICMP 直连，拒绝其他流量；它不是通用域名防火墙。独立只读的域名映射保证工作容器修改 hosts 文件不会重定向代理。IPv6 需要单独验收。

SkillEval 负责 FC 模板、外层实例生命周期、模型凭据、传输和取消。构建方固定同一发布版本，安装基础包，再调用 `install-runtime` 准备或暂存 worker。直接用 raw 安装器安装可选配方只会铺设文件，采用这条路径的构建脚本还需调用 `install-runtime`。恢复后的实例先离线激活，再执行 `plan/run`。ASC 管理本次请求的内层 Docker 资源。

Skill 的业务依赖必须位于 ASC 工作镜像内。规划时，ASC 参考默认工具清单，记录 Skill 实际需要的工具。执行每个探针前，worker 会在选定的工作镜像中核实这些依赖。仅在外层 FC 模板安装文档库或 Office 工具，被测 Skill 无法使用。部署方可复用固定版本的功能镜像 Dockerfile 构建业务依赖，再叠加 ASC 的 Agent 与采证层。记录配方提交与校验和、上游基础镜像 digest、实际安装版本及最终镜像身份。按同一配方重新构建，不代表结果与已有发布镜像完全相同。单机或 FC 安装各自记录已验证的镜像构建回执。

能力合入主线并通过相同验收后，把可信 release/index 来源从 fork 切到正式发布渠道，保留 CLI、组件名称、模型配置与 task/report 契约。OCI 分发是未来优化，不是首版或正式发布前置条件。

### 在普通 Dockerfile 中预置镜像

没有 Docker daemon 的镜像构建器可以先安装 worker 文件，等执行环境启动后再激活。基础包与可选包必须来自同一版本。预置归档属于部署制品，不增加公共 Release bundle。

在可运行 Docker 的 Linux 构建机上，使用目标业务基础镜像完成正常的显式运行时安装。然后向一个新目录导出已验证的镜像和绑定版本的清单：

```sh
sudo /usr/local/lib/anolisa/sec-core-skill-score/prepare.sh \
  --prefix /usr/local --export-preload /absolute/build-context/preload
```

输出包含 `manifest.json` 和 `skill-score-images.tar.gz`，二者通过平台可信构建上下文一起传递。RPM 安装改用 `/usr/lib/agent-sec-core/skill-score/prepare.sh --prefix /usr`。导出拒绝覆盖已有目录，并在保存镜像前检查已安装运行时、网络能力和执行环境。

最终 Dockerfile 按正常 raw 路径安装基础包和版本匹配的 ANOLISA 安装器，提供可访问的可选包仓库，然后执行：

```dockerfile
RUN ANOLISA_SKILL_SCORE_STAGE_ONLY=1 agent-sec-cli skillsec score install-runtime
COPY --chown=0:0 preload/ /usr/local/lib/anolisa/sec-core-skill-score/preload/
```

在这个构建步骤的 ASC 命令上设置 `ANOLISA_SKILL_SCORE_STAGE_ONLY=1`。ASC 准备并验证 Python、Harbor 和 worker，写入暂存记录。模板激活需要本机 Docker daemon。RPM 用户用同一命令暂存，将归档放入 `/usr/lib/agent-sec-core/skill-score/preload/`。

平台启动本地 Docker daemon 后，在实例接收评分任务前激活：

```sh
sudo agent-sec-cli skillsec score install-runtime
```

激活检查预置文件、版本和源码绑定、归档校验和及镜像身份。缺失镜像直接从本地压缩归档加载，不落地临时未压缩 tar，不构建镜像、不下载依赖；已有匹配镜像则复用。网络及执行环境检查通过后才记录就绪。Docker data-root 丢失后也可以从相同归档恢复。预置文件缺失或损坏会明确报错，不回退在线构建。

预置文件应归 root 所有，其他用户不可写。生命周期锁阻止评分期间激活。卸载会删除 ASC 生成的运行环境，但保留平台提供的归档，由镜像所有者删除或通过替换整个镜像回收。回滚时一起恢复匹配的基础包、可选文件和预置归档。该流程不启动 Docker、不修改内核，也不要求新增云端发布流程。

安装或离线激活时，就绪检查会记录本机工作镜像中的工具清单。每次评分先核对本机镜像身份，再复用对应清单。找不到匹配记录时，评分会从工作镜像重新采集。

## 构建与验证

在 Linux 上使用固定 Rust toolchain 与 Docker/Compose 开发。`packaging/skill-score/build-preview.sh` 生成两个归档。基础归档包含安装器和 release-v4 发布清单，可选归档包含 raw 构建配方包和运行时清单。发布清单记录源码提交和制品校验值。

使用预装业务依赖的基础镜像时，在显式安装阶段设置 `ANOLISA_SKILL_SCORE_BASE_IMAGE`，使用按 digest 固定的仓库引用或本地不可变 Docker image ID。本地镜像还需通过 `ANOLISA_SKILL_SCORE_BASE_RECIPE` 指向 JSON，包含 `source_repository`、`source_commit`、`dockerfile_path`、`dockerfile_sha256` 和 `upstream_base_image`（`reference`、`image_id`、`platform`）。构建器验证来源/上游身份，并将实际输出 ID 记录到本次安装回执。这些是可信部署输入，不是每任务可配置项。不能混同 Docker ID 与归档校验和，也不能为未推送镜像虚构仓库 digest。

预览工作流在发布前检查已安装包行为和网络控制。独立 Linux CLI 与 FC SkillEval 的真实模型运行分别验收。先冻结标签，分别报告正常/违规样本、激活情况、不完整执行和清理；小样本零误报不代表总体误报率。回滚与验收要求见[原生设计](../../../../src/agent-sec-core/docs/design/skill-score-native-design_zh.md)。
