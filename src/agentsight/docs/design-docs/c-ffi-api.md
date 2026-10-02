# AgentSight C FFI API

本文档描述 AgentSight 提供的 C 语言接口。采用 **eventfd + read 模式**：AgentSight 内部通过一个有界队列和一个 `eventfd` 通知调用方有新事件就绪。旧调用方继续使用 `agentsight_read()`；需要系统安全审计的调用方使用 `agentsight_read_v2()`，不需要创建第二个 handle 或 eventfd。

## 1. C 数据结构

```c
/* HTTP 层数据 — 非 LLM 的 HTTPS 流量会产生此结构 */
typedef struct {
    int32_t     pid;
    char        process_name[16];
    uint64_t    timestamp_ns;
    uint64_t    duration_ns;
    const char* method;               /* "GET", "POST", ...; NUL-terminated */
    const char* path;                 /* "/v1/chat/completions"; NUL-terminated */
    uint16_t    status_code;
    uint8_t     is_sse;
    const char* request_headers;      /* JSON string */
    uint32_t    request_headers_len;
    const char* request_body;         /* JSON or raw text, may be NULL */
    uint32_t    request_body_len;     /* 0 when request_body is NULL */
    const char* response_headers;     /* JSON string */
    uint32_t    response_headers_len;
    const char* response_body;        /* JSON or raw text, may be NULL */
    uint32_t    response_body_len;    /* 0 when response_body is NULL */
    /* 进程归属，与 AgentsightLLMData 中的同名字段同义。这三个字段追加在结构体
       尾部，因此按旧 layout 编译的调用方无需改动即可继续工作。 */
    char        cmdline[128];         /* 空格连接的 argv，截断到 127 字节；进程已退出时为空串 */
    const char* agent_name;           /* may be NULL；统一为小写；未命中配置规则时回退为进程名 */
    const char* container_id;         /* may be NULL；非容器进程为 NULL */
} AgentsightHttpsData;

/* LLM 语义层数据 — 仅当 HTTP 流量被识别为 LLM API 调用时产生 */
typedef struct {
    /* 追踪标识 */
    const char* response_id;          /* LLM API 响应 ID（如 chatcmpl-xxx）; may be NULL */
    const char* conversation_id;      /* 同一 user query 触发的调用链标识; may be NULL */
    const char* session_id;           /* may be NULL */

    /* 进程 */
    int32_t     pid;
    char        process_name[16];
    char        cmdline[128];         /* 空格连接的 argv，截断到 127 字节；进程已退出时为空串 */
    const char* agent_name;           /* may be NULL；统一为小写 */

    /* 时间与延迟 */
    uint64_t    timestamp_ns;
    uint64_t    duration_ns;

    /* 请求信息 */
    const char* request_url;          /* 完整 API 地址，如 "https://api.openai.com/v1/chat/completions" */
    const char* provider;             /* "openai", "anthropic", ... */
    const char* model;
    uint16_t    status_code;
    uint8_t     is_sse;

    /* LLM 特有信息 */
    const char* finish_reason;        /* "stop", "length", "tool_calls", ...; may be NULL */

    /* Token 用量（无信息时全为 0）
     * llm_usage: true = 数据来自 LLM API 响应中的 usage 字段（精确值）
     *            false = 由 AgentSight 本地 tokenizer 计算 */
    bool        llm_usage;
    uint32_t    input_tokens;
    uint32_t    output_tokens;
    uint32_t    total_tokens;
    uint32_t    cache_creation_input_tokens;
    uint32_t    cache_read_input_tokens;

    /* 请求/响应语义内容（JSON 字符串） */
    const char* request_messages;     /* LLMRequest.messages 序列化 JSON */
    uint32_t    request_messages_len;
    const char* response_messages;    /* LLMResponse.messages 序列化 JSON */
    uint32_t    response_messages_len;

    /* 工具定义（JSON 数组字符串） */
    const char* tools;                /* LLMRequest.tools 序列化 JSON 数组; 无工具时为 "[]" */
    uint32_t    tools_len;

    /* 增量输入消息（JSON 数组字符串）：与 SQLite genai_events.input_messages 同一套算法，
       去掉 system 消息后，保留从最后一个 user 消息开始（含）到末尾的部分 */
    const char* input_message_delta;
    uint32_t    input_message_delta_len;
} AgentsightLLMData;

typedef enum {
    AGENTSIGHT_EVENT_TYPE_HTTPS = 1,
    AGENTSIGHT_EVENT_TYPE_LLM = 2,
    AGENTSIGHT_EVENT_TYPE_SECURITY = 3,
} AgentsightEventType;

/* 通用版本化事件 envelope；payload_json 仅在回调期间有效。 */
typedef struct {
    AgentsightEventType event_type;
    uint16_t            schema_version; /* 当前为 1 */
    uint64_t            timestamp_ns;
    const char*         payload_json;
    uint32_t            payload_json_len;
} AgentsightEvent;
```

