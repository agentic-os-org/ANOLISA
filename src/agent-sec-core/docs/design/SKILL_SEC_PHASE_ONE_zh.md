# SkillSec 第一阶段迁移

[English](SKILL_SEC_PHASE_ONE.md)

SkillSec 在一个 `asc-capability-skill-sec` crate 内拆分 Skill 扫描、内容完整性、
版本存储和激活，由系统 daemon 中的 `SkillSecService` 编排。本文记录迁移合同、开发批次
及 Linux 验收边界。

## 交付与验收

核心迁移和 Agent Hook 接入分为两个 PR。当前 PR 包含 Rust 核心、公共 Action Runtime 审计、
daemon、CLI、SkillFS 和 Linux 部署。Agent Hook 实现、能力视图、Hook 默认配置与真实 Agent
验收属于下一个 PR。消费者请求和响应样例仅证明接口合同，不证明 Agent 已接入。
第二阶段统一策略体系不在当前 PR 范围内。
下表记录已实现批次及此前检查，不代表完整 V1 一致性；四项迁移修复与本轮验收见下文。

| 批次 | 职责 | 实现状态 | 进入下一批前的验收 |
| --- | --- | --- | --- |
| 1 | 类型、canonical 身份、系统密钥、Integrity | Linux 能力测试已通过 | 签名、篡改与重放拒绝，密钥权限，源目录与快照路径规则 |
| 2 | Scanner 与 analyze | Linux 能力测试已通过 | V1 结果对照，选择与别名，覆盖不足，错误，不写账本 |
| 3 | Ledger 与 Service | Linux 能力测试已通过 | 版本、补扫与强制扫描、快照、导出、串行化、扫描期间内容变化 |
| 4 | Activation | Linux 能力测试已通过 | 决策、active/pending/hidden、回滚、发布失败与启动 reconcile |
| 5 | daemon、CLI 与审计 | Runtime 与源码安装验收已通过 | 真实 CLI 请求、输出和退出码、peer 身份、审计、超时、管理员换钥、消费者样例 |
| 6 | SkillFS | Linux 真实 IPC 与 FUSE 验收已通过 | 单 socket、HMAC notify/resolver、拒绝降级、真实 FUSE 效果、普通 IPC 回归 |
| 7 | 部署 | 源码／RPM／systemd 验收已通过 | 源码与 RPM 安装、root systemd、普通本地用户调用、核心完整流程 |

每批形成一个独立编译的逻辑 commit，同时包含测试和文档。本批引入的问题修回本批 commit。
必须在 Linux 上验收；macOS 格式化或 manifest 检查不能替代构建与运行验收。

## 公共执行与 OTel 对齐

当前固定主干基线为 `e60de761e9acb01a14990cb09272cbe75c08d1bd`，已包含 PII 迁移
与离线 RPM 构建支持。库 crate 统一使用 `v2/crates/asc-*/`，保留七个业务提交。
Code Scan、PII 和 SkillSec 由同一进程装配到 ActionService，共用 Finalizer；
PII 集中规则与 SkillSec 配置分别加载。下文此前的 Linux／RPM 结果仍绑定各自记录的版本。

```text
CLI / RPC -> Handler -> ActionService -> ActionRuntime -> SkillSecExecutor -> SkillSecService
SkillFS notify -> 认证队列 / worker -> ActionService
普通目录配置 -> 启动发现 / 同一个 worker -> ActionService
启动恢复 ---------------------------> ActionService
```

`asc-action-types` 提供共享命令、canonical 身份和决策类型。执行请求仅包含业务命令和服务端
注入的调用 UID，物理目录、句柄和密钥留在能力实现内。Handler 解析协议并投影响应；Executor
通过窄的 `SkillEnvironment` 端口选择、解析目录，再由 Service 执行既有业务转换。认证映射
不可用时不回退访问 FUSE 可见目录；沿用不可用根目录表示，保留聚合操作中的逐 Skill 错误。

daemon 启动层统一组装 Service、Executor、Projector、Runtime 和共享 Finalizer。
SkillFS 位于 `asc-daemon::skillfs`，复用现有 dispatcher/session 接口。启动顺序为准备 resolver
与队列、注册 ActionService、执行恢复、启动 daemon 所有的 worker、开放 socket。
即使未配置 SkillFS，worker 也会在后台线程执行有期限的发现与扫描。Executor 只持有解析与健康
查询资源，不持有 worker。配置、私有状态或 bridge 初始化及 worker 启动失败
阻止准入；换钥恢复未完成时保留围栏，允许 status／管理员重试；单项 reconcile 失败记录诊断
并继续其他项。

普通请求沿用主干顶层 OTel carrier、兼容标签和 request scope。UID/GID/PID 来自内核；Agent
baggage 仅用于关联，不授予权限。每个出队通知和启动恢复项建立独立 daemon Context；scan、
activation 和 Busy 重试仍是不同执行调用。Runtime 对意外异常收尾一次后返回 `InvokeError`，
入口不再写第二条终态。analyze 不写账本但记录调用审计。审计、telemetry、诊断共用公共
Finalizer，保持既有 telemetry 字段白名单和各输出失败隔离。

SkillFS 保持 HMAC／notify 消息合同。认证会话先于普通请求分流，仍使用有界五秒交互；普通
SkillSec 执行默认 60 秒，上限 120 秒，其他方法沿用配置。关闭顺序为停止 UDS 准入并排空请求，
并行等待 SkillSec worker（65 秒）与 PAP（30 秒），再关闭外层 Runtime、持久化 sinks 和 OTel。
等待中的通知通过重启后扫描登记 Skill 恢复，不提供持久队列或 exactly-once 保证。

Backing mount 先创建分离克隆，通过 `/proc/self/fd` 设为 private，再由 `move_mount` 发布，
防止已运行 daemon 收到的副本被后续原位 FUSE 覆盖。daemon 无需新增挂载 capability；操作
不支持或被拒绝时直接失败。backing 传播回归和真实 FUSE 效果已在 Linux 测试机验收。

## 本轮迁移问题修复

`skillsec-four-fixes-20260929` 基于原 PR HEAD `0d252302a3de428508998ae3acc13422a2afcc22`
及上述固定主干，验证独立 review 确认的四项问题。环境为 Alibaba Cloud Linux 4.0.4、
Python 3.11.6、Rust 1.93.1。候选 3 的归档 SHA-256 为
`46f165bcac690850ee33966eba18d11eea10d487e568b3b4822bd2acefad5669`。
随后 overlay 仅修正测试语法及精简 worker 测试夹具；其摘要和定向复测保存在运行记录中。
候选 3 之后没有产品行为变更。

