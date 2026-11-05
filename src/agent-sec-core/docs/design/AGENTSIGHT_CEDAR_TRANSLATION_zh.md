# AgentSight Adapter：Cedar 文件防护映射

[English](AGENTSIGHT_CEDAR_TRANSLATION.md)

本文是 AgentSight Adapter 的专属契约，涵盖 Translator、Deployment Client 与后端防护边界。

AgentSecCore 将 Cedar 的文件删除、普通写入和精确文件禁读限制翻译成 AgentSight 结构化文件防护策略。
本实现基于 `af1bc18a9` 已有的 Cedar 准入与持久化，原生 JSON 策略继续使用原有 DSL 路径。
生产 daemon 已注册 `AgentSightTranslator` 和 `AgentSightClientFactory`，两种前端共用既有部署生命周期。

## 1. 接口、版本与决策语义

| 边界 | 契约 |
|---|---|
| Cedar 源码 | `seccore.system` revision `1`；Policy 与 Scope/Binding 快照保存原文 |
| Translator 输入 | `PreparedBinding`；通过 `asc_policy_cedar::pst` 检查并生成 PST |
| 部署计划 | `agentsight.actplane.binding.v2`，`schemaVersion: 2` |
| 结构化策略 | `application/vnd.agentsight.file-protection.v1+json`；`content` 是 JSON 文本 |
| Sight HTTP 请求 | `POST /api/enforcement/bindings`；`policy_mode: "enforce"`，`file_protection_policy.version: 1` |
| 准备后的请求 | 既有 `agentsight.enforcement.apply.v2`；保存准确请求字节和进程身份 |

v2 部署计划与 version 1 文件防护策略是不同的版本。本次不引入原生策略 `specVersion: "0.2"`，
不修改 SQLite 版本。Client 明确支持文件防护 payload version 1。
其类型、版本和 media type 统一定义在 SecCore 的 `asc-agentsight-protocol`，供 Translator 和
Client 共用。该 crate 只依赖 `serde`，固定 JSON 样例用于检查序列化契约。
增加 payload 版本需要显式修改 Translator/Client，并验证与目标发布版本的兼容性。
结构化请求不发送 `policy_dsl`；旧 DSL 请求保持原有序列化和重放字节。
可选的 `protections.read` 默认为 false，为 false 时不序列化，保持原有 version 1 载荷兼容。
禁读要求目标报告 `file_read_protection`；旧目标会在更新清理前被拒绝。

此目标接收**附加 forbid 限制**：命中时阻止对应操作，未命中时本策略不增加限制。
这里不执行 Cedar Authorizer 的默认拒绝算法。Translator 拒绝 permit，包括混合策略集中的
permit；通用前端仍保留 permit 供其他消费者使用。不补隐含 permit-all，不把 permit 转成
排除项，也不在此链路实现通用 Cedar 请求授权服务。

## 2. 字段和表达式支持范围

| 输入 | 映射 |
|---|---|
| 无约束 principal | Scope/Binding 选择进程实例及进程树 |
| `action == SecCore::Action::"file.write"` | 支持；`action in [...]` 必须非空且只包含该 action |
| `action == SecCore::Action::"file.read"` | 精确文件的 `read: true`；`action in [...]` 只能包含该 action。读写限制通过独立语句组合。 |
| 无约束 resource 或 `resource is SecCore::File` | 支持 |
| `context.input.operation == SecCore::WriteOperation::"delete"` | `delete: true` |
| `context.input.operation == SecCore::WriteOperation::"write"` | `ordinary_write: true` |
| operation 的 OR 或字面集合 contains | 保留操作与路径关联；目标及排除项相同时合并开关 |
| `resource.path == "/data/a"` | `exact_file`；执行时按单一路径匹配 |
| `path == "/data" || path like "/data/*"` | `directory_tree`；以该路径为根按子树匹配 |
| 路径字面集合 contains、路径 OR | 展开为多个目标 |
| 有界范围内的 `!=`、`!`、`unless` | 能表达时转为条目或子目录树排除项 |
| `&&`、`||`、布尔常量 | 有界布尔转换，保留交集、并集及操作与路径关系 |

等号两侧、目录树表达式的两个分支可以交换。根路径相等与子路径 like 必须组成一个 OR
子树；本实现只识别这个明确子集，不承诺识别所有逻辑等价写法。根目录使用
`path == "/" || path like "/*"`。operation 的否定条件只有在最终范围仍限制为 write/delete
时才支持；引用 create/rename/setattr 会被拒绝。