## 2. C API 接口

```c
/* ---- 错误处理 ---- */
/* 返回最近一次 API 调用的错误描述，未出错时返回 NULL。
 * 返回的指针在下次 API 调用前有效，调用者应立即拷贝。 */
const char* agentsight_last_error(void);

/* ---- 配置 ---- */
AgentsightConfigHandle* agentsight_config_new(void);
void agentsight_config_set_verbose(AgentsightConfigHandle* cfg, int verbose);
void agentsight_config_set_log_path(AgentsightConfigHandle* cfg, const char* path);
void agentsight_config_set_enable_raw_https(AgentsightConfigHandle* cfg, int enabled);
void agentsight_config_set_enable_security_audit(AgentsightConfigHandle* cfg, int enabled);
void agentsight_config_set_enforcer_socket(AgentsightConfigHandle* cfg, const char* path);
void agentsight_config_add_cmdline_rule(AgentsightConfigHandle* cfg, const char* const* rule, const char* agent_name, int allow);
void agentsight_config_add_https(AgentsightConfigHandle* cfg, const char* rule);
int agentsight_config_add_http(AgentsightConfigHandle* cfg, const char* target);
int agentsight_config_load_config(AgentsightConfigHandle* cfg, const char* json_str);
void agentsight_config_free(AgentsightConfigHandle* cfg);

/* ---- 回调类型 ---- */
typedef void (*agentsight_https_callback_fn)(const AgentsightHttpsData* data, void* user_data);
typedef void (*agentsight_llm_callback_fn)(const AgentsightLLMData* data, void* user_data);
typedef void (*agentsight_event_callback_fn)(const AgentsightEvent* data, void* user_data);

/* ---- 生命周期 ---- */
AgentsightHandle* agentsight_new(AgentsightConfigHandle* cfg);
int agentsight_start(AgentsightHandle* h);
int agentsight_stop(AgentsightHandle* h);
void agentsight_free(AgentsightHandle* h);
const char* agentsight_version(void);

/* ---- 事件通知 ---- */
/* 获取 eventfd 文件描述符，可注册到调用方的 epoll/select 事件循环。
 * 当有新事件就绪时，该 fd 变为可读（EPOLLIN）。
 * 返回 >= 0 的 fd 表示成功，< 0 表示不支持（可降级为轮询模式）。
 * 注意：该 fd 由 AgentSight 内部管理，调用方不得 close()。 */
int agentsight_get_eventfd(AgentsightHandle* h);

/* ---- 数据读取 ---- */
/* 处理当前可用事件，通过回调返回数据。返回处理事件数，0=无事件，<0=出错。
 * 两个回调独立，传 NULL 表示不关心该类型。
 * flags: 0 = 非阻塞（处理当前队列后立即返回）
 *        AGENTSIGHT_READ_BLOCK = 阻塞直到有至少一个事件 */
#define AGENTSIGHT_READ_BLOCK 1
int agentsight_read(AgentsightHandle* h,
                    agentsight_https_callback_fn http_cb, void* http_ud,
                    agentsight_llm_callback_fn  llm_cb,  void* llm_ud,
                    int flags);

/* 与 agentsight_read 使用同一队列。HTTP/LLM 仍可触发 typed callback，所有事件还会
 * 触发 event_cb；调用方可只在 event_cb 中处理 SECURITY，平滑复用旧 LLM 映射。 */
#define AGENTSIGHT_HAS_READ_V2 1
int agentsight_read_v2(AgentsightHandle* h,
                       agentsight_https_callback_fn http_cb, void* http_ud,
                       agentsight_llm_callback_fn llm_cb, void* llm_ud,
                       agentsight_event_callback_fn event_cb, void* event_ud,
                       int flags);
```

### 2.1 返回值

