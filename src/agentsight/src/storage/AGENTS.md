# Storage Layer Rules

> 适用于 `src/storage/` 目录下所有文件。

1. SQL 查询必须使用参数化语句（`?` 占位符），禁止字符串拼接
2. 新 store 类型必须实现统一的 trait 接口，通过 `Storage` 统一访问
3. 数据库 schema 变更必须向后兼容，不得破坏已有数据
4. `conn.lock().unwrap()` 是已知技术债 — 新代码应使用 `map_err` 处理 mutex poisoned
5. 查询接口必须支持时间范围过滤，与 `data_retention_days` 清理策略一致