| 问题 | 修复与验证 |
| --- | --- |
| P1：Unicode I 匹配 | 固定 static 规则显式保留 Python 的 I/i/İ/ı 等价类，包括字符范围和负向断言，不改写原文。91 组 V1 scanner/analyze 样例全部通过，其中新增 41 组；Service 和真实 CLI 验证 deny、原始快照及不激活新的风险版本。 |
| P2：findings 清理 | CLI 在单次 JSON 输出前附加 `findingsDeleted`／`findingsDeleteError`。真实 UID1001 清理失败仍保留认证提交和 exit 0；覆盖成功删除、保留输入及请求被拒绝的情形。 |
| P2：只读系统批处理 | batch/init 共用基于目录 FD 的 daemon 写权限预检，仅针对已授权、host-backed 的两个默认系统根的直接子 Skill。真实 EROFS 挂载验证 skip、不写 metadata、root 可写控制组，以及显式、范围外和非默认路径继续严格失败。 |
| P2：普通目录启动扫描 | daemon 独立于 SkillFS 持有原有 worker，共享发现逻辑将配置及已登记的授权 Skill 送入原有 ActionService scan/activate 循环。真实无 SkillFS 重启覆盖内容变化、新 Skill、风险和未变化情况；阻塞扫描单测验证启动不阻塞及关闭跟踪等待。 |

- 最终 V2 格式、严格 workspace／all-targets Clippy、rustdoc 和三项架构检查通过。
  完整 `cargo test --workspace --locked --no-fail-fast` 为 **951 通过、1 失败、3 忽略**。
  唯一失败仍为主干未修改的 event-log root-DAC 夹具；降低 DAC 权限后实际执行该单项并通过。
  原始失败命令保留，不改记为全绿。
- 新增的 ignored EROFS Rust 用例在原生 Linux 私有挂载命名空间中另行执行，**1/1 通过**；
  其他 ignored 用例不计入已执行。最终仅含测试的 overlay 通过 CLI 清理回归和三项 worker
  测试，不改变产品代码。
- release 构建、隔离目录源码安装及布局检查通过。安装态核心／公共套件为
  **898 通过、38 跳过、0 失败**，覆盖 SkillSec、Code Scan、PII、PAP 和 IPC。
  初次 debug／并行 E2E 为 93 通过、3 失败；补齐共享 daemon 的 release 环境完整重跑后，
  三项均通过，没有修改产品 deadline。
- 真实 root 原位及 UID65534 FUSE 两种拓扑通过认证、激活、SkillFS PID 不变的 daemon
  单独重启；root 用例还验证 notify 到扫描、block／rollback 暴露字节、错误 UID／密钥／
  明文拒绝和 resolver 不可用时禁止回退。

四项已确认问题完成修复；这些定向用例不代表证明了 Python／Rust 的所有 regex 语义一致，
也不代表真实 Agent 执行验收。本轮未重跑原生 RPM 安装及未改动的完整 SkillFS workspace。
Hook 和第二阶段策略继续留在后续工作。
若较早的 PR 候选曾生成错误的 pass/warn 记录，更新 daemon 不会重写这些历史：对受影响的
当前内容使用 `--force --scanners static-scanner` 补扫，并检查历史可激活版本；需要隔离时
使用现有 block／版本决策，不将重扫最新内容当作清除了全部历史风险。

## 历史范围验证


`skillsec-scope-20260929` 在当前固定主干上验证受管目录修复，环境为 Alibaba Cloud Linux
4.0.4、Python 3.11.6、Rust 1.93.1。已测试候选 5 的归档 SHA-256 为
`225304fa34af7a9c7b8d02cdfc11da1621644d4bb6d2cc7a1afc60889b4862c5`。
修复归回原七个提交后，代码 HEAD `2b101a9b` 的文件树完全一致；之后验收记录与 CI 夹具更新不改变产品代码。

- V2 格式、严格 workspace／all-targets Clippy、rustdoc 通过。完整 root workspace 测试
  为 **949 通过、1 失败、2 忽略**。唯一失败是主干未改动的 event-log DAC 夹具；
  reduced-DAC 复测实际执行一项并通过。原始完整命令仍记录为失败。
- 七项范围回归及 root／UID1001 的真实 CLI 验证通过：精确路径、`/*`、`/**`、动态子目录、
  隐藏目录与符号链接排除、副作用前整批拒绝、旧注册记录重启，以及中断换钥的授权边界。
- release 源码全新安装、打包检查和安装态核心／公共能力套件通过：**898 通过、38 跳过**。
  覆盖 SkillSec CLI、PII、Code Scan、PAP 和普通 IPC。跳过项为 36 个 Python 规则源码用例、
  1 个既有 Code Scan telemetry 用例、1 个未配置 LLM 用例；不包含 Agent Hook 部署。
- 七个提交分别通过 `cargo check --workspace --all-targets --locked`，第六、七提交另通过
  SkillFS 对应检查，共九项独立构建检查。
- 真实 FUSE 验证 root 原位和 UID65534 挂载、公共 socket 权限、认证解析、激活，以及 SkillFS
  PID 不变的 daemon 单独重启。root 用例另外验证修改到 notify 到扫描、block／rollback 的实际
  暴露字节、错误 UID／密钥／明文拒绝，以及 resolver 不可用时禁止回退。

独立 review 接受目录范围修复，并发现上文本轮已修复的四项迁移差异。原始 Unicode 证据
证明签名 pass 及 activation 发布，不代表已观测到 Agent 执行。RPM CI 夹具只授权独立
pytest 根目录；该范围验证未重跑原生 RPM 安装及未改动的完整 SkillFS workspace，
其结果仅作为对应版本的历史证据保留。

## 历史 Linux 验收（PII 合入后）

`skillsec-ready-20260928` 基于主干 `08e6be80c86ab3d41e9081967f37afcbdd59b197` 验收代码 HEAD
`72d50d82b5d4cde2d9d5fd5ef36620e291e3c8c6`。环境为 Alibaba Cloud Linux 4.0.4、
Python 3.11.6，V2 使用 Rust 1.93.1，SkillFS 使用 Rust 1.86.0。证据绑定源码树、补丁等价性、
已安装二进制及 CI 产物摘要，不以历史版本的结果替代本轮验收。

- V2 格式、严格 workspace／all-targets Clippy 及 rustdoc 通过。完整 root workspace
  复跑结果为 **942 通过、1 失败、2 忽略**。唯一失败是主干未改动的 event-log DAC 权限夹具，
  去掉 DAC override 后该单项通过；原 root 命令仍记录为失败。七个提交分别独立编译，
  第六、七提交还检查 SkillFS，共九项检查全部通过。
- release 源码安装和准确 CI RPM 独立全新安装后的完整 installed-only 套件均为
  **1,204 通过、45 跳过**。同一 daemon 处理带自定义集中规则的 PII、Code Scan 和 SkillSec，
  验证独立审计记录及 analyze 不写账本。覆盖已有 PII Hook 子进程合同，不启用或验收真实 Agent。
- SkillFS 格式、严格 Clippy、rustdoc 和 workspace 测试通过，报告 **1,620 通过、8 忽略**。
  原生 FUSE smoke 另行验证 managed worker／supervisor 恢复及清理。root 与普通用户拓扑
  通过 HMAC／resolver、notify 到 activation，以及 SkillFS PID 不变的 daemon 单独重启；
  root 用例还覆盖 block／rollback 和错误身份、密钥、明文拒绝。受环境保护的 workspace
  计数不代表全部实际执行了 FUSE 操作。
- 原生 systemd 生命周期及 UID1001 在合成 HOME、临时、共享 Skill 根目录上的业务通过，
  包括私有导出归属、非管理员换钥拒绝、重启后信任和审计保留。源码安装保留用户配置并隔离 V1 单元。
