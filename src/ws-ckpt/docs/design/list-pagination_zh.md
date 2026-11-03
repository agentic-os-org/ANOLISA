# 快照列表分页

[English](list-pagination.md)

在 bincode 请求和响应枚举末尾分别追加
`Request::ListPage { orphans_only: bool, workspace: Option<String>, limit: u32, cursor:
Option<String> }` 与 `Response::ListPageOk { snapshots, next_cursor }`。已有枚举
序号以及旧 `List`、`ListOrphans`、`ListOk` 的精确字段布局保持不变。`ListOrphans`
的请求序号仍为 28，新 `ListPage` 追加为 29。游标分页要求新版 CLI 和
daemon；旧的非分页协议消息仍可解码。

Daemon 按 `(created_at, workspace_id, snapshot_id)` 升序排列。游标是不透明且带版本
的，绑定 daemon 解析后的工作区范围和 orphan 过滤条件（`list --orphans`），并保存续翻键和第一页观察到的最大键。
上界不依赖墙上时间，校时后带有未来时间戳的既有快照仍可查询。格式错误、版本
不支持、过长或跨范围的游标都会被拒绝。第一页上界之后的新记录不会混入；位于上界内的
并发插入和删除仍会影响后续页，因此这是单调遍历而不是事务快照。

单页请求最多 10,000 条。每次查询只保留游标之后最小的 `limit + 1` 个候选键，再读取并
克隆选中的记录。比较和过滤先借用索引字段，仅复制入选候选键和观察到的上界，避免复制
和排序全部元数据。页面目标约为 1 MiB，发送前还会检查不超过 16 MiB IPC payload 硬上限。
删除或重建候选被跳过时，不能替换已经与返回条目共同通过预算检查的游标；空页仍可越过
被跳过的键。最终条目与游标组合在返回前再次验证。

若单条记录的可选字段会导致超过硬上限，daemon 返回摘要，保留 `id`、工作区、
`created_at`、`pinned` 和 `missing`。CLI JSON 中这三个字段仍位于 `meta` 下，与完整记录
保持一致；外层通过 `detail: "summary"` 与 `omitted_fields` 明确标记。被省略的可选字段
在 `meta` 中缺席，不伪造成 null。该快照仍可通过 ID 操作，不会静默截断完整记录。

未指定 `--limit` 和 `--cursor` 时，CLI 自动跟随全部游标，并保持历史输出契约：JSON 是
单个数组，表格是完整列表。聚合 JSON 会先缓存，若后续页失败则不输出不完整结果。显式
分页 JSON 是包含 `snapshots` 和 `next_cursor` 的对象；表格会标记摘要并输出下一游标。

Daemon 读取内存快照索引，不扫描快照文件内容。各工作区维护运行期身份键有序集合，
分页直接定位到续翻键之后；后续键无法改进有限候选集合时停止遍历。注册和恢复时重建
顺序，普通插入和删除增量维护；整个索引替换时比较新旧成员并更新变化的键。该结构增加
O(N) 的常驻身份存储，不改变持久化索引或游标格式。稀疏 orphan 查询仍可能检查剩余
范围中的非 orphan 键，包括确定第一页上界时，因此不承诺固定延迟。回归测试使用
100,000 条合成索引，覆盖帧超限、接近 16 MiB 的元数据、错误/跨范围游标、并发插入
删除、摘要传播以及后续页面失败。有序遍历测试还在并列时间戳、工作区范围、orphan
过滤和实时索引替换下，将完整结果与独立排序的索引进行比较。

每个分页请求（包括后续页和失败）都记录为 `ops_name: "list_page"`、`list_time: 0`、
`ops_time: 1`。旧 `List` / `ListOrphans` 保留 `ops_name: "list"` 和 `list_time: 1`。日志表示 daemon
请求结果，尚无整次查询关联或 CLI 最终结果事件，不能用于计算整次查询次数和成功率。

## Hermes 工具分页

现有 `ws-ckpt-list` 工具在出现 `limit` 或 `cursor` 任意键时启用单页 JSON 输出。
handler 区分键缺失与 null，在调用 CLI 前拒绝类型或范围错误的 limit、空或非字符串
cursor，并原样传递不透明游标。仅传 cursor 时不添加 CLI limit 参数，沿用默认值 1000。
两个键都不存在时，保留原有表格命令及 `{success, output}` 响应。

分页响应必须是对象，包含元素均为对象的 `snapshots` 数组，以及显式存在、值为 null
或非空字符串的 `next_cursor`。handler 返回含 `success`、`snapshots` 和 `next_cursor`
的 JSON 字符串，忽略 CLI 顶层附加字段。快照对象的全部嵌套字段均保留，包括摘要标记
及未来扩展。仅 null 游标表示结束；handler 不根据页长推断完成，不自动追踪游标，
不将失败重试为完整列表，也不修改配置或 checkpoint/rollback 状态。

工具描述建议使用有数量上限的第一页，并按需续页。验证覆盖注册后的 schema、精确 CLI
参数及单次调用、无效参数、异常响应、短页和空页的续页游标、摘要保留及子进程失败。
OpenClaw 和 Rust 协议保持不变；分页仍需支持该功能的 CLI 和匹配的 daemon。


### 已安装 Hermes 的验收

`tests/hermes_pagination_smoke.py` 使用已安装的 Hermes 插件加载器和工具注册表，
并通过正常插件安装脚本在临时 Hermes profile 中启用插件。请使用该 Hermes 安装
对应的 Python 环境运行。需要事先启动专用 daemon，注册两个 Btrfs 工作区，并在
第一个工作区创建至少五个真实快照。从仓库根目录运行：

```bash
python3 src/ws-ckpt/tests/hermes_pagination_smoke.py \
  --socket /tmp/ws-ckpt-pagination.sock \
  --workspace /tmp/ws-ckpt-pagination-a \
  --other-workspace /tmp/ws-ckpt-pagination-b
```

脚本比较原生首页面、续页和完整 CLI 结果，检查默认表格输出及仅传 cursor 的续页，
并要求无效参数、跨工作区游标和不可用 socket 返回明确错误。结束前检查快照列表
未改变。脚本不创建工作区或快照，不调用模型。这是手动验收工具，不表示 Btrfs
或交互式 Hermes 验收已经通过。