| 函数 | 返回值 | 说明 |
| --- | --- | --- |
| `agentsight_config_new` | `AgentsightConfigHandle*` | 成功返回句柄，失败返回 NULL |
| `agentsight_new` | `AgentsightHandle*` | 成功返回句柄，失败返回 NULL（可用 `agentsight_last_error` 查看原因） |
| `agentsight_start` | `int` | 0=成功，<0=失败 |
| `agentsight_stop` | `int` | 0=成功，<0=失败 |
| `agentsight_get_eventfd` | `int` | >= 0 为有效 fd，< 0 表示不支持 eventfd |
| `agentsight_read` | `int` | \>0=处理的事件数，0=无事件，<0=出错 |
| `agentsight_read_v2` | `int` | 与 `agentsight_read` 相同；事件来自同一个有界队列 |
| `agentsight_last_error` | `const char*` | 错误描述字符串，无错误时返回 NULL |
| `agentsight_version` | `const char*` | 版本号字符串（如 `"0.2.2"`），静态存储，无需释放 |
| `agentsight_config_add_cmdline_rule` | `void` | cfg 或 rule 为 NULL 时静默忽略 |
| `agentsight_config_add_https` | `void` | cfg 或 rule 为 NULL 时静默忽略 |
| `agentsight_config_add_http` | `int` | 0=成功，<0=目标格式非法 |
| `agentsight_config_load_config` | `int` | 0=成功，<0=失败（解析错误） |

### 2.2 线程安全

* 同一 `AgentsightHandle` 不可多线程并发调用，所有 API（start/read/stop）须在同一线程执行
* 回调函数在调用 `agentsight_read()` 的线程上同步执行，无需额外同步
* 不同 `AgentsightHandle` 实例之间完全独立，可跨线程使用
* `agentsight_get_eventfd()` 返回的 fd 可安全地在其他线程中用于 epoll/select 等待

## 3. 配置

### 3.1 配置默认值

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `verbose` | 0 | 设为 1 开启调试日志输出 |
| `log_path` | NULL | 日志文件保存路径，NULL 时输出到 stderr |
| `cmdline_rules` | 空 | 用户自定义规则列表；allow=1 为进程白名单，allow=0 为进程黑名单 |
| `domain_rules` | 空 | 域名白名单规则列表，DNS 阶段独立判定是否 attach |
| raw HTTPS FFI 输出 | `false` | 仅通过 `agentsight_config_set_enable_raw_https` 显式开启 |
| security audit FFI 输出 | `false` | 通过 `agentsight_config_set_enable_security_audit` 显式开启 |
| enforcer socket | `/run/agentsight/enforcer.sock` | 可通过 `agentsight_config_set_enforcer_socket` 覆盖 |

### 3.2 系统安全审计订阅

开启后，AgentSight 在同一个 handle 内启动一个独立的轻量订阅线程，从 enforcer 获取归一化 `SecurityEvent`，并写入与 LLM/HTTPS 共用的 FFI 有界队列。enforcer 未启动或连接断开时订阅线程会重连；该失败不会停止主 AgentSight pipeline，也不会影响 LLM 事件。

`AGENTSIGHT_EVENT_TYPE_SECURITY` 的 `payload_json` 是 schema version 1 的完整 `SecurityEvent` JSON，覆盖 `file_action`、`taint_transition`、`network_action`、`policy_decision` 和 `enforcement_state`。FFI 不负责策略下发、阻断控制或持久化重放。

`AGENTSIGHT_EVENT_TYPE_HTTPS` 的 `payload_json` 是内部 `HttpRecord` 的字段（`pid`、`comm`、`method`、`path`、`status_code`、请求/响应头与 body、`duration_ns`、`is_sse` 等）平铺在顶层，另外并列三个进程归属字段 `agent_name` / `cmdline` / `container_id`，语义与 `AgentsightHttpsData` 中的同名字段一致。**取不到的字段直接省略，不会序列化为 `null`**，因此调用方判断「键是否存在」即可，无需处理 null。

`agent_name` 在 FFI 边界**统一小写**，`AGENTSIGHT_EVENT_TYPE_HTTPS` 与 `AGENTSIGHT_EVENT_TYPE_LLM` 两种 payload 一致（配置文件里写的是 `Hermes`，这里出来是 `hermes`）。因此调用方可以直接用 `agent_name` 把同一进程的 raw HTTPS 与 LLM 事件聚合到一起，无需自行做大小写归一。注意 AgentSight 自身的 Dashboard 与 SQLite 保留配置原始大小写，仅 FFI 输出做归一化。