- 准确 RPM 来自 CI run `36421070474`、artifact `10970226616`，包完整性检查通过。
  CI 安装套件同样为 **1,204 通过、45 跳过**，完整 V2 任务成功。CLI RPM 的 SHA-256 为
  `857bb0420111553061e0c2ce9c875a5117ca268eef2837a7f5e82ab7348fcbec`。
- 该代码版本的 PR CI `Test agent-sec-core` 通过，包含此前失败的两处 CLI Clippy 检查及
  后续 V2 Rust 测试。修复仅将内部启动配置改为 Box 存储并抽出原有 UID 解析，不改变 CLI 合同。

失败的准备尝试保留在运行证据中：修正本轮构建目录权限及容器缺失工具后均完整复跑。
Agent Hook 迁移、真实 Agent／模型验收和第二阶段策略继续留在后续工作；历史回退证据保留如下。

## 历史 Linux 验收（PII 合入前）

`skillsec-d-20260928` 使用 Alibaba Cloud Linux 4.0.4、Python 3.11.6，V2 使用
Rust 1.93.1，SkillFS 使用 Rust 1.86.0。源码从 `8e3f1a0b` 开始，验收修复归回原所属
提交。验收代码 HEAD 为 `87113892a7729ec1403c69face98f25d43b29081`，后续仅更新本组
双语验收文档。运行记录保留准确补丁、源码树、二进制、命令与日志。

- V2 格式、严格 Clippy 和 rustdoc 已通过。root 下 workspace 测试为 892 通过、1 失败、
  2 忽略。唯一失败是已有日志测试依靠 chmod 制造 rename 错误，但 root 的 DAC 权限绕过
  了该限制；固定主干同样失败，当前版本去掉 DAC override 后该单项通过。
  不能把原始 root workspace 命令记为全绿。
- release 源码安装及安装后的 CLI／daemon 套件通过：953 通过、45 跳过。普通用户导出、
  拒绝换钥、审计与核心 Skill 操作另行通过。迁移后的 CLI 曾在成功输出前获取 stderr 锁，
  可能被诊断线程的满管道写入阻塞；移除提前加锁，恢复主干输出方式，并在既有满管道用例中
  增加 SkillSec 检查，保持原超时不变。
- SkillFS workspace 检查通过：报告 1,619 通过、8 忽略。随后用真实 FUSE 执行受环境保护的
  集成测试，28 个目标均成功，报告 507 通过、1 忽略；其中 3 个环境跳过项及被忽略的
  in-place backing-root 用例已单独补验。
  两组覆盖有重叠，不能相加当作独立用例数。backing 传播回归已显式通过。
- root 原位与普通用户 FUSE 挂载通过通知、扫描／激活、block／rollback、错误 HMAC／身份／
  明文拒绝及 resolver 失败不回退。两种拓扑均在仅 daemon 停止期间写入后恢复，SkillFS PID
  保持不变。仅清除最新记录的决策后，历史 rollback 决策仍可生效，与 V1 行为一致。
- 原生 systemd 生命周期通过，覆盖重启、排空、75 秒强制停止期限和启动限流。普通用户在
  发布配置的 capability 限制下操作合成 HOME、临时及系统 Skill 目录；挂载卷由 FUSE 用例验证。
- `8e3f1a0b` 的准确 CI RPM 已通过安装及 952 项安装测试（46 跳过），并完成隔离的
  V1 → V2 → 配套 V1 包、配置和状态恢复。这些 RPM 结果早于 CLI 锁修复。修复版来自
  `87113892`、CI run `36387153042`，CI 与测试机独立全新安装均为 953 通过、45 跳过，
  满 stderr 和跨 UID 回归均实际执行。CLI RPM 的 SHA-256 为
  `69797d1a9dbba14f3ff6d3a5f29f01d4f7b86ce20a083b81a6a1b402e64308b4`。

七个提交的 V2 workspace／all-targets 独立编译全部通过，第六、七提交的 SkillFS 编译
也通过，共九项检查。Agent Hook、真实 Agent／模型验收和第二阶段策略接入继续排除在
本 PR 外。之前无关的 setup-uv 收尾缓存失败未作修改，修复版 V2 RPM 任务本次全部成功。
失败尝试及其版本身份保留在运行证据中。

## 保留的业务能力

| 能力 | V1 源码基准 | 归属与批次 |
| --- | --- | --- |
| 初始化、状态、扫描器列表 | `core/status.py`、`config.py`、`cli.py` | Service/CLI，3、5 |
| 内置扫描与只读 analyze | `scanner/skill_code_scanner.py`、`scanner/builtins/cisco_static/`、`analyze.py` | Scanner，2 |
| 外部 findings 认证 | `scanner/parsers.py`、`core/certifier.py` | Scanner/Ledger，2、3 |
| 签名与文件摘要 | `signing/`、`models/manifest.py`、`core/file_hasher.py` | Integrity，1 |
| 版本历史、补扫、强制扫描与快照 | `core/certifier.py`、`core/version_chain.py` | Ledger/Service，3 |
| check、audit 与快照校验 | `core/checker.py`、`core/auditor.py` | Integrity/Ledger，3 |
| show、export、rollback、人工决策与 clear | `core/decision.py`、`core/exposure.py` | Ledger/Activation，3、4 |
| resolver、激活发布、后台变更处理 | `core/live_root.py`、`core/resolver.py`、`activation_policy.py` 与 daemon SkillFS 集成 | Activation/daemon，4、6 |

表中路径相对于 `agent-sec-cli/src/agent_sec_cli/skill_ledger/`。
V1 源码提供业务行为对照，不作为 Rust 运行时的回退实现。完整性状态仍为 `none`、`pass`、
`warn`、`deny`、`drifted`、`tampered` 六种；执行错误和激活状态分别表示。
`skill-ledger` CLI 业务入口及消费者所需字段、结果与退出码均需验收。

## 已确认的兼容性变更

1. **信任与存储：** daemon 管理统一系统签名密钥，不依赖调用者 HOME 或 passphrase。
   私钥采用私有 PKCS#8 文件，不沿用 V1 加密 seed/keyring 布局，不导入 V1 记录和密钥。
   首次缺失密钥可原子创建；已有密钥损坏或不安全时返回错误，不自动替换。
2. **Manifest：** 新记录使用 `version: 2`，增加被签名覆盖的 `canonicalSkillDir`。
   绑定 canonical 绝对源路径，不以 Skill 叶名称或实际 backing 目录名代替身份。
   canonical JSON 递归排序 key、使用紧凑 UTF-8、排除 `manifestHash` 与 `signature`，
   然后计算 SHA-256；Ed25519 签署 UTF-8 的 `sha256:<hex>` 字符串。这是新记录合同，
   不承诺 V1 字节兼容。SkillFS 的协议版本独立，保持不变。
3. **换钥：** 仅管理员可轮换系统密钥，不保留旧公钥回退，换钥前的 V2 记录同样不继续验签。
   换钥必须撤销旧 activation，再通过重新扫描、签名和激活建立信任。
   第一批的密钥初始化 API 尚不实现换钥。
4. **授权：** 当前允许所有本地用户操作所有受管 Skill。受管目录配置定义管理范围，
   不是按归属判断的 ACL。用户隔离留明确 TODO，不提供任意字节签名端点。
