# SkillGuard 第一阶段迁移

[English](SKILL_GUARD_PHASE_ONE.md)

SkillGuard 在一个 `asc-capability-skill-guard` crate 内拆分 Skill 扫描、内容完整性、
版本存储和激活，由系统 daemon 中的 `SkillGuardService` 编排。本文记录迁移合同和开发批次；
标记为计划的内容不代表能力已实现。

## 交付与验收

核心迁移和 Agent Hook 接入分为两个 PR。当前 PR 包含 Rust 核心、公共 Action Runtime 审计、
daemon、CLI、SkillFS 和 Linux 部署。Agent Hook 实现、能力视图、Hook 默认配置与真实 Agent
验收属于下一个 PR。消费者请求和响应样例仅证明接口合同，不证明 Agent 已接入。
第二阶段统一策略体系不在当前 PR 范围内。

| 批次 | 职责 | 实现状态 | 进入下一批前的验收 |
| --- | --- | --- | --- |
| 1 | 类型、canonical 身份、系统密钥、Integrity | 已实现，Linux 门禁通过 | 签名、篡改与重放拒绝，密钥权限，源目录与快照路径规则 |
| 2 | Scanner 与 analyze | 计划 | V1 结果对照，选择与别名，覆盖不足，错误，不写账本 |
| 3 | Ledger 与 Service | 计划 | 版本、补扫与强制扫描、快照、导出、串行化、扫描期间内容变化 |
| 4 | Activation | 计划 | 决策、active/pending/hidden、回滚、发布失败与启动 reconcile |
| 5 | daemon、CLI 与审计 | 计划 | 真实 CLI 请求、输出和退出码、peer 身份、审计、超时、管理员换钥、消费者样例 |
| 6 | SkillFS | 计划 | 单 socket、HMAC notify/resolver、拒绝降级、真实 FUSE 效果、普通 IPC 回归 |
| 7 | 部署 | 计划 | 源码与 RPM 安装、root systemd、普通本地用户调用、核心完整流程 |

每批形成一个独立编译的逻辑 commit，同时包含测试和文档。本批引入的问题修回本批 commit。
必须在 Linux 上验收；macOS 格式化或 manifest 检查不能替代构建与运行验收。

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
`v2/crates/action/capabilities/asc-capability-skill-guard/tests/integrity.rs`。
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

## 回退边界

第一批尚未注册 daemon 方法，不改变安装或线上状态；回退 crate 和 workspace 注册即可移除。
完整迁移替换部署时应单独保留 V1 状态，V1 无法读取 V2 manifest 或系统密钥。
回退部署必须同时恢复匹配的状态与配置，不能让两个实现同时写一个账本。