### 3.3 Cmdline Rule 配置

通过 `agentsight_config_add_cmdline_rule()` 可添加用户自定义的进程匹配规则。`allow=1` 时添加进程白名单（匹配到的进程 attach SSL 探针）；`allow=0` 时添加进程黑名单（匹配到的进程不 attach）。

#### 函数签名

```c
void agentsight_config_add_cmdline_rule(
    AgentsightConfigHandle* cfg,
    const char* const* rule,
    const char* agent_name,
    int allow
);
```

#### 参数说明

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `cfg` | `AgentsightConfigHandle*` | 配置句柄，为 NULL 时静默忽略 |
| `rule` | `const char* const*` | NULL 结尾的 C 字符串指针数组 |
| `agent_name` | `const char*` | allow=1 时匹配成功使用的 agent 名称；allow=0 时忽略（传 NULL） |
| `allow` | `int` | 1=进程白名单（attach），0=进程黑名单（不 attach） |

#### allow=1：进程白名单

rule 为 cmdline glob 通配符数组，按位置一一对应做前缀匹配：

- **按位置一一对应（前缀匹配）**：`rule[i]` 对 `cmdline[i]` 做 glob 匹配
- **大小写不敏感**：所有 glob 匹配均忽略大小写
- **rule 比 cmdline 短**：忽略多余的 cmdline 元素（前缀匹配成功）
- **cmdline 比 rule 短**：不匹配（参数不够）
- **跳过不关心的位置**：用 `"*"` 作为通配，匹配该位置的任意值

#### allow=0：进程黑名单

rule 格式与 allow=1 相同（cmdline glob），匹配到的进程不 attach：

- **匹配方式与 allow=1 相同**：按位置一一对应做 glob 前缀匹配
- **优先级高于 allow=1**：同时匹配白名单和黑名单时，黑名单生效（不 attach）

#### 示例

```c
/* 匹配 Claude Code 进程 (allow=1) */
const char* pats[] = {"node", "*claude*", NULL};
agentsight_config_add_cmdline_rule(cfg, pats, "Claude Code", 1);

/* 匹配 Aider 进程 (allow=1) */
const char* pats2[] = {"*", "*aider*", NULL};
agentsight_config_add_cmdline_rule(cfg, pats2, "Aider", 1);

/* 进程黑名单 (allow=0)：不 attach webpack 相关 node 进程 */
const char* deny[] = {"node", "*webpack*", NULL};
agentsight_config_add_cmdline_rule(cfg, deny, NULL, 0);
```

### 3.4 HTTPS Rule 配置

通过 `agentsight_config_add_https()` 可配置域名白名单规则，用于 DNS 阶段判定是否 attach SSL 探针。

#### 设计动机

用户可能关心特定域名的流量（如 LLM API 域名），Domain Rule 提供域名级别的过滤能力：当 DNS 请求的域名命中白名单且进程不在黑名单时，attach SSL 探针。

#### 函数签名

```c
void agentsight_config_add_https(
    AgentsightConfigHandle* cfg,
    const char* rule
);
```

#### 参数说明

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `cfg` | `AgentsightConfigHandle*` | 配置句柄，为 NULL 时静默忽略 |
| `rule` | `const char*` | 域名 glob 模式（支持 `*`/`?`），为 NULL 时静默忽略 |

#### 行为语义

- **不调用**：DNS 阶段不会 attach SSL 探针（仅阶段一的 cmdline_allow 可触发 attach）
- **调用一次或多次**：域名必须命中任一 rule 才会 attach SSL 探针
- **多次调用叠加**：规则之间为 OR 关系，不覆盖已有规则

#### 多次调用叠加

- 多次调用不覆盖，规则持续累加
- 同类规则之间为 OR 关系，任一匹配即命中

#### 匹配规则

- **匹配对象**：HTTP 请求的目标域名（从 `Host` header 或 URL 中提取，不含端口号）
- **Glob 通配符**：支持 `*`（匹配任意字符序列）和 `?`（匹配单个字符）
- **大小写不敏感**：域名匹配忽略大小写
- **仅作用于 attach 判定**：域名规则只决定是否为进程 attach SSL 探针，不再过滤上报。启用 raw HTTPS FFI 输出后，进程一旦被 attach（无论是命中 cmdline 白名单还是域名规则），其无法解析为 LLM 语义的流量均产生 `AgentsightHttpsData`，不受域名白名单限制