5. **运行时：** 一个 root daemon 是唯一写入者；Rust CLI 调用 daemon，不回调 Python Ledger，
   不在 daemon 缺失时本地执行。当前 PR 保持 Agent Hook 实现不变。

## 第一批的完整性边界

`SkillIdentity` 验证已展开的绝对词法路径，拒绝歧义分隔符、点路径、父目录跳转、NUL 和
非 UTF-8 路径。物理 I/O 路径单独解析，使 SkillFS live 目录与 canonical 源路径共享身份，
后续使用同一写锁。摘要计算接收明确解析后的物理根目录，不自动跟随路径组件中的符号链接。
源目录跳过 `.git`、`.skill-meta`、符号链接和特殊文件，快照校验则拒绝这些项目。
相对于目录描述符打开文件，防止枚举后被替换为符号链接；计算摘要前后检查文件元数据。
这一层本身不证明扫描一致性：第三批还需对同一暂存内容扫描并生成快照，提交前重新检查 live 内容。

签名目录必须已存在、属于服务 UID，其他用户不可访问。密钥必须是单链接的私有普通文件，
加载时拒绝符号链接、硬链接、错误属主、过大文件和非法 PKCS#8。
初始化采用独占临时文件、文件 sync、不覆盖目标的 rename 和目录 sync，并发首次初始化不会
替换胜出者的密钥。验签先检查记录约束、canonical 身份、摘要、算法、当前密钥指纹与 Ed25519
签名，通过后才能信任文件摘要和决策。

第一批集成测试位于
`v2/crates/asc-capability-skill-sec/tests/integrity.rs`。
其中 `fixtures/integrity.json` 使用公开的合成 seed（字节 0 到 31），由 Python 标准
JSON/SHA-256 和 OpenSSL Ed25519 独立生成。Rust 测试必须能验证该签名，并产生相同摘要和签名，
覆盖嵌套 metadata 排序与 Unicode。其他测试覆盖签名字段篡改、跨 Skill 与跨密钥重放、不安全
密钥、并发初始化、歧义身份和源目录/快照条目处理。

## 后续编排与恢复

Service 从读取旧状态到发布全程按 Skill 串行化。Scanner 产生 findings，Integrity 验证内容
与记录，Ledger 存储版本和快照并导出，Activation 选择和发布可见内容。传输层负责可信调用者
身份和协议解析，不复制领域状态转换。公共 Runtime/Finalizer/Sink 接收明确筛选的安全审计投影，
与完整业务输出分开。

持久化采用单文件原子替换、回滚备份和启动 reconcile，分别表示账本提交、激活选择和实际
SkillFS 效果。不引入多文件事务引擎、持久队列或 exactly-once 承诺。

SkillFS 使用公共 V2 socket，权限检查适配系统 socket 布局，同时保留可信属主、实际 peer
身份和 HMAC 密钥检查。按首帧区分普通 V2 请求与现有 HMAC 握手，仅适配旧 notify 合同，
不因此开放其他 V1 RPC 方法。

对于 `init`、`scan` 和 `certify`，Service 负责参数校验及后续密钥操作。Executor 通过公共
运行时委托这些命令；非法扫描器名称，以及认证时格式错误的 findings，均在创建或轮换密钥前
失败。公开 Service 和独立 Scanner 入口校验输入，内部单 Skill 扫描使用明确的扫描器选择，
不重复检查名称；baseline 处理多个 Skill 时也遵循同一规则。

已有密钥的初始化检查使用共享 generation 锁。仅在密钥缺失时释放读锁，取得独占锁并再次检查
后创建。换钥保留独占边界，同一 Skill 的写操作仍由自身的锁串行化。密钥权限、换钥状态和内容
身份仍在实际使用时检查。

## 回退边界

第一批尚未注册 daemon 方法，不改变安装或线上状态；回退 crate 和 workspace 注册即可移除。
完整迁移替换部署时应单独保留 V1 状态，V1 无法读取 V2 manifest 或系统密钥。
回退部署必须同时恢复匹配的状态与配置，不能让两个实现同时写一个账本。

## 第二批 Scanner 边界

`ScannerRegistry` 依次在进程内执行 `code-scanner` 和 `static-scanner`。Code Scan 复用 V2
能力，扫描 Python、Shell 和带受支持 shebang 的无扩展名文件。Static Scan 内嵌原有十条规则，
并检查元数据、链接、隐藏文件及疑似凭证文件、二进制资源和未声明联网行为。符号链接会产生
finding，但不读取目标内容。与 V1 相同，自定义扫描器仍通过外部 findings 导入；`cli`/`api`
注册信息不会让 root daemon 执行调用者提供的命令。findings-array 保留额外证据字段，并返回
可见的规范化警告。新输入拒绝已废弃的扫描器名称。

`analyze` 不依赖密钥或历史登记，但要求路径处于配置授权范围内，不写账本。完整扫描的 `pass`/`warn`/`deny` 均返回退出码
0；覆盖不完整返回 `error` 和退出码 1；非法根目录或缺少正规 SKILL.md 返回退出码 2。超过截止
时间是执行超时。分析结果的嵌套证据会脱敏物理源路径、HOME 和临时/XDG 目录。后续 CLI 适配器
负责展开用户路径，发送绝对路径请求。

V1 analyze 的 2,000 个正规文件、总计 50 MiB、目录深度 32 限制也用于内置账本扫描；超限时扫描
失败，不会认证不完整的目录清单。Code Scan 单文件限制为 1 MiB；Static Scan 使用配置项
`maxFileBytes`，默认 1,000,000。元数据解析复用现有 YAML tokenizer，保留 V1 非引号布尔值、
重复键覆盖和常规 anchor/merge 语义；拒绝递归或过大的元数据（深度 32、展开后 10,000 节点、
标量内容 8 MiB）。这些资源限制保护共享 daemon。诊断文字可随实现语言变化；规则标识、风险
级别、证据和覆盖结果属于验收合同。

目录枚举在首次超限时停止，整棵树最多收集 20,000 个名称，目录、链接和特殊文件均计入。
排除的目录只计一个名称，不遍历其内容。限额元数据以 `truncated: true` 表示已观察到的部分计数，
不代表整棵树总量。analyze 在同一个目录描述符上先检查正规 `SKILL.md` 再遍历；签名、内容复查
和回滚拒绝不完整清单。带引号或显式字符串标签的 YAML `<<`（包括指向此类键的 alias）保持
普通键语义，只有合并键才合并映射。

`tests/reference_scanners.py` 将带源码版本及 SHA-256 的 V1 结果冻结到
`tests/fixtures/scanners.json`。50 组样例同时比较两个内置扫描器和 analyze，涵盖误报抑制、
Unicode、元数据、符号链接、目录排除及覆盖不完整。仅归一化耗时、引擎版本和语言/平台诊断文字；
风险结果与证据仍逐项比较。Rust 测试还覆盖扫描器选择、禁用和仅导入项、解析器回退、旧名称、
非法输入、资源限制、截止时间及不写账本。样例生成脚本仅用于开发测试，部署的 Rust 程序不调用 Python。


## 第三批 Ledger 与 Service 边界

