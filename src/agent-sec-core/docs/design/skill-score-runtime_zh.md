# 独立 Skill 安全评测交付

[English](skill-score-runtime.md)

> 本文保留旧 Python 预览版设计，作为迁移 oracle 基线。Rust 原生评分与独立可选安装已替代本文的交付方式，参见[原生设计](skill-score-native-design_zh.md)和[当前用户指南](../../../../docs/user-guide/zh/agent-sec-core/skill-score.md)。

本次在 ANOLISA 基线 `5351c892f2e89d6be799ebdbeea948c173d3d7c2` 上纳入独立评分源码。`packaging/skill-score/source-import.json` 绑定来源附件；不会把 release tag 的基线提交视为完整源码。镜像和软件包的实际验收与单元测试分别记录。

评测 CLI 负责计划、源码 hash 核验、证据解读和报告版本；Python 3.12 worker 负责既有 Docker/Harbor 执行。预览版标准 raw 包在 `/usr/local/lib/anolisa/sec-core/skill-score/{cli,runtime}` 安装私有运行时，并在 V2 CLI 中提供 `agent-sec-cli skillsec score plan/run`。该命令走明确的本地分支，在可观测运行时初始化前执行 Unix exec，保留 PID、进程组、标准输入输出和 Python 退出状态，不连接 daemon，也不继承 daemon 请求超时。原 `skill-ledger` 拼写保留为 `skillsec` 的兼容别名。CLI 接入当前原生 prompt backend，扫描错误、降级和截断都不能当作无风险结果。

Harbor 0.22.0 保持原样。worker 生成任务公开的 `environment/docker-compose.yaml`，启用 Harbor public 网络模式，并另外记录 ASC 的有效静态策略。main 共享 ASC 侧车的 network namespace，并等待健康检查；两个服务都有显式健康检查。main 移除 NET_ADMIN、NET_RAW。

侧车镜像与防火墙脚本归 ASC 维护。规则通过所打包的 iptables match 路径绕开原生 nft FIB 表达式，保留本地目标与 ICMP 行为，代理 TCP，拒绝其他出站流量。GOST 退出后不清除过滤规则；缺少策略或侧车启动失败时不得启动 main。

每次任务的只读 GOST 配置包含内联白名单和固定域名到 IPv4 的映射。显式无 nameserver 的 resolver 阻止回落到 Docker 共享 hosts 文件。未知或大小写未匹配的域名保守拒绝，不使用被测进程修改的解析来源。本次提供全任务静态策略，不暴露 Harbor 的分阶段网络策略切换。

部署平台负责沙箱创建、TTL、上传下载、取消重试、凭据及发布回滚。plan/run 恢复相同绝对路径和内容，并通过文件接口下载产物，不解析可能被截断的 stdout。ASC 包中没有 FC SDK、模板 ID 或云管理凭据。

契约使用 `agent-sec-skill-score/task-v1` 和 `agent-sec-skill-score/report-v1`。内容不匹配时在调用 worker 前拒绝 run。非零退出可能已有报告；平台完成状态必须分别检查退出码、内容绑定、全部计划探针和证据覆盖。安全分高不等于动态执行成功。

Report-v1 增加可选的 `planning_notes: string[]`，从任务 `audit.limitations` 复制。未经验证的非阻断计划说明同时保留在公开报告和 `internal-report.json`，不再进入表示动态不足的 `limitations`；task-v1 输入不变。真实的规划错误及执行、观测或裁决失败保留原有的限制和失败处理。消费方单独展示计划说明，并继续拒绝覆盖不完整的结果，不因分数高而放行，也不通过过滤限制文案推断成功。

预览构建生成标准 `sec-core` raw 包及发布索引、独立压缩镜像、源码归档和 release-v2 清单，将校验后的平铺附件组织为本地 `v1/` 仓库，复用 ANOLISA 安装器；另保留独立的 RPM 维护者构建路径。输出不会反写源码里的 inventory。可选构建网络参数只影响 Docker 构建步骤。正式发布要求干净源码提交，dirty 开发制品明确标记。

验证分为三层：可移植契约/配置测试、Linux 原生包与网络回归、真实模型/observer/评分端到端验收。早期原型成功不能证明新包或部署通过后两层。[用户契约](../../../../docs/user-guide/zh/agent-sec-core/skill-score.md)说明命令和结果语义。