#### 域名提取逻辑

```
request_url = "https://api.openai.com/v1/chat/completions"
                       ^^^^^^^^^^^^^^
                       提取此部分作为匹配目标

Host: api.anthropic.com:443
      ^^^^^^^^^^^^^^^^^^^
      去除端口号后匹配: "api.anthropic.com"
```

#### 示例

```c
AgentsightConfigHandle* cfg = agentsight_config_new();

/* Claude Code 进程白名单 */
const char* pats[] = {"node", "*claude*", NULL};
agentsight_config_add_cmdline_rule(cfg, pats, "Claude Code", 1);

/* 进程黑名单：不 attach webpack */
const char* deny[] = {"node", "*webpack*", NULL};
agentsight_config_add_cmdline_rule(cfg, deny, NULL, 0);

/* 域名白名单：仅 attach 这些域名的 SSL 连接 */
agentsight_config_add_https(cfg, "*.openai.com");
agentsight_config_add_https(cfg, "*.anthropic.com");

AgentsightHandle* h = agentsight_new(cfg);
agentsight_config_free(cfg);
agentsight_start(h);
```

上述配置效果：
- Claude 进程访问 `api.openai.com` → attach（阶段一 cmdline_allow 命中，阶段二 domain_rule 也命中）
- Claude 进程访问 `example.com` → attach（阶段一 cmdline_allow 命中即 attach）
- webpack 进程访问 `api.openai.com` → 不 attach（cmdline_deny 黑名单一票否决）
- 未知进程 DNS 解析 `api.openai.com` → attach（阶段二 domain_rule 命中，进程不在黑名单）
- 未知进程 DNS 解析 `example.com` → 不 attach（两阶段都未命中）

### 3.5 JSON 配置文件

除了通过 C API 逐条配置，也可通过 JSON 字符串一次性加载所有规则。

#### C API

```c
/* 从 JSON 字符串加载配置，追加到已有规则中。
 * 返回 0=成功，<0=失败（解析错误，可用 agentsight_last_error() 查看）。 */
int agentsight_config_load_config(AgentsightConfigHandle* cfg, const char* json_str);
```

#### 文件格式

```json
{
  "verbose": 1,
  "log_path": "/var/log/agentsight.log",
  "cmdline": {
    "allow": [
      { "rule": ["node", "*claude*"], "agent_name": "Claude Code" },
      { "rule": ["*", "*aider*"], "agent_name": "Aider" },
      { "rule": ["python3", "*my_agent*"], "agent_name": "My Agent" }
    ],
    "deny": [
      { "rule": ["node", "*webpack*"] },
      { "rule": ["python3", "*celery*"] }
    ]
  },
  "domain": [
    { "rule": ["*.openai.com", "*.anthropic.com"] },
    { "rule": ["*.deepseek.com", "generativelanguage.googleapis.com"] }
  ]
}
```

#### 字段说明

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `verbose` | int (可选) | 1=开启调试日志，0=关闭，默认 0 |
| `log_path` | string (可选) | 日志文件路径，省略时输出到 stderr |
| `cmdline.allow[].rule` | string array | cmdline glob 数组，按位置一一匹配 |
| `cmdline.allow[].agent_name` | string | 匹配成功时的 agent 名称 |
| `cmdline.deny[].rule` | string array | cmdline glob 数组，匹配到的进程不 attach |
| `domain[].rule` | string array | 域名白名单 glob 数组，DNS 命中即 attach |

#### 加载行为

- `agentsight_config_load_config()` 将 JSON 字符串中的规则**追加**到已有配置，不清空之前通过 C API 添加的规则
- 可多次调用，规则持续累加
- 解析失败时返回 `<0`，不影响已有配置

#### 使用示例

```c
AgentsightConfigHandle* cfg = agentsight_config_new();
agentsight_config_set_verbose(cfg, 1);

/* 从 JSON 字符串加载配置 */
const char* json =
    "{\"cmdline\":{\"allow\":[{\"rule\":[\"node\",\"*claude*\"],"
    "\"agent_name\":\"Claude Code\"}]},"
    "\"domain\":[{\"rule\":[\"*.openai.com\",\"*.anthropic.com\"]}]}";

if (agentsight_config_load_config(cfg, json) < 0) {
    fprintf(stderr, "load config failed: %s\n", agentsight_last_error());
}

/* 也可继续通过 API 追加规则 */
agentsight_config_add_https(cfg, "*.my-custom-llm.com");

AgentsightHandle* h = agentsight_new(cfg);
agentsight_config_free(cfg);
agentsight_start(h);
```

