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
| 2 | Scanner 与 analyze | 已实现，Linux 验收通过 | V1 结果对照，选择与别名，覆盖不足，错误，不写账本 |
| 3 | Ledger 与 Service | 已实现，Linux 验收通过 | 版本、补扫与强制扫描、快照、导出、串行化、扫描期间内容变化 |
| 4 | Activation | 已实现，Linux 验收通过 | 决策、active/pending/hidden、回滚、发布失败与启动 reconcile |
| 5 | daemon、CLI 与审计 | Linux 验收通过 | 真实 CLI 请求、输出和退出码、peer 身份、审计、超时、管理员换钥、消费者样例 |
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

## 第二批 Scanner 边界

`ScannerRegistry` 依次在进程内执行 `code-scanner` 和 `static-scanner`。Code Scan 复用 V2
能力，扫描 Python、Shell 和带受支持 shebang 的无扩展名文件。Static Scan 内嵌原有十条规则，
并检查元数据、链接、隐藏文件及疑似凭证文件、二进制资源和未声明联网行为。符号链接会产生
finding，但不读取目标内容。与 V1 相同，自定义扫描器仍通过外部 findings 导入；`cli`/`api`
注册信息不会让 root daemon 执行调用者提供的命令。findings-array 保留额外证据字段，并返回
可见的规范化警告。新输入拒绝已废弃的扫描器名称。

`analyze` 不依赖密钥或受管目录注册，不写账本。完整扫描的 `pass`/`warn`/`deny` 均返回退出码
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

`SkillGuardService` 按 canonical Skill 身份加锁，直接路径和已验证的映射路径共用同一把锁，
闲置锁条目会释放。操作持有密钥代际读锁，第五批换钥时使用写锁。受管目录精确登记在 daemon
私有状态中，不会把用户指定目录的同级 Skill 自动纳入管理。
业务根目录不得位于 `.skill-meta` 内；内部快照校验不经过该业务根入口。

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
启动恢复仅枚举已登记根目录。修复所报告的失败原因后，对未变化内容重试 `scan`，会复用可信版本并
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

`action.skill_guard` 接受封闭的 `command` 枚举，是此前候选 `action.skill_ledger` 的明确 V2
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

root 所有的 `/etc/agent-sec/skillguard.json` 或 `--skillguard-config` 指定 `stateDir`、精确
`managedSkillDirs`、scanner 覆盖配置与 parsers。默认状态目录 `/var/lib/agent-sec/skillguard`
要求 root 所有、0700，当前密钥保持 0600。不导入用户配置、旧历史和 keyring。init baseline 和
check/scan --all 由 CLI 发现当前用户默认 Skill 位置及两处系统目录中的精确根目录，再由 daemon
与已登记目录合并；单个显式路径不隐式登记相邻 Skill。status 查看系统登记集合，不随调用者 HOME 改变。
CLI 的共享发现逻辑包括 `$XDG_DATA_HOME/anolisa/skills` 下的直接 Skill 子目录，遵循安装器的
路径规则：未设置、为空、相对路径或原始路径含 `.`/`..` 段时，回退到
`$HOME/.local/share/anolisa/skills`；合法但不存在的目录直接跳过。`HOME` 未设置或为空时，
使用 CLI 当前用户的系统账户主目录。只有 CLI 读取调用者环境，
`init --no-baseline` 和显式路径请求不触发发现，隐藏目录及快照过滤保持不变。

换钥持有全局代际写锁，写入私有 intent，撤销全部登记 Skill 的 activation 后才替换密钥。
有未完成回滚时须先 reconcile。撤销失败保留旧密钥，并阻止普通账本操作，直到管理员重试或启动
恢复成功。恢复发现指纹已经变化时，不会再次换钥。启动恢复经过公共 Action Runtime；单个 Skill
恢复失败可见，但不关闭其他 daemon 方法。

公共 Finalizer/Sink 仅记录受控的 command、数量、判定状态、版本和执行错误类别；不包含原始
findings、源码、导入证据、人工理由、路径和密钥字节。完整业务结果仍返回客户端。最多同时执行
两个 SkillGuard 请求以限制内容捕获内存，满载返回明确 Busy。默认预算 60 秒，timeoutMs 或
CLI --timeout-ms 在服务端最多 120 秒；其他方法保留原有预算。不自动重试请求。响应超过 3 MiB
返回 `ResponseTooLarge` 和 `operationMayHaveCommitted=true`，不静默截断数据、不撤销已提交
操作；findings 导入上限为 2 MiB。

`v2/fixtures/skillguard/consumer.json` 提供正常、风险、未初始化、超时、执行错误及激活未完成
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