`SkillSecService` 按 canonical Skill 身份加锁，直接路径和已验证的映射路径共用同一把锁，
闲置锁条目会释放。操作持有密钥代际读锁，第五批换钥时使用写锁。受管目录精确登记在 daemon
私有状态中，不会把用户指定目录的同级 Skill 自动纳入管理。
业务根目录不得位于 `.skill-meta` 内，包括已认证映射的物理路径；内部快照校验不经过该业务根入口。

扫描先捕获最多 2,000 个普通文件、50 MiB、10,000 个目录、32 层深度，排除 `.git` 和 `.skill-meta`。
扫描使用私有暂存目录，另行保留原始符号链接分类供 static scanner 报告；临时快照写入并校验后、正式发布前重新检查 live
字节、普通执行位、目录、链接和根目录身份。快照保留空目录，移除 setuid/setgid 位。新快照先于
签名版本和 `latest.json` 发布；每个文件使用独占临时文件、fsync 和原子替换。多文件发布中断可被
检测，启动 reconcile 与回滚编排在第四批交付，本批不宣称完成。

仅在 latest、版本记录和快照均验证通过时复用未变化版本；补扫添加缺失扫描器，force 在同一版本
替换扫描结果。内容变化或记录损坏时创建新版本，连接到最近的完整可信前驱。JSON 和快照都会占用
版本槽位，不可信的高编号不会造成编号跳跃。`check` 比较 live 内容与最新可信记录，不要求快照；
`audit` 校验父签名，并可额外检查快照。不可信记录不能提供对外展示的元数据。
没有任何账本工件时，即使尚未初始化密钥，`check` 仍返回 `none`，空历史 `audit` 仍成功。
这些只读查询不创建密钥或账本；已有工件仍须通过认证。

登记在签名提交之后、激活发布之前。首次登记失败时，请求报错且不发布激活，但版本可能已经提交。
启动恢复仅枚举已登记根目录，并拒绝当前配置范围外的目录。修复所报告的失败原因后，对未变化内容重试 `scan`，会复用可信版本并
完成登记；不承诺为尚未成功响应的首次请求提供持久化发现队列。

导出从可信快照生成 `snapshot/`、`manifest.json` 和 `findings.json`。调用者须先在 Skill 与
状态目录之外创建自己的空输出目录；Service 校验真实 peer UID、目录类型和写权限。daemon 不以
root 创建任意目标父目录，不跟随符号链接，也不截断已有目标文件。新建导出文件和目录归真实调用者
所有，调用者可编辑或清理导出结果；账本和快照存储仍由 daemon 管理。`active` 选择与回滚决策流程随
第四批 Activation 一起加入。

`tests/reference_ledger.py` 固化了十组带源码哈希的 V1 工作流，Rust 对照业务状态、版本号、扫描
结果合并、文件数、变化列表与审计结论。V1/V2 密钥、manifest 格式和签名按已批准的破坏性变更处理。
另有并发认证、别名串行化、等待超时、暂存后内容变化、缺失或伪造记录、安全导出及精确注册测试。
本批尚不注册 daemon 或 Hook 接口。


## 第四批 Activation 边界

Service 已提供 `decide`、`clear_decision`、`show`、`activate` 和 `rollback`；scan/certify
在释放同一 Skill 写锁前发布激活。`allow`、`always_allow`、`block`、`rollback` 保留 V1
选择规则，只有 `always_allow` 继承到新的内容版本。`active` 导出已选择的可信快照。
`show` 只读，分别呈现 latest/active、源目录一致性、findings 和有长度限制的审核说明。

发布写入 SkillFS 使用的最小 schema-1 `activation.json` 与目录 xattr；目标是已验证快照、
安全的待审核占位目录，或显式 block 对应的 null。`contractWritten`、
`activationXattr.written`、`activationPending` 区分业务提交与发布完成。xattr 失败不会撤销
已签名决策；activate 或启动 reconcile 可重试。发布成功不等于已证明真实 FUSE 暴露效果。

回滚扫描已捕获的可信快照，备份当前目录，在替换源内容前写入 daemon 私有的单 Skill 恢复意图。
备份保留嵌套 metadata 目录及符号链接文本而不跟随目标；特殊文件或超限目录在替换前报错。签名快照仍排除符号链接
和特权执行位。仅准备完成的意图不会覆盖后来的用户修改；开始替换后，匹配的签名版本提交前
失败会恢复备份，提交后则由 reconcile 修复 latest，不撤销已提交版本。恢复校验备份摘要和链接
文本，拒绝损坏备份；备份保留供显式检查。启动 reconcile 也会在快照有效时修复已认证版本与
latest 的分裂，清理遗留内部临时项并重新发布。不引入自动历史清理策略或通用事务引擎。

`tests/reference_activation.py` 冻结十二组带源码哈希的 V1 流程。Rust 对照选择结果、人工决策、
回退、漂移、回滚、导出和 show 说明。Linux 测试覆盖真实 xattr、文件/xattr 分裂失败、回滚提交
失败、源替换中断（含 SKILL.md 缺失）、备份损坏和已提交意图恢复。daemon 启动循环及真实 SkillFS
消费者在后续批次集成。

## 第五批 daemon、CLI 与审计边界

`action.skill_sec` 接受封闭的 `command` 枚举，是此前候选 `action.skill_ledger` 的明确 V2
替代；不开放任意 Action 调用或普通 V1 RPC envelope。响应包含 `success`、`exitCode`、`error`、
`errorType` 与业务 `data`。Rust `skill-ledger` CLI 输出业务对象并采用显式退出码，不读取 daemon
密钥、不回调 Python、不在本地执行扫描。路径展开、findings 文件读取与删除、导出目录创建均以
CLI 调用者自身权限执行。

支持 `init`、`check`、`analyze`、`scan`、`certify`、`status`、`audit`、`list-scanners`、`decide`、
`show`、`export`、`rotate-keys`，以及发布重试命令 `activate`。`init --no-baseline` 只建密钥；
`init --force-keys` 与 `rotate-keys` 要求内核 UID 为 0。旧 `init-keys` 和用户口令 `--passphrase`
不属于系统密钥接口。`scan/certify` 可首次创建缺失密钥，但不会替换损坏密钥。`check` 的
 deny/tampered/error 退出 1；scan/certify 与完整 analyze 的风险结果退出 0。analyze 覆盖不足退出 1，
非法输入退出 2。已提交操作可能退出 0 且 `activation.activationPending=true`；消费者必须检查
发布状态，不能仅凭退出码声称发布或 FUSE 生效。

进程默认 socket 为 `/run/agent-sec-core/daemon.sock`，可由 `--socket` 或非空
`AGENT_SEC_DAEMON_SOCKET` 覆盖。运行目录要求服务所有、0700/0750/0755，通过私有 flock 文件
保持单实例；仅清理已验证所有权且连接返回 refused 的残留 socket。进程端点为 0666，嵌入式服务
默认仍为 0600。这部分定向对齐尚未合并的系统服务 PR #3217（`5d2ff1f`）；不意味着该 PR 已合并，
也不采用其服务身份设计。

root 所有的 `/etc/agent-sec/skillsec.json` 或 `--skillsec-config` 指定 `stateDir`、精确
`managedSkillDirs`、scanner 覆盖配置与 parsers。默认状态目录 `/var/lib/agent-sec/skillsec`
要求 root 所有、0700，当前密钥保持 0600。不导入用户配置、旧历史和 keyring。
`managedSkillDirs` 保留精确路径、末尾 `/*` 和末尾 `/**` 三种形式，与请求中的精确身份分开解析，
不增加 `allowedSkillRoots`。递归发现包含有 `SKILL.md` 的模式根目录自身，跳过隐藏后代，拒绝符号
链接遍历；每次聚合请求重新发现，因此通配范围内新增子目录无需重启。配置采用绝对路径，不展开 HOME。