路径必须是绝对字面路径，不能包含 NUL、`* ? [ ] { } \\`、重复分隔符、`.`、`..`，除 `/`
外不能有末尾斜杠。不补 cwd，不做 shell 展开或大小写折叠，不在 PAP 主机解析文件系统。
路径相等不代表整个目录树，也不等于 Cedar 实体 ID 相等。
部署沿用现有进程树 Scope；Translator 拒绝 `CgroupId` selector。

因为这里完全不解析主机文件系统，翻译与策略保存都不检查目标是否存在、是否为普通文件：
尚不存在的路径与指向目录的路径都可以被接受。`exact_file` 与 `directory_tree` 描述的是执行侧
比较观测路径的方式，不是本阶段校验的前置条件；规则最终能否作用于某个目标，取决于消费
翻译结果的对应后端能力。

排除项只属于所在目标，其他目标仍可以保护该条目。相同目标及相同排除项可合并操作开关；
相同路径的目标类型或排除列表不同时直接拒绝，不引入隐含优先级。只排除目录根、保留其
子树的情况被拒绝。精确文件目标不携带排除项。

转换限制为：64 个目标、64 个中间布尔分支、64 个字面集合元素、每分支累计 64 个排除项，
表达式递归深度 64。超限整份拒绝，不截断。转换结果为空时拒绝，不解释为撤销部署。
这些限制发生在 SDK 解析之后，不解决 Cedar 解析器自身的复杂度限制。

## 3. 拒绝规则与允许的语义降低

以下输入被拒绝：permit；未限制 action 或混合读写 action；exec/network；目录树禁读；未限制 operation 的 file.write；
create/rename/setattr；`context.input` 下的 `program`、`mode`、`ownerUid`、`ownerGid`、
`cwd`、`arg_count`、`args` 条件；resource 身份或层级条件；
任意 like 模式；无界补集；未支持的函数或表达式。常量 false 分支中的不支持条件也拒绝。
AgentSight 拒绝带主体约束、主体引用或注解的策略。共享前端校验 schema 版本，拒绝模板槽位，并校验支持的注解。

本目标只接受以下固定的后端语义差异：

| 映射 | 后端边界 |
|---|---|
| delete → 删除保护 | 还包含保持目标及祖先路径稳定、阻止移入未保护范围、覆盖和部分交换的限制；不是只拦 unlink，也不是禁止全部 rename |
| write → 普通写保护 | 使用结构化 `file_permission / MAY_WRITE` 检查；不能承诺完整防篡改，truncate、mmap 等修改途径不由该检查完整覆盖 |
| read → 精确文件禁读 | 保护 Apply 时持有的普通文件身份，包括别名、已有只读 FD 和新建映射。已有映射会导致 Apply 拒绝；外部替换文件或轮换密钥后须重新下发。 |
| 路径范围 → 文件系统目标 | Sight 在目标进程的文件系统视图解析条目、目录身份和相关别名；删除／普通写的路径范围不承诺保护该 inode 的全部硬链接 |
| `static_at_apply` | 下发时解析挂载拓扑；目录树覆盖后续新增文件，但不保证自动适配后续任意挂载变化 |

分类结果为 UNKNOWN 时，当前后端拒绝删除及相关 rename，普通写检查则放行。
只开启普通写保护不会禁止删除；两者都需要时同时开启两个开关。后端拒绝与保护范围相关
的 overlay/overlayfs 拓扑。精确文件及目录根不能是末级符号链接；中间符号链接按后端
解析器处理。schema 校验本身不证明这些运行时条件成立。

除此之外不允许语义降级：不得丢弃条件、操作、排除项或语句；不得把路径扩大为父目录；
不得把当前 glob 匹配文件枚举成长期规则；不得在失败后回退旧 DSL 或 audit 模式。
多拦截也属于语义变化，不能作为上述固定契约之外的默认处理。

## 4. 文件防护流程

防误删策略如下：

```cedar
forbid (principal, action == SecCore::Action::"file.write",
        resource is SecCore::File)
when {
    context.input.operation == SecCore::WriteOperation::"delete"
    && (resource.path == "/workspace" || resource.path like "/workspace/*")
}
unless {
    resource.path == "/workspace/cache"
    || resource.path like "/workspace/cache/*"
};
```

[防误删源码封装](../../v2/fixtures/adapters/agentsight/cedar-file-protection/prevent-delete.json)与
[禁止普通写入源码封装](../../v2/fixtures/adapters/agentsight/cedar-file-protection/prevent-write.json)
是测试实际使用的 fixture。后者将 operation 改为 `"write"`。
同一路径同时禁止两种操作时使用：

```cedar
[SecCore::WriteOperation::"delete", SecCore::WriteOperation::"write"]
    .contains(context.input.operation)
```

防误删生成的策略体为：

