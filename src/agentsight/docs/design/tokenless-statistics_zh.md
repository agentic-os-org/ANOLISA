# Tokenless 统计记录

[English](tokenless-statistics.md)

服务使用仅保存在后端的凭据查询已配置的 Tokenless API，将有界查询结果以事务保存到
现有 GenAI store，复用数据库生命周期和保留策略。原始源字段保持不变，本地会话关联
仅采用唯一匹配的已采集 tool-call ID；缺失或歧义匹配保持 null。端点、源 ID 与原始
时间戳用于重复刷新去重，不引入恢复协议。刷新元数据时保留已缓存的完整内容。
失败明确返回；源服务离线时仍可查看已保存的统计数据。