Executor 在物理解析和业务副作用前，拒绝不属于配置模式或已认证 SkillFS 挂载范围的调用路径。
analyze、导出、后台工作及普通启动 reconcile 共用这个边界。聚合发现合并配置模式和仍受授权的
登记历史，调用端发现不能扩权；一个越界调用路径就会使整个批次在处理任一 Skill 前失败。
空 check/scan --all 仍为执行失败，不创建密钥；调用端发现限制为 1024 项。登记只记录运行历史，
不能作为授权来源。status 使用当前配置和仍受授权的历史，不随调用者 HOME 改变。
配置挂载子树不经 FUSE 遍历，其精确目录由认证通知和已有登记提供。

新的换钥若需撤销已移出范围的历史暴露，root 须先恢复该范围配置。已有的私有换钥恢复意图仍可在
配置变化后完成撤销；例外仅适用于 root 换钥恢复，不允许对当前范围外目录发起新的 baseline 扫描。

CLI 的共享发现逻辑包括 `$XDG_DATA_HOME/anolisa/skills` 下的直接 Skill 子目录，遵循安装器的
路径规则：未设置、为空、相对路径或原始路径含 `.`/`..` 段时，回退到
`$HOME/.local/share/anolisa/skills`；合法但不存在的目录直接跳过。`HOME` 未设置或为空时，
使用 CLI 当前用户的系统账户主目录。只有 CLI 读取调用者环境，
`init --no-baseline` 和显式路径请求不触发发现，隐藏目录及快照过滤保持不变。

换钥持有全局代际写锁，写入私有 intent，撤销全部登记 Skill 的 activation 后才替换密钥。
有未完成回滚时须先 reconcile。撤销失败保留旧密钥，并阻止普通账本操作，直到管理员重试或启动
恢复成功。intent 仅保存旧密钥指纹和 canonical Skill 身份；启动恢复、`rotate-keys` 和
`init --force-keys` 在撤销前重新解析当前物理目录及 inode，Service 要求映射完整覆盖记录中的
Skill 集合。解析失败保留 intent 和旧密钥，允许重试；指纹已经变化时无需再解析映射，只清理
intent，不会再次换钥。启动恢复经过公共 Action Runtime；单个 Skill
恢复失败可见，但不关闭其他 daemon 方法。

公共 Finalizer/Sink 仅记录受控的 command、数量、判定状态、版本和执行错误类别；不包含原始
findings、源码、导入证据、人工理由、路径和密钥字节。完整业务结果仍返回客户端。最多同时执行
两个 SkillSec 请求以限制内容捕获内存，满载返回明确 Busy。默认预算 60 秒，timeoutMs 或
CLI --timeout-ms 在服务端最多 120 秒；其他方法保留原有预算。不自动重试请求。响应超过 3 MiB
返回 `ResponseTooLarge` 和 `operationMayHaveCommitted=true`，不静默截断数据、不撤销已提交
操作；findings 导入上限为 2 MiB。

`v2/fixtures/skillsec/consumer.json` 提供正常、风险、未初始化、超时、执行错误及激活未完成
样例。CLI 输出测试消费样例，Runtime 与真实 CLI 测试独立验证执行、调用者身份、换钥和审计脱敏。
这些证据不代表 Hook 接入或 SkillFS 生效。第五批 Linux 格式检查、严格 Clippy、工作区测试及
rustdoc 均通过，共覆盖 806 个不同测试。跨 UID CLI 用例在正常 root DAC 能力下运行，其他
工作区测试采用削减 DAC 的配置。独立 daemon 与 CLI 完成 25 次操作，覆盖重启恢复、换钥、
普通用户导出、PAP、公共 Code Scan，以及审计身份、风险判定和敏感内容脱敏。

第五批保留公共审计的 `result.verdict`：按命令投影，批量操作取最高风险结果，非判定操作不虚构安全结论。
单项 scan/certify/decide 保留 `keyCreated`；空 `check/scan --all` 返回执行失败且不创建密钥。
调用端单次发现最多 1024 个目录，已持久化的系统注册表不受此请求输入限制，避免阻塞状态查询和换钥。

root daemon 回滚后，恢复的普通文件与目录归属于源 Skill 目录的所有者，普通用户可以继续
编辑。快照仍去除特权权限位。跨 UID CLI 用例验证回滚后由真实普通用户写入原文件。

## 第六批 SkillFS 边界

`asc-daemon::skillfs` 负责兼容适配。通用 socket service 只增加可选的连接内会话接口，
并在帧之间保留缓冲区剩余字节。普通 V2 envelope 继续拒绝未知字段。认证会话只接受
`skill_ledger.skillfs_notify_change`，不能调用 PAP 或其他 V1 方法。

保留现有四帧 HMAC 握手、字符串 `authVersion="1"`、客户端与服务端独立 domain、原始 payload
加 `auth.frame` 标签，以及 notify schema version 2。daemon 将会话限制为四个入站帧、
每帧 64 KiB，首帧之后总计五秒。证明无效时直接关闭连接，不回退到明文。认证后的业务响应
均签名，包括拒绝响应。`accepted`、`queued`、`coalesced` 只表示内存工作状态；仅修改
`.skill-meta` 的通知以 `ignored=true` 确认。
合并事件仍返回 `queued=true`。保留 V1 的 `skill` 摘要、metadata-only `reason`、
daemon 生成的 `request_id`、`stdout/stderr/exit_code` 及结构化错误字段。
worker 意外退出会关闭通知准入并报告不健康状态，需重启 daemon 恢复。

SkillFS 认证客户端接受原有私有端点，或 root 所有的 `0755` 父目录加 `0666` socket。
握手前检查祖先目录归属、符号链接、可写权限、端点类型与归属，以及已连接进程的内核 UID。
HMAC 密钥仍为仅所有者可访问的 `0600` 普通文件。公共 socket 不授予管理员换钥权限，也不
改变 PAP 授权；第一阶段仍允许所有本地用户操作所有受管 Skill。

root 所有的 `--skillsec-config` 文件接受以下可选绑定。路径必须为规范化绝对路径，
同一挂载的 canonical/live 根可完全相等（普通非原地挂载），否则必须互不包含；不同挂载的
前缀不能重叠。daemon 和各 SkillFS 进程分别持有同一原始 HMAC 密钥的私有副本。
该密钥独立于 `stateDir/signing-key.pk8`；系统签名换钥和用户 HOME 都不决定 HMAC 密钥。

```json
{
  "stateDir": "/var/lib/agent-sec/skillsec",
  "managedSkillDirs": ["/home/alice/.openclaw/skills/demo"],
  "skillfs": {
    "authKeyFile": "/etc/agent-sec/skillfs-hmac.key",
    "mounts": [{
      "controlSocket": "/run/user/1000/skillfs/control.sock",
      "canonicalRoot": "/home/alice/.openclaw/skills",
      "liveRoot": "/home/alice/.openclaw/live-skills",
      "peerUid": 1000
    }]
  }
}
```

