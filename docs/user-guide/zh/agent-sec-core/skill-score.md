# Skill 安全评测

[English](../../en/agent-sec-core/skill-score.md)

Skill Score 为一个 Skill 规划任务，在受控 Docker 环境中执行，并给出有证据支撑的安全分。标准 ASC CLI 提供 Rust 原生规划、裁决和评分；动态执行依赖必须显式安装。模型辅助阶段会消耗 Token。

## 安装

fork 预览版包含两个独立交付归档，各附一个 `.sha256` 文件。基础归档 `sec-core-bundle-<version>-linux-x86_64.tar.gz` 包含标准基础 raw 包、索引、发布清单及配套 ANOLISA 安装器；下载和安装基础归档不需要可选 bundle。按可信发布渠道提供的摘要校验后解压到空目录，再解压其中的 `v1/anolisa-cli-<installer-version>-linux-x86_64.tar.gz` 取得安装器。正式默认渠道可能尚未包含此预览版。

从解压后的仓库安装基础组件：

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

基础 install、update、repair 不获取评分所需的 Python/Harbor 环境，也不构建容器镜像。`plan` 只需要基础 CLI 和模型配置；其他 ASC 适配器保留已有依赖。

动态执行时，再下载并校验 `skill-score-bundle-<version>-linux-x86_64.tar.gz`，解压到同一目录。它包含小型可选 raw 包、源码归档和运行时清单，其中有 Dockerfile、构建脚本、worker 代码和依赖锁；不包含预构建容器镜像或第三方 Python 环境。准备 Linux x86_64、本地 Docker 与 BuildKit/Compose、可用的网络 namespace、防火墙/conntrack 及 ptrace/strace，然后显式安装：

```sh
sudo agent-sec-cli skillsec score install-runtime
```

系统 raw 安装会调用 anolisa，使用主包安装记录中的仓库和精确版本。请将配套 anolisa 安装到 /usr/local/bin 或 /usr/bin，并保留已校验的仓库。本地仓库移动后，将同版本制品恢复到原位置。FC 模板等持有已校验临时安装器的调用方，仍可调用底层 raw 安装命令并明确指定仓库和版本。

raw 安装结果和 dry-run 中的 `source_repo` 与 `artifact` 是展示标识。显式 HTTP(S) 来源只显示协议、主机和可选端口；重放已记录的来源时显示 SHA-256 指纹。需要下载地址时，请查看已核实的仓库配置及其分发索引。

基础 raw 包支持 ANOLISA 0.3.21，可选包要求 0.3.22 或更新版本。若旧管理器安装了基础包，先升级管理器，核实原始 raw 仓库和已装版本。在 `/etc/anolisa/repo.toml` 的 raw 后端配置该可信原始地址，再执行 `sudo anolisa --install-mode system repair sec-core`。修复若重放了基础包，会记录其来源。执行 `anolisa --install-mode system --json status`，确认基础包行已有 `raw_repository_sha256` 后再重试 `install-runtime`。若健康基础包返回 `nothing-to-repair` 且指纹仍缺失，可使用已核实的原地址和精确版本显式安装：

