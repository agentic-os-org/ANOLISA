# 快照列表分页

[English](list-pagination.md)

在 bincode 请求和响应枚举末尾分别追加
`Request::ListPage { orphans_only: bool, workspace: Option<String>, limit: u32, cursor:
Option<String> }` 与 `Response::ListPageOk { snapshots, next_cursor }`。已有枚举
序号以及旧 `List`、`ListOrphans`、`ListOk` 的精确字段布局保持不变。`ListOrphans`
的请求序号仍为 28，新 `ListPage` 追加为 29。游标分页要求新版 CLI 和
daemon；旧的非分页协议消息仍可解码。

Daemon 按 `(created_at, workspace_id, snapshot_id)` 升序排列。游标是不透明且带版本
的，绑定 daemon 解析后的工作区范围和 orphan 过滤条件（`list --orphans`），并保存续翻键和首次索引扫描观察到的最大键。
上界不依赖墙上时间，校时后带有未来时间戳的既有快照仍可查询。格式错误、版本
不支持、过长或跨范围的游标都会被拒绝。第一页上界之后的新记录不会混入；位于上界内的
并发插入和删除仍会影响后续页，因此这是单调遍历而不是事务快照。

单页请求最多 10,000 条。每次扫描只保留游标之后最小的 `limit + 1` 个候选键，再读取并
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

Daemon 读取内存快照索引，不扫描快照文件内容。每页仍扫描完整查询范围：N 条快照、
P 页需要 O(N * P) 次索引访问，另加候选维护和序列化。减少分配不等于消除重复扫描；
要直接定位续翻键，需要有序索引。不承诺固定延迟。回归测试使用 100,000 条合成索引，覆盖帧超限、接近
16 MiB 的元数据、错误/跨范围游标、并发插入删除、摘要传播以及后续页面失败。

每个分页请求（包括后续页和失败）都记录为 `ops_name: "list_page"`、`list_time: 0`、
`ops_time: 1`。旧 `List` / `ListOrphans` 保留 `ops_name: "list"` 和 `list_time: 1`。日志表示 daemon
请求结果，尚无整次查询关联或 CLI 最终结果事件，不能用于计算整次查询次数和成功率。