control 端点保持私有：配置中的 peer UID、仅所有者访问的父目录和 socket、内核 peer 检查，
以及现有 control HMAC domain。只读 `skill.resolveLiveSource` 的响应必须匹配配置中的两个
前缀、精确相对 Skill ID 和 `shared_path`。source/live 别名映射到同一个 canonical 锁身份。
打开 backing 目录时检查返回的 device/inode，进入 service 后再次检查。配置内解析失败不
回退读取 FUSE 可见视图。批量命令保留逐 Skill 的解析错误，`status` 仍返回密钥就绪状态。
换钥的私有恢复记录持久化目录身份；映射未解析成功时不会开始替换密钥。

一个 worker 按 canonical Skill 合并通知，默认 debounce 为 500 ms，持续编辑时最多延迟
两秒。它通过公共 Action Runtime 执行 scan，然后执行 activation；扫描失败或 noop 也不
跳过激活。只有能够证明尚未获准执行的 `Busy` 在期限内重试，提交后状态不明的失败不自动
重放。每个操作限时 30 秒。失败保留在审计、stderr 和 `status` 的 `skillfs` 计数中。
关闭时停止队列接收，等待当前一组有界操作结束。

daemon 同步恢复 rollback/latest/activation 后，由其持有的唯一 worker 根据当前精确路径、`/*`、
`/**` 配置及仍受授权的历史记录发现普通 Skill，也包括尚未登记的 Skill；同时补扫已登记的挂载
Skill，不要求 SkillFS 同时重启。发现复用 RPC 的有界、无符号链接遍历并跳过挂载子树。
启动列表去重后进入同一扫描／激活循环，慢扫描不延迟 socket 开放。实时通知最多保留
256 个不同 Skill，满队列返回签名拒绝。这是 Rust 相比 Python 无界 pending map 增加的
明确资源限制，不表示持久化接受。挂载启动通知补充新发现的 Skill；没有持久事件队列，也不
重放原通知顺序。

第六批测试覆盖冻结的 SkillFS HMAC 向量、同次读取中的多帧、错误密钥与 payload MAC、
明文拒绝、source/live 身份、错误 resolver 映射、backing inode 被替换、daemon 启动补扫、
扫描失败后的激活尝试和普通 V2 请求。合成 resolver 测试证明 IPC 合同；历史 Linux 验收（早于本次对齐）另外通过了
V2 工作区门禁、SkillFS 工作区测试及按既有用例环境前提执行的定向复验，以及仓库真实 FUSE smoke。
root 原地挂载和 UID 1001 普通挂载各完成 25 项真实 daemon/SkillFS 操作，覆盖认证通知与
resolver、发布、错误身份/密钥/明文拒绝，以及仅重启 daemon 后的恢复。
SkillFS Clippy 使用仓库固定的 Rust 1.86，V2 使用 Rust 1.93.1。这些是核心与 FUSE 结果，
不代表 Agent Hook 或安装后的 systemd 验收。

## 第七批：部署边界

V2 `install-core-v2` 安装 Rust 二进制、root 系统 unit 和初始私有配置。
V2 RPM 复用二进制与系统 unit 的安装目标，并使用 systemd 系统服务脚本宏。
V1 保留原用户 unit。源码安装和 unit 均不启用 Agent Hook，也不导入 V1 状态。
源码重装和 RPM 升级保留已有配置（`%config(noreplace)`）。签名密钥由业务操作初始化，安装过程不创建。

系统 unit 管理 `/run/agent-sec-core`（0755）、`/var/lib/agent-sec/skillsec`（0700）和
`/var/log/agent-sec`（0700），umask 为 0077。进程以 root 运行，仅保留 DAC override、CHOWN 和
FOWNER capabilities，并保留 `NoNewPrivileges`、`SystemCallFilter=@system-service`、native syscall
架构、`MemoryDenyWriteExecute` 和内核保护。HOME、`/tmp`、系统 Skill 根目录和
共享挂载保持可访问，因为它们是支持的内容位置。daemon 不具有 SYS_ADMIN。
systemd 测试容器所需的额外 namespace 管理能力属于测试运行环境，不属于产品服务权限。

V2 CLI RPM 不再依赖 Python、GPG 或 loongshield；未改动的 Hook 子包保留各自依赖。
仓库完整 RPM 配方仍构建插件包和 sandbox，OpenClaw 构建依赖要求 Node.js 22.14 或更新版本，
V2 CI 使用 Node 22。`V2_CARGO_TARGET_DIR` 支持任务私有缓存，同时保留真实 release 构建流程。

`tests/packaging/test-skillsec-install.sh` 使用真实构建产物，在临时 DESTDIR 检查配置保留、
不自动激活或创建密钥，以及 V1 unit 隔离。安装后的 Python V2 E2E 使用独立状态和审计目录，
并以普通 UID 验证 root daemon 的 PAP 权限拒绝。这些检查与实际 systemd 生命周期和真实
FUSE 证据分别记录。
[核心指南](../../../../docs/user-guide/zh/agent-security/agent-sec-core/skillsec-v2.md)
提供源码/RPM 命令、共享卷要求及恢复匹配状态的升级回退步骤。

历史交付验收（早于本次对齐）在 Alibaba Cloud Linux 4 x86_64 上通过。与主线系统 daemon 对齐后，源码
安装套件通过 914 项，跳过 38 项（含构建容器没有 system manager 的用例）；RPM 安装套件
通过 914 项、跳过 37 项，再单独通过修正后的 systemd 生命周期用例，共 915 项独立用例。
两套均排除两项真实模型测试。其余 skip 涉及仅源码环境提供的规则库存/元数据及 telemetry；
SkillSec、PAP 和 daemon 生命周期用例均已执行。
这些测试无法调用 Python Ledger。RPM 来自仓库原配方，DNF 在正常依赖检查下安装核心及
Skill 资源包；这是仓库构建产物的证据，不代表 GitHub CI 结果。

原样的产品 unit 在真实 PID 1 systemd 255 下完成 45 项操作。有效及上界 capabilities 精确为
CHOWN、DAC_OVERRIDE、FOWNER，并启用 `NoNewPrivileges`。UID 1001 可管理私有 HOME、
`/tmp`、系统及共享卷 Skill；回滚后可继续编辑，换钥仅限 root，重启保留信任，公共审计不含
敏感细节。包生命周期的 12 项检查通过：重装保留配置，卸载保存修改过的配置并保留私钥，
恢复安装后信任和包校验保持有效。这验证 V2 包恢复，不代表 V1 降级或 Agent Hook 接入。
system manager fixture 还验证产品的 75 秒强制停止期限及启动限流。由于 `/run` 可能为
noexec，用例原位执行选定产物，仅在隔离测试 unit 注入非终止的停止信号，并验证真实的启动
拒绝行为，避免依赖发行版相关的 `Result` 字符串。此前失败的 fixture 尝试单独保留。

## Hook 后续交付