```sh
sudo anolisa --install-mode system install sec-core-skill-score --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

如需在空操作修复后恢复统一命令，可在维护窗口卸载 `sec-core`，使用 ANOLISA 0.3.22 或更新版本从已核实的原始仓库重新安装同一版本的基础包，在 status 中确认来源指纹后执行 `install-runtime`。

Linux x86_64 的 V2 RPM 用户先执行 `sudo dnf install agent-sec-core`，再执行上面的同一条命令。现有 `agent-sec-core` RPM 携带构建配方，显式安装时才在 `/usr/lib/agent-sec-core/skill-score` 下准备运行时。RPM 升级后再次执行该命令，最终卸载时会清理归本包所有的生成环境。同机同时存在系统 raw 和 RPM 两份 CLI 时，需要先解决混装。

安装阶段从已校验的源码构建工作镜像和 ASC 网络侧车，获取锁定的 Python 3.12/Harbor 依赖，并验证生成的运行环境。基础镜像固定 digest；Dockerfile 内的包管理器仍需要联网。系统包和部分传递依赖并未全部锁定，因此不同构建可能得到不同 image ID。安装侧生成 `image-build.json`，绑定发布清单摘要、实际基础镜像、工具清单和两个镜像的不可变 config ID，以及 OCI 导出中存在的 manifest ID；就绪检查和执行都核验该回执，不承诺预先知道输出 digest。经典 Docker 导出只绑定已核验的 config ID，不生成虚假的 manifest ID。构建失败、超时或取消时提示受限诊断日志路径；成功后删除日志。

当前执行后端需要这两个镜像。组件准备最多 30 分钟（镜像构建子进程上限 25 分钟），磁盘需容纳镜像层及临时本地导出；除工作容器的 4096 MiB 外，还需为控制器和侧车预留内存。构建或网络失败会中止安装，不激活不完整环境。`ANOLISA_SKILL_SCORE_SOURCE_DIR` 可提供已校验的 Python/uv 归档与 wheel；`ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE=1` 禁止依赖下载回退，但镜像构建仍需可联网的构建环境；`ANOLISA_SKILL_SCORE_OFFLINE=1` 则拒绝全新镜像构建。已经准备完成的 FC 模板恢复时不下载、不重建。

`run` 先检查就绪状态，依赖缺失或不一致时中止并给出安装/修复建议，不在执行中下载、构建或拉取。没有 `prepare` 命令。更新基础组件不会自动更新或安装可选组件，需要显式把两者安装/更新至兼容版本。`install --all` 是明确安装索引中全部组件的操作，也包含可选组件。

移除动态执行依赖使用 `sudo anolisa --install-mode system uninstall sec-core-skill-score`。卸载钩子清除确认归属且未使用的私有环境，包括 repair 保留的环境，基础组件继续保留。运行中的任务或未确认清理的记录会阻止卸载并给出恢复提示；先确认对应请求资源已回收，不要仅为绕过检查而删除记录。卸载不清理共享依赖缓存或 Docker 镜像。

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

Report-v1 通过 `content_hash` 绑定任务，将 `decision`/`score` 与 `execution_status`、`dynamic_complete`、`cleanup_status` 分开。消费方要求执行与回收完整，不能只看安全分。`execution[].completed` 表示任务完成，与观测完整、探针激活及风险确认不同。原 Skill 的业务任务即使失败，有充分证据时仍可评估；Critical 提前停止时，未执行的探针明确记为不完整。

`planning_notes` 保留未经验证的非阻断计划背景。运行限制、缺失证据和环境不支持仍须展示，不通过过滤限制文案推断成功。一起保存 task、report、`internal-report.json`、`evidence-catalog.json` 和回收产物。证据按大小限制且不跟随链接读取；采集摘要证明的是采集后的完整性，不独立证明被测容器可写文件的真实性。部署方控制证据访问和留存。

每项 `execution` 分开记录 `execution_complete`（执行与采证完整）和 `completed`（业务任务成功）。Agent 拒绝或 Skill 自身报错可以被完整评测，但不等于业务任务成功。

## 平台接入与正式发布迁移

ASC 通过公开任务 Compose API 使用未修改的 Harbor 0.22.0。工作容器共享 ASC 侧车的网络 namespace，不持有 Docker socket 或网络管理权限。侧车采用固定模型主机 TCP 策略，保留本地目标/ICMP 直连，拒绝其他流量；它不是通用域名防火墙。独立只读的域名映射保证工作容器修改 hosts 文件不会重定向代理。IPv6 需要单独验收。

SkillEval 负责 FC 模板、外层实例生命周期、凭据注入、传输、取消和运维。模板构建时显式安装匹配的基础/可选组件，构建并校验两个镜像，保存构建回执。恢复后的实例执行与单机相同的 ASC CLI。ASC 只管理本请求的内层 Docker 资源，其回收状态与 SkillEval 的外层 FC 回收分别记录。

Skill 的业务依赖必须位于 ASC 工作镜像内。仅在外层 FC 模板安装文档库或 Office 工具，被测 Skill 无法使用。部署方可复用固定版本的功能镜像 Dockerfile 构建业务依赖，再叠加 ASC 的 Agent 与采证层。记录配方提交与校验和、上游基础镜像 digest、实际安装版本及最终镜像身份。按同一配方重新构建，不代表结果与已有发布镜像完全相同。单机或 FC 安装各自记录已验证的镜像构建回执。

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

`ANOLISA_SKILL_SCORE_STAGE_ONLY=1` 是供镜像构建使用的输入：安装、校验 Python、Harbor 和 worker 文件，不访问 Docker，也不写入 `ready.json`。它也可以随固定仓库和版本的显式 ANOLISA raw 可选包安装命令传入。RPM 在安装现有基础 RPM 后使用相同环境变量和 CLI，默认预置目录为 `/usr/lib/agent-sec-core/skill-score/preload/`，不新增 RPM。不要把该变量设为镜像全局 `ENV`。

平台启动本地 Docker daemon 后，在实例接收评分任务前激活：

```sh
sudo agent-sec-cli skillsec score install-runtime
```

激活检查预置文件、版本和源码绑定、归档校验和及镜像身份。缺失镜像直接从本地压缩归档加载，不落地临时未压缩 tar，不构建镜像、不下载依赖；已有匹配镜像则复用。网络及执行环境检查通过后才记录就绪。Docker data-root 丢失后也可以从相同归档恢复。预置文件缺失或损坏会明确报错，不回退在线构建。

预置文件应归 root 所有，其他用户不可写。生命周期锁阻止评分期间激活。卸载会删除 ASC 生成的运行环境，但保留平台提供的归档，由镜像所有者删除或通过替换整个镜像回收。回滚时一起恢复匹配的基础包、可选文件和预置归档。该流程不启动 Docker、不修改内核，也不要求新增云端发布流程。

## 构建与验证

在 Linux 上使用固定 Rust toolchain 与 Docker/Compose 开发。`packaging/skill-score/build-preview.sh` 构建原生基础包、可选源码组件、配套安装器及 release-v4 元数据，再输出两个独立归档，绑定源码和制品身份。旧 `RAW_SKILL_SCORE_PAYLOAD` 路径被拒绝，不再包含评分 Python 3.11 环境；保留的 Python 评分器仅用作差分测试 oracle。

使用预装业务依赖的基础镜像时，在显式安装阶段设置 `ANOLISA_SKILL_SCORE_BASE_IMAGE`，使用按 digest 固定的仓库引用或本地不可变 Docker image ID。本地镜像还需通过 `ANOLISA_SKILL_SCORE_BASE_RECIPE` 指向 JSON，包含 `source_repository`、`source_commit`、`dockerfile_path`、`dockerfile_sha256` 和 `upstream_base_image`（`reference`、`image_id`、`platform`）。构建器验证来源/上游身份，并将实际输出 ID 记录到本次安装回执。这些是可信部署输入，不是每任务可配置项。不能混同 Docker ID 与归档校验和，也不能为未推送镜像虚构仓库 digest。

预览工作流在发布前检查已安装包行为和网络控制。独立 Linux CLI 与 FC SkillEval 的真实模型运行分别验收。先冻结标签，分别报告正常/违规样本、激活情况、不完整执行和清理；小样本零误报不代表总体误报率。回滚与验收要求见[原生设计](../../../../src/agent-sec-core/docs/design/skill-score-native-design_zh.md)。
