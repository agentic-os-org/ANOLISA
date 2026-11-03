# AgentSight Adapter：Cedar 文件防护映射

[English](AGENTSIGHT_CEDAR_TRANSLATION.md)

本文定义 AgentSight Translator 支持的 Cedar 子集和文件防护目标格式。
本实现基于 `af1bc18a9` 已有的 Cedar 准入与持久化，原生 JSON 策略保留既有 DSL 翻译。
Translator 生成结构化计划；Deployment Client 对该格式的支持尚未接入，当前 Client 会拒绝其 media type。

## 1. 接口、版本与决策语义

| 边界 | 契约 |
|---|---|
| Cedar 源码 | `seccore.system` revision `1`；Policy 与 Scope/Binding 快照保存原文 |
| Translator 输入 | `PreparedBinding`；通过 `asc_policy_cedar::pst` 检查并生成 PST |
| 部署计划 | `agentsight.actplane.binding.v2`，`schemaVersion: 2` |
| 结构化策略 | `application/vnd.agentsight.file-protection.v1+json`；`content` 是 JSON 文本 |

v2 目标计划与 version 1 文件防护策略是不同的契约。本次不引入原生策略
`specVersion: "0.2"`，不修改 SQLite 版本。SecCore 的 `asc-agentsight-protocol` 定义
version 1 payload 的类型、版本和 media type，供 Translator 与后续 Client 接入使用。
该 crate 只依赖 `serde`，固定 JSON 样例用于检查序列化契约。
支持新 payload 版本需要显式接入，并验证与目标发布版本的兼容性。

此目标接收**附加 forbid 限制**：命中时阻止对应操作，未命中时本策略不增加限制。
这里不执行 Cedar Authorizer 的默认拒绝算法。Translator 拒绝 permit，包括混合策略集中的
permit；通用前端仍保留 permit 供其他消费者使用。不补隐含 permit-all，不把 permit 转成
排除项，也不在此链路实现通用 Cedar 请求授权服务。

## 2. 字段和表达式支持范围

| 输入 | 映射 |
|---|---|
| 无约束 principal | Scope/Binding 选择进程实例及进程树 |
| `action == SecCore::Action::"file.write"` | 支持；`action in [...]` 必须非空且只包含该 action |
| 无约束 resource 或 `resource is SecCore::File` | 支持 |
| `context.input.operation == SecCore::WriteOperation::"delete"` | `delete: true` |
| `context.input.operation == SecCore::WriteOperation::"write"` | `ordinary_write: true` |
| operation 的 OR 或字面集合 contains | 保留操作与路径关联；目标及排除项相同时合并开关 |
| `resource.path == "/data/a"` | `exact_file`；部署时必须是已存在的普通文件 |
| `path == "/data" || path like "/data/*"` | `directory_tree`；部署时根必须是已存在的目录 |
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
部署沿用现有进程树 Scope；`CgroupId` selector 仍被该 Adapter 拒绝。

排除项只属于所在目标，其他目标仍可以保护该条目。相同目标及相同排除项可合并操作开关；
相同路径的目标类型或排除列表不同时直接拒绝，不引入隐含优先级。只排除目录根、保留其
子树的情况被拒绝。精确文件目标不携带排除项。

转换限制为：64 个目标、64 个中间布尔分支、64 个字面集合元素、每分支累计 64 个排除项，
表达式递归深度 64。超限整份拒绝，不截断。转换结果为空时拒绝，不解释为撤销部署。
这些限制发生在 SDK 解析之后，不解决 Cedar 解析器自身的复杂度限制。

## 3. 拒绝规则与允许的语义降低

以下输入被拒绝：permit；未限制 action；read/exec/network；未限制 operation 的 file.write；
create/rename/setattr；`context.input` 下的 `program`、`mode`、`ownerUid`、`ownerGid`、
`cwd`、`arg_count`、`args` 条件；resource 身份或层级条件；
任意 like 模式；无界补集；未支持的函数或表达式。常量 false 分支中的不支持条件也拒绝。
公共前端另行拒绝未知 schema revision、principal 约束及引用、模板槽位和全部 annotations，
包括 `@id`、`@advice`。

本目标只接受以下固定的后端语义差异：

| 映射 | 后端边界 |
|---|---|
| delete → 删除保护 | 还包含保持目标及祖先路径稳定、阻止移入未保护范围、覆盖和部分交换的限制；不是只拦 unlink，也不是禁止全部 rename |
| write → 普通写保护 | 使用结构化 `file_permission / MAY_WRITE` 检查；不能承诺完整防篡改，truncate、mmap 等修改途径不由该检查完整覆盖 |
| 路径范围 → 文件系统目标 | Sight 在目标进程的文件系统视图解析条目、目录身份和相关别名；精确文件保护不代表保护该 inode 的全部硬链接 |
| `static_at_apply` | 下发时解析挂载拓扑；目录树覆盖后续新增文件，但不保证自动适配后续任意挂载变化 |

分类结果为 UNKNOWN 时，当前后端拒绝删除及相关 rename，普通写检查则放行。
只开启普通写保护不会禁止删除；两者都需要时同时开启两个开关。后端拒绝与保护范围相关
的 overlay/overlayfs 拓扑。精确文件及目录根不能是末级符号链接；中间符号链接按后端
解析器处理。schema 校验本身不证明这些运行时条件成立。

除此之外不允许语义降级：不得丢弃条件、操作、排除项或语句；不得把路径扩大为父目录；
不得把当前 glob 匹配文件枚举成长期规则；不得在失败后回退旧 DSL 或 audit 模式。
多拦截也属于语义变化，不能作为上述固定契约之外的默认处理。

## 4. 防误删和禁止写入示例

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

翻译遇到不支持输入时返回 `CEDAR_RULE_1_UNSUPPORTED_CONDITION` 等错误码；
Translator 对无效 Cedar 返回 `INVALID_CEDAR_POLICY`，对空结果返回 `CEDAR_EMPTY_EFFECTIVE_POLICY`。

## 5. 实现划分与验证

`cedar.rs` 检查策略头并合并目标，`cedar/expressions.rs` 转换布尔条件，`cedar/paths.rs`
处理字面路径范围和排除项。这些是私有翻译实现；Reconciler 与 SQLite 格式保持不变。

在 `v2` 下执行：

```sh
cargo test -p asc-policy-adapter-agentsight --locked
```

测试在显式 permit 基线下，对照 Cedar 决策检查已支持的路径和操作条件，并覆盖不支持输入
和超限拒绝。这些测试验证翻译结果；部署与真实内核防护还需要 Client 接入和单独验证。