后续交付保留 `skill-ledger` 命令和能力 ID、宿主语言、匹配规则、默认策略及启用状态，
调整 daemon 初始化与命令结果校验，并将 Cosh 宿主超时设为 10 秒，覆盖顺序执行的
3 秒初始化和 5 秒查询；不引入统一 Hook 框架或第二套策略层。
六类适配器的行为和执行错误边界见
[用户指南](../../../../docs/user-guide/zh/agent-security/agent-sec-core/skillsec-v2.md#agent-hook-接入)。

按四个逻辑提交交付：初始化、结果处理、真实 Rust CLI／daemon 合同、文档与验收记录。
真实宿主验收单独推进。
每批代码附带测试。合同用例位于 `tests/v2/e2e/test_skillsec_hook_contracts.py`；记录器通过
exec 调用真实 Rust CLI，不替换输出。源码资产与安装资产分别验证，安装模式缺失插件即失败，
并接入 `make test-e2e-rpm-v2`。以源码暂存资产执行安装模式，只证明安装布局，不证明 RPM
已经构建或安装。

本轮验证状态单独记录如下，不把先前核心／RPM 结果当作新增适配器的证据。宿主确认、工具
阻断及模型行为需要真实宿主单独验收。`SKILL_LEDGER_HOOK_ENABLED=false` 是临时运营绕过；
恢复旧插件会恢复旧行为，但不能把 V1 信任导入 V2。

### 当前 Hook 基线（2026-09-30）

四个 Hook 提交已对齐到合入核心 PR #3295 后的主干 `5e811c459`。产品及测试修订
`6227e1772` 在 Alinux 4 x86_64、Python 3.11.6、Node.js 22.23.0、Rust 1.93.1 下
重新完成基本验收，Rust CLI 和 daemon 使用新的 target 目录构建。

| 检查 | 结果 |
| --- | --- |
| Python Hook 适配器，含 Hermes | 373 项通过 |
| 能力环境变量视图 | 178 项通过，1 项既有 native 扩展检查跳过 |
| OpenClaw 构建与完整单元测试 | 构建通过，204 项测试通过 |
| 启动、重启及 Skill 状态子集 | 48 项通过 |
| 真实 Rust CLI／daemon Hook 合同 | 源码布局 112 项、安装布局 112 项通过 |
| 安装后的 Cosh 直接调用 | 15 项通过 |
| 公共 V2 E2E | 420 项通过 |
| raw 打包 | 通过 |

主干现在会在启动时扫描获授权的普通 Skill。合同夹具等待真实启动激活的审计事件后，
才创建待测 Skill；重启时等待两个 Skill 完成处理。未初始化密钥场景使用独立的空授权
范围 daemon，审计断言按 Hook 子进程 PID 排除后台调用。此次仅调整测试准备流程，
保留产品启动行为。安装资产由源码构建，这些结果不代表 RPM 安装或真实 Agent／模型
验收。后续文档修改不改变已验证的代码。

```text
CLI SHA-256:    10f983c4ae63130105510d23a33d887aaf3c8aa0ed5bf756553fca98f7ef1e86
Daemon SHA-256: f623b214408ff333af87ab17fef826b2adbd5851443a855a5c15232f295747cc
```

以下记录作为历史证据保留，不作为当前基线通过的证明。

### 历史 Hook 验证记录

四个 Hook 提交已对齐核心基线 `0d252302`，保留离线构建参数、RPM 依赖和 daemon
启动／退出测试。代码修订 `687d2e5d8` 在 Alinux 4 x86_64、Python 3.11.6、Node.js 22.23.0、
Rust 1.93.1 下重新完成基本验收：

| 检查 | 结果 |
| --- | --- |
| Python Hook 适配器，含 Hermes | 373 项通过 |
| 能力环境变量视图 | 178 项通过，1 项既有 native 扩展检查跳过 |
| OpenClaw 构建与完整单元测试 | 构建通过，204 项测试通过 |
| 安装后的 Cosh 直接调用 | 15 项通过 |
| 真实 Rust CLI／daemon Hook 合同 | 源码布局 112 项、安装布局 112 项通过 |
| 公共 V2 E2E | 420 项通过 |
| raw 打包 | 通过 |

CLI 和 daemon 从空 target 目录重新编译。安装布局使用源码构建资产，不代表 RPM 验收。
受管目录回归改为验证范围拒绝、原有 Hook 错误策略，以及不写元数据或登记记录。
Cosh 用例读取安装后的 manifest，模拟 2 秒初始化和 3.4 秒查询；独立变异验证确认
5 秒配置会失败，10 秒配置返回预期 `ask`。旧范围用例的四项失败及容器缺少 `cmp` 的
首轮结果均保留；修正测试合同并补齐该测试依赖后，上述检查全部通过。
用户要求的真实 Agent／模型验收继续暂缓。后续仅补充验收文档，不改变已验证的产品及测试文件。

```text
CLI SHA-256:    a020cd6eeb85dce879b67256c1043aa3f8af27ec905097ec17ddc0671fba26e4
Daemon SHA-256: e8baaa547d58c63764e8270785d60e62b8499fc30455302ccff0864f2fca6def
```

以下 `72d50d82` 结果为历史证据，不作为新基线通过的证明。

在 Alinux 4 x86_64、Python 3.11.6、Node.js 22.23.0 的独立容器中，373 项针对性 Python
测试、175 项 OpenClaw 单元测试及 OpenClaw TypeScript 构建通过。112 项真实后端 Hook
合同分别在源码资产、源码构建后安装到包路径的资产上通过。两轮使用相同的全新核心构建
`72d50d82`：

```text
CLI SHA-256:    1b8f56588478e99a8537278171b53df56ea236f660fb5aff23f9804fb5cda0f0
Daemon SHA-256: bef6f9e3694ce9c35c9cf28d65fe392aaa0c4515410e0c0ef99a3f55c3b9b3d6
```

在 Linux 的 `src/agent-sec-core` 下执行；需要独立的 root daemon 环境、`PATH` 中的真实
二进制及已编译插件资产：

```sh
SKILLSEC_HOOK_LAYOUT=source python3 -m pytest -q tests/v2/e2e/test_skillsec_hook_contracts.py
SKILLSEC_HOOK_LAYOUT=installed python3 -m pytest -q tests/v2/e2e/test_skillsec_hook_contracts.py
```

公共 `tests/v2/e2e` 回归 415 项全部通过。首轮两项失败来自夹具配置：未启动外部 daemon，
以及跨 UID 用例继承了私有构建 `TMPDIR`。改用 CI 方式启动独立 daemon，并使用容器 `/tmp`
后通过，未修改产品代码。文档命名、双语目录一致性及相对链接检查通过。

旧 Qwen 直接 Hook E2E 文件使用当前 Python V1 源码，6 项全部通过。审计断言同时覆盖
幂等初始化和后续暴露状态查询；这属于向后兼容证据，不代表原生 Qwen 验收。

真实宿主验收**尚未完成**。Qwen 启动时的自动更新在业务请求前将测试机共享安装从 0.19.9
改为 0.24.6。按用户要求，真实宿主验收与共享安装恢复暂缓，本轮仅做基本验收。此事记为测试控制违规，不作为 SkillSec
产品结论。已准备的离线恢复方案可恢复官方 0.19.9 及 Linux 可选依赖；由于更新前没有完整
目录哈希，不能证明逐字节恢复。重启宿主进行干净验收前，需要禁用自动更新并保护共享安装路径。
独立容器合同测试与这轮中断的真实宿主试验分别记录。