### 3.6 匹配判定逻辑

匹配分为两个独立阶段，均用于判定是否 attach SSL 探针：

#### 阶段一：进程创建时

当新进程创建时，仅根据 cmdline 判断是否 attach SSL 探针：

```
attach_ssl = cmdline_allow匹配(进程)
```

- 若进程命中 cmdline_allow，attach SSL 探针
- 此阶段不涉及域名判断（域名信息尚不存在）

#### 阶段二：DNS 事件到达时

当 DNS 事件到达时，判定是否 attach SSL 探针：

```
attach_ssl = domain_rule匹配(域名) AND NOT cmdline_deny匹配(进程)
```

流程图：

```
DNS 事件到达
  │
  ├─ 进程命中 cmdline_deny 黑名单？── 是 ──→ ❌ 不 attach
  │
  └─ 否
      │
      ├─ 域名命中 domain_rule？──── 是 ──→ ✅ attach SSL 探针
      │
      └─ 否 ────────────────────────────→ ❌ 不 attach
```

关键语义：
- **两阶段独立**：阶段一和阶段二分别独立判定，任一阶段命中即 attach
- **阶段一只看 cmdline**：cmdline_allow 命中即 attach，不需要等到 DNS 事件
- **阶段二只看 domain 和黑名单**：domain_rule 命中即 attach
- **cmdline_deny 两阶段都生效**：黑名单一票否决
- **都不配置**：无事件输出

### 3.7 Raw HTTPS FFI 输出

raw HTTPS fallback 默认关闭。需要接收 `AgentsightHttpsData` 的 FFI 调用方必须在
`agentsight_new()` 前显式开启：

```c
void agentsight_config_set_enable_raw_https(
    AgentsightConfigHandle* cfg,
    int enabled
);
```

- `enabled == 0`：不可解析为 LLM 语义的 HTTPS 流量不会构造或进入 FFI 事件队列。
- `enabled != 0`：不可解析流量通过 HTTPS callback 上报，不再受域名规则过滤。
- 配置句柄为 NULL 时静默忽略。
- 此开关仅影响 FFI raw HTTPS 输出，不改变 standalone binary 的分析和存储行为。

## 4. 使用示例

完整示例程序见 `tools/examples/agentsight/agentsight_example.c`。

### 4.1 eventfd + epoll 模式（推荐）

```c
/* --- 初始化阶段 --- */
AgentsightConfigHandle* cfg = agentsight_config_new();
agentsight_config_set_verbose(cfg, 1);

AgentsightHandle* h = agentsight_new(cfg);
agentsight_config_free(cfg);

if (!h) {
    fprintf(stderr, "agentsight_new failed: %s\n", agentsight_last_error());
    return -1;
}

agentsight_start(h);

/* 获取 eventfd，注册到统一 epoll */
int as_efd = agentsight_get_eventfd(h);
if (as_efd < 0) {
    fprintf(stderr, "eventfd not supported, fallback to polling\n");
    /* 降级到轮询模式，见 3.2 */
}

int epoll_fd = epoll_create1(0);
struct epoll_event ev = {
    .events   = EPOLLIN,
    .data.fd  = as_efd,
};
epoll_ctl(epoll_fd, EPOLL_CTL_ADD, as_efd, &ev);

/* --- 事件循环 --- */
while (running) {
    struct epoll_event events[64];
    int n = epoll_wait(epoll_fd, events, 64, 500 /* ms */);

    for (int i = 0; i < n; i++) {
        if (events[i].data.fd == as_efd) {
            agentsight_read(h, on_https_event, NULL,
                               on_llm_event,   NULL,
                               0 /* non-blocking */);
        }
    }
}

/* --- 清理阶段 --- */
epoll_ctl(epoll_fd, EPOLL_CTL_DEL, as_efd, NULL);
agentsight_stop(h);
agentsight_free(h);  /* 内部 close(as_efd)，调用方不得重复 close */
close(epoll_fd);
```

### 4.2 轮询模式（降级 / 简单场景）