```json
{
  "version": 1,
  "topology_mode": "static_at_apply",
  "targets": [{
    "path": "/workspace",
    "kind": "directory_tree",
    "protections": {"delete": true, "ordinary_write": false},
    "exclusions": [{"path": "/workspace/cache", "kind": "directory_subtree"}]
  }]
}
```

只禁止普通写入时两个开关为 false/true，同时保护时为 true/true。不同操作对应不同路径时，
保持独立目标，不将所有操作扩大到所有路径。

禁读策略及其可执行的[源码封装 fixture](../../v2/fixtures/adapters/agentsight/cedar-file-protection/prevent-read.json)：

```cedar
forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/workspace/secret" };
```

它生成 `exact_file` 目标，开关为 `{"delete":false,"ordinary_write":false,"read":true}`，
不隐含禁止写入或删除。独立的读／写／删除语句仅在目标范围相同时合并开关。
目录树禁读返回 `CEDAR_RULE_<index>_UNSUPPORTED_READ_TARGET`。
后端要求 `agent-file-guard`、已启用的 BPF LSM 和对应禁读 hook。
Apply 期间须保持目标访问及进程树稳定；并发下发、io_uring/AIO、进程内存访问及动态挂载
不在保证范围内。已经读取到内存中的内容无法撤回。

各项防护共用以下步骤：

1. CLI/RPC 提交源码封装；PAP 校验后保存 Policy revision，语法或 schema 不合法不会生成新版本。
2. Scope 固定引用 Policy revision；发现进程实例后生成 Binding 快照。更新 Policy 不隐式更新已有绑定。
3. Reconciler 调用 Translator，将 PST 条件转为结构化计划；任意不支持内容使整个 Binding 翻译失败。
4. Client 检查 boot ID、PID namespace、进程 start time，准备准确 HTTP 字节；既有生命周期在修改目标前登记目标身份。
5. Preflight 检查 ready/ActPlane、静态拓扑及所需的精确文件删除、目录树删除、普通写和／或 `file_read_protection` 能力。缺少字段视为无能力，在清理旧目标和 POST 前拒绝。POST 前再次检查身份，回执必须匹配请求且为带 domain 的 enforced 状态，才确认 Present。
6. Sight 校验目标存在性、类型、拓扑及实际容量并安装保护；删除 Scope 时使用已保存的目标引用撤销。

V2 daemon 与 Sight 已配置时，在 `src/agent-sec-core/v2` 下运行：

```sh
agent-sec-cli policy create --name prevent-delete --language json --file fixtures/adapters/agentsight/cedar-file-protection/prevent-delete.json
agent-sec-cli policy create --name prevent-write --language json --file fixtures/adapters/agentsight/cedar-file-protection/prevent-write.json
agent-sec-cli scope create --pid <PID> --policy-id <POLICY_ID> --policy-revision <REVISION>
agent-sec-cli binding list
agent-sec-cli scope delete --scope-id <SCOPE_ID>
```

每个 Scope 使用选定的 Policy；同一进程需要两种保护时将条件合入一个 Policy。
创建 Policy 成功仅表示准入成功，应检查生成的 Binding 状态。部署前路径必须存在于目标进程的文件系统视图。

静态不支持返回 `CEDAR_RULE_1_UNSUPPORTED_CONDITION` 等有界错误码；Translator 内部对输入或
schema 不合法返回 `INVALID_CEDAR_POLICY`，空结果返回 `CEDAR_EMPTY_EFFECTIVE_POLICY`。
Policy 创建或更新 RPC 的准入校验失败返回 `invalid_argument`。
运行能力和目标准备失败由 Client/后端报告。Client 更新前先 preflight，再清理旧目标；
但后端路径和拓扑校验发生在 POST，仍可能出现旧保护已清理、新策略安装失败的情况。
本次沿用既有生命周期，不增加事务替换或自动回滚。

## 5. 实现划分、验证与回滚

`cedar.rs` 检查策略头并合并目标，`cedar/expressions.rs` 转换布尔条件，`cedar/paths.rs`
处理字面路径范围和排除项。这些都是私有翻译实现，不增加持久化 AST/IR。
Client 解码策略、检查能力并保留重放和回执核对。Reconciler 与 SQLite 存储格式不变。

在 `v2` 下执行：

```sh
cargo test -p asc-policy-translator-agentsight -p asc-agentsight-client -p asc-pcp --locked
```

Translator 测试在显式 permit 基线下，对照 Cedar 决策检查已支持的路径和操作条件，并覆盖
不支持输入和超限拒绝。Client 测试检查固定 version 1 JSON 请求、未知 payload 版本和字段拒绝、
准确字节重试、更新清理前的能力拒绝、无效策略及回执不匹配。
PCP 的 `cedar_file_protections_flow_through_pap_sqlite_reconciler_and_http` 对删除、写入和禁读 fixture
覆盖 PAP、SQLite、实例准入、真实 Translator/Client/Ureq、HTTP 回执、Binding Ready 与撤销。
实例发现和远端 PEP 使用受控替身；这些测试不证明真实内核防护或完整文件防篡改。

