# AgentSecCore 的 PEP 策略选择接口

[English](CEDAR_PEP_SELECTION.md) · [统一策略管理与自动下发总方案](CEDAR_PRINCIPAL_POLICY_DEPLOYMENT_zh.md)

本文面向实现策略消费方的开发者。基线 `d41cede90` 支持 Cedar v3 自动下发；本分支增加三个 PEP 共用的 `select` 接口。PAP 保留策略原文，每个 PEP 决定消费哪些语句和分支，按策略集版本复用结果。[接口类型](../../v2/crates/asc-policy-cedar/src/selection.rs) · [公共缓存](../../v2/crates/asc-pap/src/selection.rs)

## 1. 一条规则怎样被消费

用户希望禁止 Qoder 在 `/work/reports` 及其子路径上普通写入、删除或重命名。

```cedar
forbid (
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when { principal has agentType && principal.agentType == "qoder" }
when { resource.path == "/work/reports" || resource.path like "/work/reports/*" }
when {
    context.input.operation == SecCore::WriteOperation::"write" ||
    context.input.operation == SecCore::WriteOperation::"delete" ||
    context.input.operation == SecCore::WriteOperation::"rename"
};
```

三个操作共用同一主体和路径。选中一个或多个分支时，保留 `forbid`、Qoder、路径和适用的注解，只裁剪最后一个 `when`。

| PEP | 选中的操作 | 下游实际输入与结果 |
|---|---|---|
| Sandbox | `write` | PDP 将保留普通写入分支的 Cedar 语句与其他适用的许可、禁止一起装载。Qoder 即使有通用写许可，申请写 `/work/reports/q1.txt` 时也会被拒绝。Sandbox 启动时构造的文件写请求使用 `write`。 |
| ActPlane | `write`、`delete` | 自动 Scope 保存这条含两个分支的完整片段。daemon 匹配 Qoder 后，编译器从快照读取该片段，生成普通写入和删除保护。编译输入不含 `rename`。 |
| Gateway | 空，与 HTTP 无关 | 该文件规则不进入凭据计划。对 HTTP 规则，Gateway 从缓存取选中片段，再按主体范围及凭据授权、限制的原有规则组装各 Scope。 |

在这个例子里，三个 PEP 都未承接通用的 `rename` 禁令。ActPlane 的删除保护为了维持路径稳定，可能额外阻止上级目录的重命名，属于收紧。其目录树普通写保护无法覆盖共享可写内存映射产生的全部修改，属于放松。这两项均是带条件的能力说明，进程实际是否挂接、操作是否被阻止由运行时回执或事件说明。[Sandbox 请求构造](../../v2/crates/asc-pdp-runtime/src/request.rs) · [文件保护边界](../../../agentsight/docs/design/actplane-file-enforcement-profile_zh.md)

## 2. 接口契约

```rust
type Select = fn(&ValidatedStatement<'_>) -> Selection;

struct Selection {
    source: StatementRef,
    selected: SelectedStatement,
    deviations: Vec<EnforcementDeviation>,
}
```

- **输入。** 一条已经通过原始 schema 校验的完整语句，带 Policy ID、Policy 修订、语句 ID、schema 引用、条件和注解。同一实现对相同输入给出相同选择。进程、请求、可用凭据和已发布 Scope 由消费方在之后核验。
- **`source` 指回原句。** 用 Policy ID、修订和语句 ID 确定唯一来源。
- **`selected` 决定执行输入。** 整句可以引用固定修订中的原句。部分选中时返回可独立装载的完整 Cedar 片段及其在原句中的分支位置。Sandbox 直接装载片段；部署路径将片段写入自动 Scope 快照，Binding 只消去该 Scope 已核实的主体条件。空结果区分“与本 PEP 无关”和“相关但无法消费”，后者附原因。可能影响某项授权的限制仍交给该后端作准入判断。
- **`deviations` 描述已知执行差异。** 对应适配模块给出原文分支、收紧或放松、原因码、出现条件和说明。未来管理端可按来源汇总覆盖和差异。只能整句求值、无法细分覆盖时保留“未确定”。本期仅提供返回契约，查询 API 和页面留待后续。

首版只裁剪能保持原意的分支。“写 A 或删 B”选中删除后仍须是“删 B”。Sandbox 对无法安全裁剪、但可能影响其请求的语句，保留整句及 `when`、`unless` 和注解，沿用现有路径检查与 Cedar 求值。合法 Cedar 若无法生成所需的 Sandbox 权限，保留装载错误。v1/v2 先按现有整份 Policy 的 principal 头部判断请求准入，原先只分配给手工 Scope 的规则继续用于该快照。[旧版准入](../../v2/crates/asc-policy-cedar/src/lib.rs) · [路径检查](../../v2/crates/asc-pdp-runtime/src/path_conditions.rs)