```c
AgentsightConfigHandle* cfg = agentsight_config_new();
AgentsightHandle* h = agentsight_new(cfg);
agentsight_config_free(cfg);

if (!h) {
    fprintf(stderr, "agentsight_new failed: %s\n", agentsight_last_error());
    return -1;
}

agentsight_start(h);

while (running) {
    agentsight_read(h, my_http_cb, http_ctx,
                       my_llm_cb,  llm_ctx,
                       0 /* non-blocking */);
    usleep(100000);  // 100ms 轮询间隔
}

agentsight_stop(h);
agentsight_free(h);
```

## 5. 内存规则

* 回调中的指针仅在回调执行期间有效，调用方需自行拷贝
* `agentsight_new()` 内部拷贝配置，不消费 config handle，调用者须自行 `agentsight_config_free(cfg)`
* 同一 config handle 可复用于创建多个 `AgentsightHandle` 实例
* `agentsight_free()` 须在 `agentsight_stop()` 之后调用
* `agentsight_get_eventfd()` 返回的 fd 由 `agentsight_free()` 内部关闭，调用方**不得**自行 `close()`

## 6. HttpsData 与 LLMData 的关系

一条被捕获的 HTTPS 流量最多产生一种 FFI 数据：若被识别为 LLM API 调用，则产生 `AgentsightLLMData`；否则仅在 raw HTTPS FFI 输出开启时产生 `AgentsightHttpsData`。两者互斥，不会同时产生，无需关联。

## 7. 编译与链接

### 7.1 从源码构建（CMake 集成）

AgentSight 已集成到 coolbpf 的 CMake 构建系统中，通过 `ENABLE_AGENTSIGHT` 选项控制：

```bash
# 构建 libagentsight（不含 server/Dashboard，无需 Node.js）
mkdir -p build && cd build
cmake -DENABLE_AGENTSIGHT=on ..
make libagentsight

# 同时构建 C 示例程序
cmake -DENABLE_AGENTSIGHT=on -DBUILD_EXAMPLE=on ..
make agentsight_example

# 安装
make install
```

CMake 选项说明：

| 选项 | 默认值 | 说明 |
| --- | --- | --- |
| `ENABLE_AGENTSIGHT` | OFF | 构建AgentSight FFI 库（`libagentsight.so` + `agentsight.h`） |

构建产物：

| 文件 | 安装路径 | 说明 |
| --- | --- | --- |
| `libagentsight.so` | `${prefix}/lib/` | C FFI 共享库 |
| `agentsight.h` | `${prefix}/include/` | C 头文件（cbindgen 自动生成） |

### 7.2 链接

```bash
gcc -I/usr/local/include -L/usr/local/lib -lagentsight -o myapp myapp.c
```

### 7.3 独立构建（含 Dashboard）

如需构建完整的 AgentSight（含嵌入式 Web Dashboard），使用 `src/agentsight/Makefile`：

```bash
cd src/agentsight
make build-all          # 构建前端 + Rust 二进制
make install            # 安装 agentsight CLI
```

## 8. 变更记录

| 版本 | 变更 |
| --- | --- |
| v0.1 | 初始版本，轮询 read 模式 |
| v0.2 | 升级为 eventfd + read 模式；新增 `agentsight_get_eventfd()`；`agentsight_read()` 增加 `flags` 参数；新增 `agentsight_config_set_log_path()`；大 buffer 指针增加 `_len` 字段；新增 `llm_usage` 字段区分 token 数据来源 |
| v0.2.1 | 集成 CMake 构建系统（`ENABLE_AGENTSIGHT` 选项）；新增 C 示例程序 `tools/examples/agentsight/`；新增 `cbindgen.toml` 自动生成完整 C 头文件；新增 FFI API 文档 |
| v0.3 | `agentsight_config_add_cmdline_rule()` 新增 `allow` 参数：allow=1 为进程白名单，allow=0 为进程黑名单 |
| v0.4 | 新增 `agentsight_config_add_domain_rule()` 接口，支持域名白名单；新增 `agentsight_config_load_config()` 支持 JSON 字符串加载配置 |
| dev | 新增 `agentsight_read_v2()` 版本化通用事件 envelope；可选订阅归一化系统安全审计事件，并与 HTTPS/LLM 共用 handle、队列和 eventfd |
| dev | `AgentsightHttpsData` 尾部新增 `cmdline` / `agent_name` / `container_id`（追加不改动既有字段偏移）；`AGENTSIGHT_EVENT_TYPE_HTTPS` 的 `payload_json` 同步带上这三个字段 |
