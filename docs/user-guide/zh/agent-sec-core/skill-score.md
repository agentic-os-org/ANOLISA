# Skill 安全评测

[English](../../en/agent-sec-core/skill-score.md)

Skill Score 为一个 Skill 规划任务，在受控 Docker 环境中执行，并给出有证据支撑的安全分。标准 ASC CLI 提供 Rust 原生规划、裁决和评分；动态执行依赖必须显式安装。模型辅助阶段会消耗 Token。

## 安装

首个原生预览版使用配套的 fork release：两个 raw 组件、一个独立镜像归档，以及支持候选版本准备的安装器。正式默认渠道可能尚未包含此预览版。使用可信 release 附带的安装器（0.3.20 或更高版本），执行前按 release-v3 清单校验所有制品。SkillEval 的 bundle 脚本会自动准备本地仓库。

将校验后的 raw 包和索引组织到本地 `v1/` 仓库后，安装基础组件：

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

基础 install、update、repair 不下载评分所需的 Python、Harbor、Docker 或镜像。`plan` 只需要基础 CLI 和模型配置。ASC 其他适配器已有的 Python 依赖另行管理。

动态执行需要 Linux x86_64、可用的本地 Docker daemon、Compose、网络 namespace、防火墙/conntrack 能力及 ptrace/strace。除工作容器申请的 4096 MiB 外，还需为控制器和侧车预留内存。准备好宿主后，显式安装相同版本的可选组件：

```sh
sudo anolisa --install-mode system install sec-core-skill-score --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

这个小 raw 包包含 ASC 自有 worker、共享契约、依赖锁文件和安装 helper。安装时从上游分发源获取锁定的 Python 3.12/Harbor 依赖，校验独立的 `skill-score-images.tar.gz` 后加载，再核对两个镜像的不可变 image ID 和平台。两个 raw 包均不包含容器镜像。工作镜像与 ASC 网络侧车缺一不可；单机 Linux 与 FC 使用同一网络实现。

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

配置文件存在但无效时直接失败。文件不存在时，迁移期接受旧的 `ANTHROPIC_API_KEY` 与 `ANTHROPIC_BASE_URL` 成对配置，并输出弃用提示；两个旧变量都必须提供。

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

SkillEval 负责 FC 模板、外层实例生命周期、凭据注入、传输、取消和运维。模板构建时显式安装匹配的基础/可选组件，加载一次校验过的归档。恢复后的实例执行与单机相同的 ASC CLI。ASC 只管理本请求的内层 Docker 资源，其回收状态与 SkillEval 的外层 FC 回收分别记录。

Skill 的业务依赖必须位于 ASC 工作镜像内。仅在外层 FC 模板安装文档库或 Office 工具，被测 Skill 无法使用。部署方可复用固定版本的功能镜像 Dockerfile 构建业务依赖，再叠加 ASC 的 Agent 与采证层。记录配方提交与校验和、上游基础镜像 digest、实际安装版本及最终镜像身份。按同一配方重新构建，不代表结果与已有发布镜像完全相同。生成的 ASC 镜像仍通过同一套校验后的归档交付，供单机与 FC 执行复用。

能力合入主线并通过相同验收后，把可信 release/index 来源从 fork 切到正式发布渠道，保留 CLI、组件名称、模型配置与 task/report 契约。OCI 分发是未来优化，不是首版或正式发布前置条件。

## 构建与验证

在 Linux 上使用固定 Rust toolchain 与 Docker/Compose 开发。`packaging/skill-score/build-preview.sh` 构建原生基础包、可选 raw 组件、镜像归档、配套安装器及 release-v3 元数据，绑定源码和制品身份。旧 `RAW_SKILL_SCORE_PAYLOAD` 路径被拒绝，不再包含评分 Python 3.11 环境；保留的 Python 评分器仅用作差分测试 oracle。

使用预装业务依赖的基础镜像时，将 `ASC_WORK_BASE_IMAGE` 设为按 digest 固定的仓库引用或本地不可变 Docker image ID。本地镜像还需通过 `ASC_WORK_BASE_RECIPE` 指向 JSON，包含 `source_repository`、`source_commit`、`dockerfile_path`、`dockerfile_sha256` 和 `upstream_base_image`（`reference`、`image_id`、`platform`）。构建程序通过 Docker 核对上游引用和平台，将配方记入工作基础镜像元数据，并给预览版本追加 `.base.<12位image-ID>`。自定义基础镜像的包名与 release tag 因而区别于自动发布的默认预览版。这些是维护者的构建输入，终端用户运行 Skill 不需要选择。Docker image ID 可能随存储后端表示配置或 manifest 身份，不能混同于归档校验和，也不能据此虚构仓库 digest。

预览工作流在发布前检查已安装包行为和网络控制。独立 Linux CLI 与 FC SkillEval 的真实模型运行分别验收。先冻结标签，分别报告正常/违规样本、激活情况、不完整执行和清理；小样本零误报不代表总体误报率。回滚与验收要求见[原生设计](../../../../src/agent-sec-core/docs/design/skill-score-native-design_zh.md)。
