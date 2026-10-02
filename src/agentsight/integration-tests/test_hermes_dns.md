# Hermes DNS 集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

验证通过 UDP DNS 方式捕获 Hermes agent（连接 `dashscope.aliyuncs.com`）的完整流程：

1. 配置含 `https: [{"rule": ["*.dashscope.aliyuncs.com"]}]` 时，UDP DNS BPF 探针应被加载并 attach（判定依据：启动日志无 "UDP DNS probe disabled"）
2. Hermes 进程发起 DNS 查询 `dashscope.aliyuncs.com` 时，DNS 事件触发 SSL 探针 attach（判定依据：SQLite `genai_events` 表含 hermes pid 的记录；且**不含** cmdline 发现 hermes 的记录，证明 attach 仅由 DNS 触发）
3. SSL 探针 attach 后，能捕获 hermes 对 `dashscope.aliyuncs.com/compatible-mode/v1` 的 LLM API 调用（判定依据：SQLite `http_records` 表含 path 为 `/compatible-mode/v1` 的记录）
4. 不匹配 `https` 规则的域名不触发 attach（判定依据：日志中出现 DNS 事件但无 `Attaching via domain rule` 行）
5. 被 cmdline deny 规则匹配的进程（如 `curl`），即使域名匹配也不 attach（判定依据：DNS 事件被捕获但无 `Attaching via domain rule` 行）

## 判定方法

优先使用 **SQLite 查询**验证数据落库，日志仅辅助定位：

| 方法 | 适用场景 |
|------|----------|
| `sqlite3 <db_path> "SELECT ..."` | 验证数据是否落库（主要判定） |
| 日志 grep 关键行 | 辅助定位问题、确认过程 |

数据库默认路径：`/var/log/sysak/.agentsight/agentsight.db`

## 测试配置

使用以下 JSON 配置文件（保存到测试机 `/etc/agentsight/config.json`）：

> **注意**：cmdline allow 中不包含 hermes 规则，确保 attach 仅由 DNS 域名匹配触发，而非 cmdline 发现。deny 规则保留 `*curl*` 用于验证 deny 逻辑。

```json
{
  "cmdline": {
    "deny": [
      {"rule": ["*curl*"]}
    ]
  },
  "https": [
    {"rule": ["*.dashscope.aliyuncs.com"]}
  ]
}
```

## 测试步骤

### 步骤 1：验证 UDP DNS 探针加载

1. 将上述配置写入 `/etc/agentsight/config.json`
2. 启动 `agentsight trace --verbose`
3. grep 日志确认无 "UDP DNS probe disabled"

### 步骤 2：验证 DNS 域名匹配触发 attach

1. 保持 agentsight trace 运行
2. 启动 Hermes agent 进程，确保其向 `https://dashscope.aliyuncs.com/compatible-mode/v1` 发起请求（会先触发 DNS 查询）
3. 等待 hermes 完成至少一次 LLM API 调用
4. 查询 SQLite 验证 DNS attach 生效（hermes pid 的数据已落库）：
   ```bash
   sqlite3 /var/log/sysak/.agentsight/agentsight.db \
     "SELECT * FROM genai_events WHERE pid=<hermes_pid> LIMIT 5"
   ```
   预期：返回至少 1 条记录
5. 确认 hermes 仅通过 DNS 触发而非 cmdline 发现（无 cmdline allow 规则匹配 hermes，上述配置中 cmdline allow 为空）

### 步骤 3：验证 LLM API 调用被捕获

1. 查询 SQLite http_records 表，确认请求路径：
   ```bash
   sqlite3 /var/log/sysak/.agentsight/agentsight.db \
     "SELECT method, path, host FROM http_records WHERE path LIKE '%compatible-mode%' LIMIT 5"
   ```
   预期：返回含 `POST /compatible-mode/v1` 和 `dashscope.aliyuncs.com` 的记录

### 步骤 4：验证不匹配域名不触发 attach

核心验证：域名不匹配 `https` 规则时，SSL 探针**未 attach**。

1. 用任意进程发起 DNS 查询到不匹配域名（如 `nslookup example.com` 或 `curl https://example.com`）
2. grep 日志确认 DNS 事件被捕获但**未触发 attach**：
   - 应出现 `[UDP-DNS] pid=<pid> domain=example.com`（DNS 事件被捕获）
   - 应**不**出现 `[UDP-DNS] Attaching to pid=<pid> via domain rule (domain=example.com)`

### 步骤 5：验证 deny 规则阻止 attach

核心验证：curl 的域名匹配 `https` 规则，但被 cmdline deny 规则阻止，SSL 探针**未 attach**。

1. 运行 `curl https://dashscope.aliyuncs.com/compatible-mode/v1`（域名匹配但进程被 deny）
2. grep 日志确认 curl 的 DNS 事件被捕获但**未触发 attach**：
   - 应出现 `[UDP-DNS] pid=<curl_pid> domain=dashscope.aliyuncs.com`（DNS 事件被捕获）
   - 应**不**出现 `[UDP-DNS] Attaching to pid=<curl_pid> via domain rule`（deny 规则阻止了 attach）
3. SQLite 无 curl pid 的记录作为附加验证（attach 未发生，自然无数据落库）

## 运行条件

- root 权限（eBPF 要求）
- Linux kernel >= 5.8 with BTF
- 网络可达 `dashscope.aliyuncs.com`（需发起外部 DNS 查询）
- 测试机上有可运行的 Hermes agent 进程