## 3. 缓存和发布

公共调用层维护进程内缓存，键为 **PAP 策略集更新编号和 PEP 类型**。缓存保存静态选择，三个 PEP 各自复用自己的结果。每个 PEP 只需保留当前选择；已发布的执行内容由 PDP 或 Scope 快照承载。

1. PAP 在新增、修改或删除策略的提交事务中递增更新编号。请求可单独读取编号。缓存命中时直接使用选中片段或据此装载的 PDP。未命中时，在一致的读快照中取得编号和策略，复用同版解析结果，对该 PEP 全量筛一轮。按快照的实际编号保存结果。
2. 同一 PEP、同一版本的并发装载共享一次计算。延续每个 PDP 的刷新锁和部署发布前的来源修订核对，迟到的旧结果不能覆盖已生效的新版本。更新编号仅供缓存失效判断，Sandbox 回执的 `policy_version` 继续标识参与授权的策略集。无法取得或装载当前授权策略时返回现有错误。
3. Sandbox 每次请求重新求值。ActPlane 对新实例做匹配、按需核验 UID 并挂接。Gateway 按 Scope 编译，在成功准入和发布后切换，HTTP 请求使用已发布计划。在运行中候选准入被拒时，已有进程和新匹配进程继续用当前服务版本。重启后重建内存缓存，按已有恢复流程核对自动凭据路由后才恢复流量。

升级前应备份 SQLite 策略数据库。首次启动时，仓库将数据库从版本 1 升至 2 并初始化策略集版本号。如果表结构或已保存的策略未通过校验，事务回滚，数据库仍为版本 1。升级成功后若要回退到旧版程序，需要恢复升级前的一致备份。[数据库迁移](../../v2/crates/asc-policy-repository-sqlite/src/migration.rs)

## 4. 两种重试分别取哪个版本

| 重试的工作 | 版本来源 | 例子 |
|---|---|---|
| 执行某个已发布 Binding 的安装 | 取该 Binding 固定的执行快照，确认任务仍属于当前 Binding 修订；过期任务交由协调器淘汰。 | ActPlane 安装失败且分配仍有效时，重试原来的两个分支。若其间已撤销或替换该 Binding，旧任务不能重新安装。 |
| 管理员执行自动 `scope retry`，或 worker 重试尚未发布的候选 | 核对 PAP **当前**策略集，复用该版本的选择，再组装候选并重新准入。首次失败可以只有候选 ID。 | Gateway 正在服务 v1（GET），v2 增加 POST 但准入失败。故障修复后重试 v2；如果 PAP 已改成 v3，就尝试 v3；来源已删除则走撤销流程。 |

[自动 Scope 重试](../../v2/crates/asc-pap/src/service.rs) · [首次发布失败后的重试回归](../../v2/crates/asc-pap/tests/automatic_scopes.rs)

## 5. 验收用例

| 用例 | 操作及可观测结果 |
|---|---|
| 片段与下游一致 | 提交第 1 节的策略，检查 PDP 输入含普通写分支，ActPlane 编译输入含普通写和删除，Gateway 输入为空。比较有通用许可时 Qoder 与其他类别的实际 Sandbox 决策。对“写 A 或删 B”验证片段保留操作和路径的对应关系。 |
| 三个 PEP 复用 | 从空缓存开始，对同一版本分别计数。连续发送 1,000 次 Sandbox 授权和多次 HTTP 请求，多次匹配 Qoder 进程并规划多个 Scope。每个实际使用该版本的 PEP 只进行一轮全量筛选；核对实例和请求仍分别执行。UID 1000、1001 各有凭据 Scope 时，仅限 1001 的 POST 禁令只进入 1001 的候选。 |
| 保留旧版范围与回执 | 只给一个 PID 手工绑定旧 v2 文件禁令。修改该手工策略，确认其他进程的 Sandbox 决策和回执策略集版本均保持原值。 |
| 更新与并发装载 | 在通用测试环境中只用 v7 许可读取目标文件。用同步屏障暂停 v7 装载，提交 v8 删除许可后发起新请求，再让旧装载完成。所有提交后发起的请求均拒绝并对应 v8 的授权策略集；同一版本的并发请求只触发一轮筛选。另让旧部署候选延迟到新候选发布后再提交，核对旧候选无法恢复原授权。 |
| 重试与恢复 | 分别触发第 4 节两类失败。核对 Binding 的修订和片段，观察 Gateway 实际 GET/POST 在候选发布成功前后的结果。再覆盖首次准入失败尚无快照、重试前已换版或已删除来源、重启后需先核验的情形。 |