回滚到不支持 Cedar 翻译的二进制前，应先撤销结构化部署。原有 DSL 请求字节兼容；
Cedar 源码及快照保留[整体设计](POLICY_FRONTEND_AST_AND_TRANSLATION_zh.md)所述兼容边界。
本次没有数据库迁移或策略源码转换。

## 6. 原有 JSON 策略的翻译边界

`AgentSightTranslator::translate` 读取完整 Binding 快照，校验全部输入，将规则降低为
ActPlane DSL，再包装为目标计划。它不查询最新模板，不执行请求求值，也不进行目标 I/O。

| AST 要素 | Translator 接受范围 | 超出范围时 |
|---|---|---|
| 效果 | `block` | `RULE_<index>_UNSUPPORTED_EFFECT` |
| category/action | `file` / `write` | `RULE_<index>_UNSUPPORTED_ACTION` |
| 条件 | 精确为 `{"operation":{"eq":"delete"}}` | `RULE_<index>_UNSUPPORTED_CONDITION` |
| 历史 | 不携带 | `RULE_<index>_UNSUPPORTED_HISTORY` |
| 路径模式 | 字面路径，或字面前缀后接 `/**` | 拒绝不支持的 glob |
| 规模 | 最多 128 条；降低后的路径模式最多 63 bytes | 明确的上限错误 |
| DSL 字面量 | 路径/理由不含引号、反斜杠或控制字符 | 明确的路径/理由错误 |
| 挂载范围 | 以 Binding 固定实例为根的进程树 | 拒绝 cgroup selector |

任意一条合法规则不受支持，整个 Binding 转换失败，不会静默下发其中一部分。规则顺序
决定稳定生成的名称，不表示通用 first-match/last-match 判定。多条受支持的 block 规则
覆盖其匹配范围的并集。当前不做复杂条件化简。

以下模板取自
[prepared-Binding fixture](../../v2/crates/asc-policy-types/tests/fixtures/prepared-binding.json)
中的第一条规则：

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": { "type": "file", "path": "/etc/agent/config.yaml" },
      "where": { "operation": { "eq": "delete" } }
    }
  ]
}
```

生成的 DSL 为：

```text
source AGENT = exec "**"
rule agentseccore-unlink-0000:
  block unlink file "/etc/agent/config.yaml" if AGENT
  because "AgentSecCore file deletion policy"
```

显式 `because` 会替换默认理由。SecCore 生成格式为 `agentsight.actplane.binding.v2`
的 `TargetBindingPlan`，其中携带来源 ID/revision、DSL 和固定实例的进程树挂载信息。

源码：[翻译器](../../v2/crates/asc-policy-translator-agentsight/src/translate.rs)、
[计划结构](../../v2/crates/asc-policy-translator-agentsight/src/plan.rs)、
[翻译测试](../../v2/crates/asc-policy-translator-agentsight/tests/translation.rs)。

## 7. AgentSight 后端部署细节

原有 JSON/DSL 路径使用以下协议：

1. Client 重查进程身份，并要求健康接口 `ready=true`、backend 为 `actplane`、
   `capabilities.file_delete_guard=true`。随后携带认证 POST 到
   `/api/enforcement/bindings`，请求包含 DSL、Policy ID/revision、root PID、start time
   和 `policy_mode=enforce`。默认 API 根为 `http://127.0.0.1:7396/api`。产品 Binding
   当前不提供 Agent 身份，`agent_id` 为空，省略 `session_id`。
2. AgentSight 经 Coordinator、UDS 调用本地 Enforcer；Enforcer 编译 DSL，为目标进程
   建立执行域/标签，并向 ActPlane 提交编译后的策略字节。Client 只接受内容匹配且状态为
   `Enforced`、带 domain ID 的响应；随后通过 Repository 条件写保存观测和 `READY`。
   暂时性失败进入有界重试。

这里有两次不同的编译：SecCore AST → DSL，以及 Enforcer DSL → 执行配置。当前
ActPlane 编译器将 `write` 与 `unlink` 都降低为 `OP_WRITE`；生成 DSL 或收到部署响应
不证明真实内核只阻止删除。真实 deny 测试仍是独立证据。

源码：[Client](../../v2/crates/asc-agentsight-client/src/client.rs)、
[Enforcer](../../../agentsight/crates/agentsight-enforcer/src/actplane.rs)、
[操作降低](../../../agentsight/crates/actplane-ifc-compiler/src/dsl/lower.rs)。
