# ADR-004: SQLite 作为默认存储

## 状态
已采纳

## 背景
AgentSight 需要持久化审计事件、Token 消耗记录、HTTP 记录和 GenAI 语义事件。需要选择存储引擎：SQLite、RocksDB、纯内存，或远程数据库。

## 决策
使用 **SQLite** 作为默认本地存储，通过 SLS logtail 文件作为云端导出通道。

## 理由
- 零依赖部署：SQLite 嵌入二进制，无需安装额外服务，适合 sysak 集成部署场景
- 查询灵活：审计、Token 统计、中断事件等查询需求多样，SQL 比 KV 存储更适合
- RocksDB 引入大量 C++ 编译依赖，与 eBPF 工具链的构建复杂度叠加会显著增加构建时间
- 纯内存方案不满足重启后数据保留的需求
- 远程数据库引入网络依赖，不适合边缘部署场景

## 后果
- SQLite 的写入并发受限（WAL 模式下单写多读），高并发写入场景需要连接池 + Mutex
- 当前实现中 49 处 `conn.lock().unwrap()` 是技术债，mutex poisoned 时会 panic
- 数据保留策略通过 `data_retention_days` 配置实现定期清理
- 云端持久化通过 SLS logtail 文件异步导出，与本地存储解耦
