# 只读统计查询

[English](stats-query.md)

`tokenless_stats::reader::StatsReader` 打开已有的 `stats.db`，不会创建表、
迁移 schema 或改变统计写入行为。它为需要原始统计数据的集成提供查询入口。

## 接口约定

- `list(RecordQuery)` 按记录 ID 倒序返回元数据。默认 50 条，最多 100 条；
  使用 `next_before_id` 和相同的 session/tool-call 筛选条件继续翻页。
- 记录 ID 属于当前数据库。分页边界不提供数据库清空或替换后的恢复能力。
- `get(id, include_content)` 返回一条记录或 `None`。默认不包含内容；
  `content_included` 区分未请求内容与内容未存储时的 null。显式请求的 text
  和 output 不会被截断。
- 保留原始 RFC3339 时间、operation、mode 和可选归属字段。历史缺失字段为
  null，未知 mode 不转换为 Active。损坏的时间返回错误，不替换为当前时间。
- 必需列为 ID、时间、operation、agent ID 及压缩前后的字符和 Token 数量。
  旧表结构（包括随包 RTK 创建的表）缺少的可选字段为 null。按不存在的
  归属列筛选时明确返回不支持该表结构的错误。
- `applied_operations` 保留数据库中的 JSON 文本。Token 数量仍为估算值；
  生产者存储了计数器信息时，由 `tokenizer_id` 标识。
- 数据库缺失、不支持的表结构、损坏记录和 SQLite 读取失败均返回错误。
  不会静默跳过记录并把不完整页面当作成功结果。

查询入口使用 SQLite 只读连接，busy timeout 为 500 ms。读取实时 WAL 数据库
仍需访问 SQLite 的伴随文件。HTTP 监听和凭据校验由可选的 `query-server` 服务层负责。

`list_for_tools` 与带凭据的 `POST /v1/stats/records/query` 通过绑定的 JSON 数组批量匹配
最多 128 个精确 tool ID。POST JSON 包含 `tool_use_ids`、`limit`（1–100）及可选
`before_id`；请求体上限为 1 MiB。ID 非空且每个最多 1024 字节。分页只含元数据，
按源 ID 倒序，并使用与 GET 相同的排他游标；重复 ID 不会使记录重复。

## 查询成本与源端升级

仅为实际传入的筛选条件生成 SQL，所有值仍通过参数绑定。session 查询使用
生产者的 session 索引，ID 游标使用主键，tool 查询使用 `idx_tool_use_id`。
原生 recorder 下次打开已有数据库时补齐该索引；新数据库初始化时即包含它。
reader 不会创建索引或迁移源库。未包含该索引的旧库或仅由 RTK 写入的库仍可读，
但在新版原生 recorder 打开该库前，tool 匹配可能扫描全表。过渡期间需考虑
源端与 AgentSight 的查询预算；接口不承诺固定延迟。本次不增加源库自动保留
策略，也不删除源端数据。HTTP 服务启动时若缺少 `idx_tool_use_id`，会警告一次，
但不创建索引，也不阻止启动。

## 损坏记录诊断

匹配到损坏记录时整页仍失败。HTTP 500 `invalid_record` 包含 `record_id`
（ID 本身不是整数时为 null）和 `field`，不返回无效值或存储内容。按生产者
运维流程检查并修复指定源记录后，重试同一查询。手动诊断时可使用
`before_id=<record_id>` 读取该行之前的旧记录；这是主动缩小范围的查询，
不代表完整结果或自动恢复。AgentSight 继续明确报错，不使用部分结果计算统计。

HTTP 服务先鉴权再读取批量请求体，请求体上限仍为 1 MiB。鉴权后先识别路由，
未知路径（包括尾斜杠）在检查方法前返回 404。两个 GET 路由也支持 HEAD，
沿用相同的查询校验和状态，但不返回响应体。方法错误返回 405，包含
`Allow: GET, HEAD` 或 `Allow: POST`。非 loopback 明文监听及 Unix 凭据文件具有 group/other 权限时
会在启动时警告，但不改变原有启动成功行为。凭据文件建议使用 0600，远程
访问应使用私有网络或 TLS 代理。
