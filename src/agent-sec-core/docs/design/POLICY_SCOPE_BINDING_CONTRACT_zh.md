# Policy Template、Scope 与 Binding 生命周期契约

文档类型：`[TARGET V2][IMPLEMENTED]` 对象与生命周期契约；第 7 节为 `[IMPLEMENTED]` 通用规则结构与首阶段执行边界。
本契约的对象职责、revision 与快照生命周期与
[AgentSecCore 安全策略语言](https://alidocs.dingtalk.com/i/nodes/20eMKjyp810mMdK4HrdLaNnXJxAZB1Gv)
一致；策略内容已使用通用 rules 结构，执行能力仍限定为文件删除 block 子集，未实现完整语言语义。
本文替代旧设计中 Scope 仅表示 PID/cgroup、Scope 自身增版、用户手动
Binding CRUD 和 Scope 引用阻止 Policy 更新/删除的目标语义；不改写已有测试的历史结果。

当前持久化实现见 [Policy SQLite 持久化与崩溃恢复设计](POLICY_SQLITE_PERSISTENCE_DESIGN_zh.md)：
包含数据库事务、内部 `status_version` CAS、Discovery 恢复及本地 crash 验收证据。

## 1. 对象与版本

**PolicyTemplate 是可被多个 Scope 引用和复用的策略。** 它具有独立身份、内容 revision
和完整规则内容；Template 表示策略可以复用于不同主体分配。本文的 Policy 与 Policy Template
指同一个受管理的策略对象，wire 中的 `template` 字段保存该策略的规则内容。
“禁止删除文件”是当前支持的规则场景，不是 PolicyTemplate 的概念定义。
同一个 PolicyTemplate revision 可以被 Scope S1、S2 分别选用，两者各自保存完整快照、
管理自己的 Binding；删除一个 Scope 不影响另一个 Scope，也不删除来源策略。

| 对象 | 身份与版本 | 保存内容 | 用户操作 |
|---|---|---|---|
| Policy Template（可复用策略） | 稳定 Policy ID + 单调递增 revision | 每个 ID 只保留一个 current 完整记录 | create、update、get、list、delete |
| Scope（Assignment） | 独立 Scope ID；没有 Scope revision | 不可变 selector + 指定 Policy revision 的完整快照 | create、get、list、delete、retry |
| Binding | 系统生成 Binding ID，归属于唯一 Scope | 一个策略快照、来源身份、具体执行实例和部署状态 | 只读查询；不提供手动 create/update/delete |

策略语言中的 `spec_version` 是格式版本，当前 wire 在策略内容中使用 `specVersion`。Policy `revision` 是内容版本。Policy 内容变化沿用 ID 并
分配下一个 revision；相同内容的幂等写入不增版。某个 `(Policy ID, revision)` 对应
的内容不可改写，但不要求模板库永久保存该版本。保留既有 revision 分配头，避免删除
内容后复用版本号；不新增历史版本库、引用计数或历史 revision 的 CRUD。

Scope 创建后不可修改。更换 selector、策略集合或任何所选 Policy revision，均创建
新 Scope ID；Scope 的运行/删除状态变化不属于配置修改，不需要 Scope revision。
实现保留内部 `bindingRevision` 及底层 changed-spec 回归覆盖；系统新建 Binding 从 1 开始，不能
将它解释成 Scope revision、Policy revision 或每次重试都变化的状态计数器。

## 2. 精确引用与完整快照

创建输入按 Policy ID/revision 选择模板；服务端读取并验证精确 current 版本，再把完整
内容保存在 Scope 中。输入引用和保存的快照是不同层次；PAP 负责模板校验，Adapter 负责目标编译。

```text
ScopeCreate
  selector
  policyTemplates[]: { policyId, policyRevision }

StoredScope
  scopeId
  selector
  policySnapshots[]: complete PreparedPolicy
```

以上字段与当前 wire 一致；创建输入用 `policyTemplates`，保存结果用 `policySnapshots`。
`PreparedPolicy` 只包含 `policyId`、`policyName`、`revision` 和 `template`，Scope 和 Binding
保存同一个完整模板快照。
每次 Apply 按 `Binding.policy.template → AgentSight Adapter → ActPlane DSL → Client`
执行。Adapter 只读取 Binding 快照。
当前策略内容的格式版本、规则、资源及条件目录校验在 `PolicyTemplate::validate()`；
AgentSight 的 glob 子集、DSL 字面量、规则数和长度限制由 Adapter 拒绝检查。
Scope 没有 `revision`，消费它的请求/响应和 Binding 来源信息也不携带 `scopeRevision`。
内部模型见 [PreparedPolicy](../../v2/crates/asc-policy-types/src/policy.rs)。

引用解析与 Scope 保存必须具有一致的准入边界：并发 Policy 更新/删除时，创建要么保存
请求的完整版本，要么明确失败，不能悄悄换成最新版本、混合版本或保存悬空引用。
Scope 保存成功后，模板更新或删除不影响其快照，也不影响随后匹配到的新进程。
从已有 Scope 生成 Binding 时读取 Scope 快照，不再要求来源 Policy 仍是 current 或存在。

模板库只保存 current：新的 Scope 创建请求不能通过 ID/revision 获取已被替换的旧版本。
既有 Scope 的快照继续可用，但不是对外提供历史模板查找的版本库；本次不新增从其他
Scope 导入旧版本的接口。删除 Policy 是删除模板库记录，不是撤销已分配的策略。

```text
P@1 -> create S1 with a complete P@1 snapshot
update P -> current record becomes P@2; S1 remains unchanged
create S2 from P@2 -> wait for required Bindings to become effective
delete S1 -> stop admission and reconcile removal of its Bindings
delete P -> S2 and newly matched instances still use its P@2 snapshot
```

## 3. Scope 拥有 Binding 生命周期

Scope 同时选择主体并分配策略。系统为每个 `(Scope ID, Policy ID/revision, execution
instance)` 生成一条 Binding，重复扫描不能重复部署。不同 Scope 独立拥有自己的 Binding。
Binding 的 Apply/清理意图来自 Scope 生命周期及实例发现，不来自用户 Binding mutation。
公开 Binding 查询应显示 Scope ID、Policy ID/revision、实例身份、部署状态及安全失败原因。

每个 Binding 只需要自身对应的策略内容和必要来源信息。不能把包含全部策略快照的
完整 Scope 再复制到每条 Binding，造成每个策略重复携带同 Scope 的所有其他策略。
具体存储布局应保持执行输入自洽，并在 Scope 清理期间保留完成部署清理所需的内容。

进程名称/path 是选择条件，具体执行实例是另一个字段。PID 实例至少区分 boot、PID
namespace、PID、start time；发现、入队、重试和实际下发必须保持同一实例，不能在
重试时重新解释为复用同一 PID 的新进程。进程退出或不再匹配由系统发起清理；局部
扫描失败不能当作退出或清理完成。PID selector 固定首次发现的实例，不跟随 PID reuse；当前拒绝 cgroup assignment。

### 3.1 Scope 删除与清理顺序

以下流程已接入 SQLite PAP、discovery 与 Reconciler。

1. **接受删除意图**：原子保存 Scope 正在删除的状态并关闭新 Binding 准入，保留 Scope
   配置及策略快照。并发或延迟到达的发现结果不得在此之后创建子 Binding。删除意图不可撤销。
2. **停止发现**：取消并 join 该 Scope 的 discovery worker；停止扫描不等于远端策略已移除。
3. **请求子 Binding 清理**：系统处理全部所属 Binding，包括 pending、已生效、失败及部分
   部署状态。Apply 正在执行时先接受 Delete 意图，等待旧调用退出并记录其副作用，再进行
   同 Binding 的清理；旧 Apply 结果不能将删除意图改回 READY。
4. **执行远端删除**：Reconciler 清理所有已知或可能存在的目标，包括 UNKNOWN。超时或
   不确定结果不能作为不存在的证明；按有界预算重试。失败保留可查询错误和全部未完成
   清理责任，不因回收缓存、线程或本地记录而报告成功。
5. **完成回收**：确认所属目标全部不存在且没有未完成的本地目标调用后，条件移除已清理
   的 Binding。最后一条 Binding 删除成功时，同时移除已停止 discovery 的 Scope 及其快照；
   若没有 Binding，则停止 discovery 后直接移除 Scope。删除不影响来源 Policy Template，
   也不影响其他 Scope 及其 Binding。Active Scope 不因实例退出、Binding 暂时清空而删除。

删除返回 `ScopeDeletion {scopeId, completed}`。`completed:false` 表示意图已受理但清理
尚未完成，此时 Scope 为 `DELETING`，相关失败仍可查询；`completed:true` 表示已回收。
重复删除合并到已有意图，不重复创建清理任务，不重置重试预算。完成后 get 返回 not_found，
list 不再包含该 Scope。删除不存在的 Scope 返回 `completed:true`，重启后也可重复删除。

有界自动重试继续适用，周期扫描不能重置终态失败预算。`policy.scopes.retry` / `scope retry`
仅将所属 `APPLY_FAILED`/`DELETE_FAILED` 转回对应 pending，保留目标责任，不重启非终态工作。
正在删除的 Scope 重试仍先停止发现，再请求清理；不能回到 ACTIVE，也不能改 assignment。

## 4. 与已有 Reconciler 的边界

```text
Policy current record -> Scope-owned policy snapshots
Scope + instance discovery -> stored Binding intents -> Binding ID notification
BindingReconciler -> Adapter/Client -> deployment observations and status
```

Scope 层拥有分配和子 Binding 生命周期；既有 Reconciler 继续拥有每个 Binding 的目标
执行、重试、状态和清理记账。队列只携带 ID，执行重读 repository；同 Binding 串行，
条件写防止旧任务覆盖新意图；修改性 I/O 前登记目标，确认全部目标不存在后才回收责任。
Scope 删除可以在 Apply 期间被接受，但旧调用退出及结果记账之前不能启动同 Binding
的下一次目标操作。无须为 Scope 重写一套 PEP 执行状态机。

Scope 替换采用不同 Scope/Binding 身份，不能伪装为修改旧 Scope 或手动 Binding UPDATE。
先创建新 Scope、确认所需 Binding 生效、再删除旧 Scope 是替换顺序；并存期间的目标语义使用
策略语言的统一效果组合规则；当前仅执行文件删除 block 子集，未实现通用效果组合。
该顺序不承诺全局原子切换、无保护间隙或自动回滚。
后端是否允许新旧部署并存须验证；当前 AgentSight Client 的旧 Binding update 路径
是先删后建，不能作为“失败时旧策略必然保留”的证据。

## 5. 实现与兼容边界

| 入口 | 当前实现 |
|---|---|
| Policy | 一个 current 完整记录，保留 revision 分配头；无历史库/引用计数 |
| Scope | 无 revision，保存完整快照；事务内与 Policy mutation 原子校验 |
| discovery | `start(seed)`，恢复 Assignment、PID pin 和已知实例；每 2 秒扫描；缓存命中也重试失败准入 |
| Binding | 系统去重创建，查询可见；单策略快照 + Scope ID/selector + 固定实例身份 |
| 删除 | 关闭准入、join worker、复用 Reconciler 清理、失败可查、完成后回收 |
| 协议 | 12 个 PAP 方法：Policy 5，Scope 5（含 retry），Binding 2（get/list） |
| 大小 | 编码后 Policy/Scope 最多 1 MiB，列表条目预算 3 MiB；offset 按实际返回条数推进 |
| AgentSight | plan/apply 格式升级到 v2，跨次尝试及下发前校验固定进程身份 |
| 存储 | `policy-state.db`；WAL/FULL、三表事务、内部 status CAS 和写回执；启动恢复 discovery 与未完成 reconcile |

Scope update、手动 Binding create/update/delete 返回 `unknown_method`；旧 Scope revision
参数拒绝。CLI/daemon 必须成套升级；不导入旧进程内 PAP 记录或 V1 数据。
SQLite 后端复用 workspace 已有依赖。discovery 容量仍为 32 个 Active Scope；未配置发现或
Reconciler 不可用时新 Scope 返回 unavailable。Policy 写入、查询及 Scope 删除仍可执行。

discovery 启动失败时，PAP 尝试按顺序补偿删除，但始终返回原始启动错误。补偿失败日志
包含 Scope ID、启动错误和补偿错误；遗留 Scope 保持可查询，依赖恢复后显式删除。
删除时先记录 DELETING 再停止 worker，stop 失败不撤销删除意图；依赖恢复后重试推进。
已 shutdown 的 registry 不会因重复请求自行恢复。

SQLite 使用实例/策略身份的唯一约束，准入事务在已保存集合中去重；32 个策略上限不限制
进程或 Binding 总数。内存测试后端保留相同语义。

源码入口：[Scope](../../v2/crates/asc-policy-types/src/scope.rs)、
[PAP](../../v2/crates/asc-pap/src/service.rs)、
[SQLite repository](../../v2/crates/asc-policy-repository-sqlite/src/lib.rs)、
[discovery](../../v2/crates/asc-daemon-core/src/scope_discovery.rs)、
[Client](../../v2/crates/asc-agentsight-client/src/client/reconciliation.rs)。

## 6. 可执行验收

- PAP：并发 Policy 更新/删除与 Scope 创建只能保存精确版本或失败；模板删除后未来实例
  使用旧快照；重复发现去重，多 Scope 独立；关闭准入、旧 Apply CAS 和显式失败重试。
- discovery/Client：名称/path、改名/exec、局部扫描失败；PID reuse 不重定向已有 Binding，
  PID selector 不跟随新实例；固定 boot/namespace/start time 在 prepare 和请求前核对。
- Runtime/PCP：保留串行、条件写、部分失败、UNKNOWN 目标和预算回归；Scope 删除在 Apply
  期间受理，旧调用退出后完成所有目标清理，不丢失责任；最后一条 Binding 删除时同时
  移除已停止 discovery 的 Deleting Scope，Active Scope 变空仍保留。
- 组合：`procfs_discovery_uses_saved_policy_and_scope_delete_cleans_all_instances` 使用真实
  procfs、PAP、runtime、Adapter 和 scripted Client，覆盖模板更新/删除后新增进程及最终回收。
- 协议/CLI：完整 method/错误/scenario fixtures、UDS、bootstrap、CLI process 和 Python
  `test_policy_assignment_snapshot_and_cleanup`；包括重复已完成删除、旧接口拒绝。
- 完整下发链路：[HTTP mock E2E](../../tests/v2/e2e/test_policy_delivery_e2e.py) 经真实
  CLI/daemon、SQLite、discovery、Adapter 和 Client，验证模板更新后新发现进程仍使用 Scope
  的旧快照，完整 HTTP 请求匹配既有 fixture；Ready 落库后删除 Scope，验证远端 DELETE
  与本地 Binding/Scope 回收。mock 只替代 AgentSight 服务端，不验证实际内核执行。
- 大小：超限单记录保存前拒绝；列表遵守字节预算且分页可继续。

运行门禁：在 `v2/` 执行 `cargo fmt --all -- --check`、
`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、
`cargo doc --workspace --no-deps`；Python E2E 使用本次构建的 CLI/daemon。
持久化的 SIGKILL/故障注入证据见 SQLite 设计第 9 节；这些本地检查不代表远端 CI、
RPM/systemd 部署、物理断电恢复或真实 kernel enforcement。
旧验收记录保留其历史基线，不能把旧 PASS 改写为本次或持久化交付结果。

先前 process-local 阶段的本地记录：Rust workspace 共 802 项测试通过（0 failed/ignored），Python CLI/daemon
流程 2 项通过；fmt、Clippy（deny warnings）、rustdoc、Python 格式/lint、文档命名与链接
检查通过。Python 使用 3.11.6 和本次本地构建的二进制，socket 测试在允许本地 UDS 的环境
执行。未运行远端 CI 或真实 AgentSight/kernel 验收。

## 7. 通用规则结构改造计划

本节为 `[IMPLEMENTED]` 首阶段结构和执行边界。策略语言负责语义，本节固定本地实现的
字段映射、首阶段支持范围、兼容边界及验收；不复制网络和 AgentHook 的完整属性目录。

### 7.1 已确认的决策

| 问题 | 决策 |
|---|---|
| 第一阶段执行范围 | 改为通用 `rules` 结构；执行能力仍限定为禁止删除文件 |
| 合法但目标不支持的规则 | 允许创建和保存模板；Binding 转换失败并报告原因，不部分下发 |
| 文件 target | 带类型对象：`{"type":"file","path":"/workspace/**"}`；每条规则一个目标，多路径展开成多条规则 |
| 旧开发期数据库 | 不提供旧模板 JSON 迁移，仅支持新建数据库；旧库显式拒绝，不自动删除或重建 |

### 7.2 数据结构与职责

保留 `PreparedPolicy {policyId, policyName, revision, template}`、现有 PAP 方法与 CLI 命令。
身份、名称和内容 revision 由外层对象管理，不在规则内容中重复维护。
下面的 `PolicyTemplate` 是现有 Rust 类型名，对应 wire 的 `template` 内容字段；
该类型使用通用规则结构，承载可复用策略的内容：

```text
PolicyTemplate
  specVersion: "0.1"
  description?: string
  rules: non-empty array<Rule>

Rule
  effect: block | allow | require_confirmation
  category: file | network | agenthook
  action: read | write | exec (currently registered for file)
  target: typed target compatible with category + action
  where?: Condition
  previous?: HistoryCondition
  because?: string

FileResource
  type: "file"
  path: normalized absolute path or supported authoring glob
```

wire 沿用当前 camelCase；与语言文档的 snake_case 字段做显式映射，例如 `spec_version`
对应 `specVersion`，条件中的属性名也遵守相同映射，不能混用两种拼写。
规则不增加 ID；保留输入顺序，模板相等比较仍使用完整结构，规则重排视为内容变化。
`description` 为说明，`because` 为规则静态理由，不参与条件求值。
文档中的 `warn` 效果尚未定义组合语义，首阶段不接受；不能作为 block 的降级结果。

Condition 使用语言文档中的单属性比较或 `and`／`or`／`not` 组合；HistoryCondition
使用 Event 或同样的逻辑组合。Event 不包含 effect、because 或嵌套 previous。
省略 where／previous 表示无对应限制。属性类型、可用运算和适用动作必须已定义；
未知属性、类型不符、空组合和非法 category/action/target 组合在模板创建/更新时拒绝。
rules 限制为 1–1024 条；条件深度最多 16，整个策略合计最多 4096 个条件/历史节点。
说明与理由各最多 4096 bytes，拒绝 NUL；这些上限与既有单记录和 wire 限制共同约束输入。

文件目标先固定上述单路径对象。NetworkResource、HookList、rename 双路径目标及尚未定稿的
AgentHook 属性不能仅靠任意 JSON 接受；先完成各自的类型与语义定义，再开放保存。
允许保存“目标后端不支持”的合法规则，不等于允许保存语义未定义的规则。
第一阶段可用 `file.read`、`file.exec`、其他已定义的 file.write 条件，以及 allow、
require_confirmation 和合法历史 Event 验证该边界；保存不代表这些规则可执行。

Scope 继续回答“谁受哪些策略约束”，选择主体并保存精确 revision 的完整快照。
PolicyTemplate 回答“什么条件、动作、资源、效果及理由”。Binding 保存其中一个模板快照和
固定实例身份；执行直接走 `Binding.policy.template → Adapter → DSL → Client`。
PAP 进行目标无关的结构与语义校验；Adapter 检查目标能否完整实现全部规则，不重新读模板库。

### 7.3 首阶段可执行例子

以下是当前 CLI 可接受的策略内容文件：

```json
{
  "specVersion": "0.1",
  "description": "Protect important files from deletion",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/workspace/important/**"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect important files from deletion"
    },
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/etc/agent/config.yaml"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect agent configuration from deletion"
    }
  ]
}
```

AgentSight Adapter 首阶段只支持上述单条 operation=delete 条件、block、file/write 和文件目标
的组合，不带 previous；每条规则生成 `block unlink file`，because 使用该规则的理由，
省略时沿用现有固定理由。路径与理由必须通过 DSL 字面量检查，拒绝引号、反斜杠和控制字符，允许普通 Unicode；
当前 ActPlane lexer 无转义语法，不使用 JSON 转义伪装成可表达的 DSL。
不带 where 的 file.write 表达更广泛的写操作，不能转成只阻止删除的规则。
复杂逻辑条件即使可推导为相同限制，也不在首阶段进行化简。

保留现有路径规范化及 AgentSight glob、长度、规则数限制。规则按输入顺序产生稳定名称；
相同路径但不同条件或理由的规则不能按路径合并。多条 block 的匹配集合为并集。
任何一条规则不支持，整个 Binding 转换失败，以 `RULE_<index>_<reason>` 有界机器码报告从 0 开始的规则位置及原因；确认全部可转换后才交给
Client，不发送部分规则，也不先登记一个不会执行的远端 Apply。失败不影响其他 Binding。
Scope 创建成功仅表示分配已保存，后续 Binding 可因目标不支持进入失败状态；重试不能把
不支持的规则变为支持，需要升级后端，或创建采用受支持模板 revision 的新 Scope。

第一阶段不实现通用效果组合、历史事实采集与恢复、人工确认流程、网络或 AgentHook 执行。
现有 ActPlane 将 unlink 与 write 映射到同一底层操作的限制仍需单独处理；生成正确 DSL
及 mock 请求不构成“真实内核仅阻止删除”的证明。

### 7.4 实现与验收范围

1. **类型及校验**：替换固定场景 enum；实现通用规则、文件目标、条件和历史 Event 的类型及
   目录校验。对未定义资源明确拒绝，测试合法但当前不可执行规则能够保存。
2. **API 与 Adapter**：同步模板文件、响应和错误 fixtures；实现首阶段全部规则的转换检查、
   稳定 DSL、逐规则理由及不支持诊断。现有命令和 PAP 对象生命周期保持原职责。
3. **持久化格式**：按 [SQLite 设计](POLICY_SQLITE_PERSISTENCE_DESIGN_zh.md#34-通用规则改造的首版格式边界)
   初始化新格式库并拒绝旧库；在新库中覆盖 Policy、Scope、Binding 完整快照的保存和重启解码。
4. **回归**：多路径转换为多规则；混合支持/不支持规则不得产生 HTTP Apply；模板更新或删除后
   Scope 及随后新增实例仍使用保存的旧 revision；重启后保持同样的 DSL 语义与实例身份。
   because 包含引号、反斜杠、控制字符和非 ASCII 时检查安全处理及明确失败行为。
5. **删除与恢复**：在新格式库重复验证 Apply 中删除 Scope、UNKNOWN 记账、SIGKILL 恢复和
   最后一条 Binding/Scope 回收；旧库拒绝启动前后，原 DB/WAL 中的业务内容不能被清空。

实现复用现有类型、Adapter、Repository 和 Runtime，没有新增 crate 或独立规则执行框架。
领域校验回归见 [通用规则测试](../../v2/crates/asc-policy-types/tests/authoring.rs)，
逐规则转换、拒绝及 DSL 注入边界见 [Adapter 测试](../../v2/crates/asc-policy-adapter-agentsight/tests/translation.rs)，
三处快照重开及旧编码拒绝见 [SQLite 契约测试](../../v2/crates/asc-policy-repository-sqlite/tests/contracts.rs)。
[HTTP mock E2E](../../tests/v2/e2e/test_policy_delivery_e2e.py) 覆盖完整下发与含不支持规则时
保存成功、Binding 失败且无 HTTP 调用。测试验证本地数据和 mock 请求，不代表真实内核执行。
