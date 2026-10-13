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
仍需访问 SQLite 的伴随文件。该库不提供 HTTP 监听或凭据校验，二者由服务层负责。
