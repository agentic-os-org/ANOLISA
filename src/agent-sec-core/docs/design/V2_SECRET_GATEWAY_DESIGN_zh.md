# V2 Secret Gateway 设计

| 属性 | 值 |
| --- | --- |
| 状态 | [TARGET V2] |
| 范围 | GitHub 凭据；复用原生 Policy/Scope/Binding 与 process discovery |
| 相关文档 | [部署契约 §14](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md#14-target-v2-secret-gateway-进程权限与数据目录dproc-gw)、[daemon 协议 §18](DAEMON_PROTOCOL_V1_zh.md#18-target-v2-secret-gateway-管理) |

本文面向维护 Policy 契约与网关实现的开发者，定义布尔响应开关、未压缩正文的有界流式
替换和压缩正文透传边界。发布支持以对应版本的实际制品为准，源码行为不代表已发布能力。

## 1. 目标与授权对象

Secret Gateway 让 Agent 只持有与真实 GitHub Token 同形的 fake，在 Policy 允许的 HTTPS
请求 Authorization 中把 fake 换成真实值；响应是否反向替换由 `responseRewrite` 控制。
设计前提是 Agent 无法从环境变量、文件、工具或其他渠道取得真实凭据。响应改写不是完整
防泄漏保证：关闭改写或上游返回压缩正文时，真实 key 仍可能回到 Agent（见 §4.3），因此
不能无条件声称 Agent 始终只会看到 fake。这也不是一般 DLP：请求正文不检查或改写，若
Agent 已取得真实凭据，本设计不保证阻止其通过正文外泄。

授权由 PAP 中的 Policy、Scope 和 Binding 负责：Policy 描述允许的请求，Scope 固定 Policy
revision 与 owner UID 并持续选择进程，Binding 固定一次发现的进程实例。Credential Store 保存
真实值与 Scope fake；Gateway Runtime 保存执行投影与清理责任，不成为第二套授权配置。
key 导入不授权，执行投影不能在 canonical 对象失效后独立恢复权限。

本版本仅支持 GitHub 凭据元数据和静态 HTTPS allow 规则；不提供 DLP、审计导出、OAuth 或
其他 provider 的启动凭据映射。

| 组件 | 位置 | 职责 |
| --- | --- | --- |
| Policy Types | `v2/crates/asc-policy-types` | 原生 `network/use_credential/http_request` 与 Scope owner |
| PAP / PCP | `v2/crates/asc-pap`、`asc-pcp` | 固定 assignment 快照、discovery、Binding reconciliation 与删除 |
| Credential Store | `v2/crates/asc-credential-store` | root 真实值、稳定 Scope fake、永久 tombstone、网关快照 |
| Gateway Plan | `v2/crates/asc-gateway-plan` | 从 Scope 的凭据 Policy 快照编译确定性 plan |
| Gateway Runtime | `v2/crates/asc-gateway-runtime` | Scope 准备/撤销、Binding 实例接管、generation、systemd 与 nft |
| 数据面 addon | `v2/gateway/addon/` | mitmproxy 请求判定、替换与响应检查 |
| CLI | `v2/apps/asc-cli/src/commands/` | root key 管理、既有 Policy/Scope 操作、owner fake/CA 查询 |

## 2. 原生 Policy 与 Scope

### 2.1 Policy authoring

Policy 使用既有 `specVersion: "0.1"`、`rules` JSON，不增加独立授权语言。凭据规则为：

```json
{
  "specVersion": "0.1",
  "rules": [{
    "effect": "allow",
    "category": "network",
    "action": "use_credential",
    "target": {
      "type": "http_request",
      "credentialId": "github",
      "scheme": "https",
      "host": "api.github.com",
      "path": "/**",
      "methods": ["GET", "HEAD"],
      "responseRewrite": true
    }
  }]
}
```

- 凭据 Policy 只能含 `allow/network/use_credential`，不支持 `where`、`previous`、确认或 file
  规则；单 Policy 混合必须拒绝。不同 file/credential Policy 可在同一个 Scope 中分别执行。
- `credentialId` 是 store 标识，不是 Token；GitHub 标识为 `github`。
- host 为精确小写 DNS 名，最长 253 字节；不支持通配 host。
- path 最长 512 字节、必须绝对；`*` 为一段，`**` 跨多段，其余为字面段，不允许 `.`、`..`。
- methods 是 1–6 个不重复的大写 `GET/HEAD/POST/PUT/PATCH/DELETE`。
- `responseRewrite` 必填，且只接受 JSON bool `true` / `false`；不接受省略、null、字符串
  `"true"` / `"false"`、数字或旧 `buffered` / `none`，不提供默认值或兼容别名。
  `true` 重写注入后响应的 headers、未压缩正文和 trailers；`false` 全部不重写（详见 §4.3）。
- 不提供 `auto` / `stream` 等技术配置；可选 `because` 最长 256 字节且不含控制字符。

标准 GitHub Policy 的完整 JSON 位于 `v2/crates/asc-gateway-plan/policies/github.json`；RPM 将其安装为 `/usr/share/anolisa/sec-core/gateway/github-policy.json`。各规则的布尔开关如下：

| host | path | method | responseRewrite |
| --- | --- | --- | --- |
| `api.github.com` | `/**` | GET、HEAD、POST、PUT、PATCH、DELETE | `true` |
| `github.com` | `/**/info/refs` | GET | `true` |
| `github.com` | `/**/git-upload-pack` | POST | `false` |
| `github.com` | `/**/git-receive-pack` | POST | `true` |

上传接口、raw 内容、GitHub Enterprise 不在标准 Policy 中。原 `buffered` 项改为 `true`，
不再整包缓存；Git upload-pack 原 `none` 改为 `false`，headers、body、trailers 均不重写。
旧 `none` 仍替换 headers/trailers，因此这一映射改变行为，并非兼容迁移。字节替换不是
Git 或通用二进制协议解析；带 hash、签名或 framing 的内容即使长度不变，也不保证替换后
协议正确。标准 Policy 的选择不构成任意二进制响应可安全改写的保证。

### 2.2 Scope 准备与发现

root 通过既有 `scope create` 指定精确 Policy ID/revision、owner UID，以及 `--process-name`
或 `--executable` 持续 selector。kernel comm 限制为 15 字节；executable 为规范化绝对路径。
凭据 Scope 必须有非 root owner；普通 file Scope 的权限、selector 和快照行为保持不变。

创建 Scope 固定完整 Policy 快照，校验 owner 与网关秘密读取权限，预留端口并签发稳定 fake。
没有进程时也可以准备 plan。启动顺序为：导入 key → 创建 Policy → 创建 Scope → enable →
owner 查询 fake/CA → Agent 自身加载配置 → 自主启动。未满足 §4.1 的网关 ready 条件时查询
必须明确失败，不向客户端交付部分配置。

PCP process discovery 约两秒运行一次。每个匹配进程实例通过既有 Binding 接管，核对 boot ID、
PID namespace、PID、start time、UID 和组；目标及已有后代快照逐个核验后移入实例 cgroup。
新后代继承 cgroup。仅在 Binding 接管和引流完成后保护新连接，不承诺 first packet；已建立
连接和接管期间在未迁移进程中并发产生的后代不受追溯保护。失败迁移可能保留部分已移动
进程，清理责任不能丢弃。

同一 Scope 的多实例共享 fake、端口与 Policy plan，各实例保留独立 Binding。
不重叠进程树使用独立执行 cgroup；同 Scope 的父子重叠树复用已拥有的路径，不迁移已经
受保护的进程。每条 Binding 持久保存根路径及覆盖的共享路径引用，先保存责任再接入未受
保护的成员；重试只增加经过核验的引用，不丢弃已有责任。所有路径仍核验完整进程身份，
拒绝跨 Scope 或无拥有者的网关 cgroup。先删父或先删子都不会撤除其它 Binding 仍引用的
路径；实例退出只撤除对应执行责任，不撤销共享 fake。
匹配实例重启由 discovery 重新接管，fake
保持稳定。同 UID 多 Scope 有不同 fake，但同一用户仍可读取彼此 fake；cgroup 不是 UID
内部的安全隔离边界。

## 3. Credential Store

### 3.1 持久文件与值

- `<数据目录>/credentials` 必须 root 拥有且为 0700，`store.json` 为 0600，上限 8 MiB；逐级
  打开路径，拒绝符号链接。目录 advisory lock 保证单写者，打开时删除崩溃遗留 `.tmp-*`。
- 修改在副本上校验并原子替换；文件未替换时磁盘与内存保持原状，已替换但持久性未确认时
  内存跟随磁盘，避免撤销后继续下发旧值。
- 真实值为 1–512 字节可见 ASCII；不实现 `Clone/Serialize/Display`，Debug 不含真实值，比较
  不在第一个不同字节提前退出。凭据 ID 为有界小写 DNS label，Scope fake 以 canonical Scope ID
  归属，不以 PID 或进程名归属。
- GitHub carrier 为 `header_token`（Authorization Bearer/token）与 `basic_password`（Basic 密码）。
  已知前缀为 `ghp_`、`gho_`、`ghu_`、`ghs_`、`ghr_`、`github_pat_`。
- fake 保留最长已知前缀、长度与标点，以均匀随机同类字符替换其余字母/数字。前缀之后少于
  16 个字母/数字则拒绝；候选与任何真实值/fake 相等时重新生成，最多八次。

### 3.2 生命周期

每个（credential，Scope）只有一个活跃 fake，重复签发返回相同值。`key add` 对相同 secret
幂等成功；已有凭据的不同值必须报错，不覆盖真实值。更换 key 必须先 remove 再 add，
不自动轮换。Scope 删除先关闭 canonical admission，再把该 Scope 的 fake 退役；不删除
共享真实值。空 Scope 也必须释放准备责任和端口，不能依赖最后一个 Binding 的回调。

全局 `key remove` 删除真实值并退役所有相关 fake，不影响其他凭据。重新导入不复活旧 fake；
使用更换后的 key 必须创建新 Scope，并将新 fake 加载到 Agent 配置中。
tombstone 永久保留，防止旧 fake 成为“未知值”而被默认放行；含 tombstone 的 fake 总数最多
4096，每 Scope 最多 64 个活跃 fake。加载和写入必须校验真实值/fake 全局不冲突、fake 唯一、
退役凭据无活跃 fake、同 credential/Scope 不重复，以及 carrier 非空。

### 3.3 网关快照

仅选中凭据的活跃 fake→真实值映射和所有 tombstone 被下发；未选中的真实值不离开存储目录。
快照持有 generation，归属使用 Scope ID。目标目录归 root 且 group/other 不可写，快照 0640、
网关属组、原子替换。`key list` 只返回 active 凭据 ID：`{"credentials":["github"]}`。
`key add/remove` 成功仅返回 `{"credential":"github"}`。管理结果不改变既有凭据文件的权限
与快照保护边界。

## 4. Plan 与数据面

plan 从 Scope 的 canonical pinned Policy 快照编译，忽略独立 file Policy。Policy ID 排序、
methods 确定性排序；规则记录来源 Policy ID/revision/ruleIndex。plan 不含真实值或 fake，二者
通过单独快照解析。作者 Policy JSON 保持 §2.1 的既有形状，包括 target 中的 `scheme` 和
`responseRewrite`；plan 是该快照的执行投影，不是另一种 authoring 格式。

plan 的完整字段形状如下，示例只包含一条 HTTPS 规则：

```json
{
  "format": "agent-sec-gateway-plan/v1",
  "scopeId": "00000000-0000-4000-8000-000000000001",
  "policies": [{"id": "policy:github", "revision": 7}],
  "credentials": ["github"],
  "rules": [{
    "source": {"policyId": "policy:github", "policyRevision": 7, "ruleIndex": 0},
    "match": {
      "scheme": "https",
      "host": "api.github.com",
      "path": "/**",
      "methods": ["GET", "HEAD"],
      "credentialId": "github"
    },
    "responseRewrite": true,
    "because": "GitHub REST API with github"
  }]
}
```

`scopeId` 直接标识 Scope，`policies` 列出固定 revision，`credentials` 是凭据 ID 数组。
每条规则仅有 `source`、`match`、`responseRewrite`、`because`；`source.ruleIndex` 是作者
Policy 中的规则位置，数据面判定使用 rules 数组位置，不额外保存 index。`match.methods`
为方法数组，`match.credentialId` 为单个凭据 ID。Plan 的 `responseRewrite` 同样必填且严格为
JSON bool，由 pinned Policy 原样投影；Rust 与 addon 均拒绝旧字符串、缺失或其他类型。
示例保留 `agent-sec-gateway-plan/v1` 标签，但标签不代表与旧字符串格式兼容；旧 Plan 必须
从已转换或重建的 canonical 快照重新生成，不能绕过 §5.1 的持久化兼容边界。

携带本 Scope fake 的请求必须命中同时点名该凭据的规则，未授权固定 block；无托管凭据
固定 allow，但仍执行安全校验。这两项是数据面语义，不是 plan 配置字段。plan 上限为
256 条规则、64 个凭据；Rust 与 Python 两侧拒绝未知字段和重复键。

### 4.1 启动与 generation

独立 `agent-sec-gateway.service` 以无特权 `agent-sec-gw` 运行 pinned mitmproxy standalone 12.2.3，
各架构 SHA-256 见 `v2/gateway/mitmproxy.provenance`。禁止 `ssl_insecure`、`stream_large_bodies`、
`save_stream_file`、非零 `flow_detail`；必须设置 runtime dir，防止绕过证书或记录秘密。

addon 每 0.5 秒检查 active manifest，整体加载 generation、快照与 listener plan；使用
`O_NOFOLLOW`，只接受普通文件。manifest、plan、快照上限分别为 64 KiB、1 MiB、8 MiB。
listener 端口与 Scope 唯一，plan 的 `scopeId` 必须与 listener 相同，所引用凭据必须有
所属 Scope fake。加载失败保留旧 generation，相同失败五秒内不重试；manifest 删除则清空
状态，后续请求拒绝。

addon 原子写 `status/health.json` 的 `loadedGeneration/listeners/lastAttempt/caError`，并提供
`status/ca-cert.pem`。unit 启动前和停止后删除 health，防止旧进程报告充当新进程 readiness。
这些是内部 readiness 输入。公开 `gateway status` 仅为 `{enabled, ready, scopes}`，Scope 项为
`{scopeId, bindings}`；`ready` 要求 enabled、cgroup v2 检查通过、unit active、loaded generation
与当前 generation 匹配、CA 可用。已启用时每次 status 都重新只读检查 cgroup，失败返回
`ready=false`，不改变状态；`credentials` 也重新检查，仅返回 `{scopeId, environment}`，
未就绪明确失败。generation 与 revoked/tombstone 保留在内部发布、恢复和撤销逻辑中，
不作为管理查询的展示字段。`enable/disable` 成功返回 `{}`。网关 ready 不替代 Binding `READY`
及引流完成的等待。

enable 在改变 enabled 前检查 Linux `/sys/fs/cgroup` 的 `statfs` 类型为
`CGROUP2_SUPER_MAGIC`，且 daemon 自身 `/proc/self/cgroup` 视图存在 `0::` 条目；
证据不可读或非 v2 时失败。每次 apply（含已启用状态恢复）在发布前重新检查。默认关闭恢复
不运行此预检，不会仅因 cgroup v2 不可用而失败；不自动降级为 v1、不修改挂载。
此检查不证明完整内核/nft 能力：不独立预检 nft `socket cgroupv2`，仍由实际规则应用检验，
不支持时应用失败。

### 4.2 请求判定与响应

请求在 `requestheaders` 阶段完成授权与 Authorization fake→真实值注入，不等待正文；注入
独立于 `responseRewrite`，`true` / `false` 均须经过相同授权与请求安全校验。请求正文原样
流式传输，不解压、不缓存、不检查或改写。URL（含 query 的原始
及 URL 解码形式）、header 值以及解析后的 Basic 用户名/密码中既有真实凭据拒绝仍保留。
query 不作为凭据替换 carrier，Policy path 匹配不含 query。

请求校验顺序保持：generation/listener → Authorization 重复与 Basic 格式 → URL/header 中
真实值泄露 → fake 的 carrier、Scope 与 tombstone → 明文 → Host/SNI/authority → Upgrade →
HTTPS 端口与规范 path、method、Policy allow。常见拒绝码为 `gateway_not_ready`、`duplicate_authorization`、
`malformed_authorization`、`real_credential_exposed`、`fake_in_username`、`retired_credential`、
`subject_mismatch`、`carrier_not_allowed`、`plaintext_credential`、`invalid_host`、
`sni_host_mismatch`、`authority_mismatch`、`sni_missing`、`upgrade_not_allowed`、
`credential_not_allowed`。不携带托管凭据仍需通过有关校验，不是 TLS 透传。

在请求头阶段拒绝有正文的 HTTP/1 请求或 HTTP/2 请求时，立即关闭连接，不继续接收或
缓冲正文，不承诺结构化 HTTP 拒绝响应；HTTP/1 空正文请求保留
`{"error":"<code>","source":"agent-sec-gateway"}`，通常 403；
SNI/Host 不符为 421，内部异常 `gateway_error` 为 500。输出日志只允许 addon 的固定安全诊断，
不输出请求字段、依赖解析消息或异常堆栈；内部 readiness 报告保留，底层故障日志细节减少。

命中 allow 后只替换 Authorization token/Basic 密码，保留 Basic 用户名；不因响应开关关闭
而跳过注入，也不修改请求 `Accept-Encoding`，压缩协商由客户端与上游决定。

### 4.3 响应改写

只对实际注入凭据的请求应用 `responseRewrite`。替换表将实际注入的 Authorization
（含 Basic 编码）映射回原 fake Authorization，将裸真实值映射回 Scope fake；不提供
任意编码或语义还原。未注入的响应保持普通流式转发。

| responseRewrite | headers | body | trailers |
| --- | --- | --- | --- |
| `true` | 按替换表 real→fake | 未压缩正文有界流式替换；其他编码原样流式透传 | 按替换表 real→fake |
| `false` | 不重写 | 原样流式透传 | 不重写 |

`false` 关闭全部响应凭据改写，也不因该开关剥离校验头或长度头；它不等价于仍替换
headers/trailers 的旧 `none`。正常 HTTP 传输与错误处理不因此停用。

开启改写时，`Content-Encoding` 缺失、空值或 `identity` 的正文按连续字节流替换，不依赖
network chunk 边界。每次输出已能确定不会参与后续匹配的前缀，仅保留可能构成替换源前缀
的最小未决尾部；保留量受替换源长度约束，不随整篇正文增长。真实 key 或完整注入的
Authorization 横跨 chunk 时仍应替换，真正 EOF 才处理最后尾部；非终止空 DATA 不是 EOF。
没有整包缓存，也没有累计 encoded/decoded 正文的 8 MiB 限额。§3.1、§4.1 中的持久文件与
快照容量限制不是正文限额。

匹配器的尾部有界不等于底层传输队列有界：固定 mitmproxy 在下游 HTTP/2 窗口不足时
继续接收上游数据并排队。网关处理已注入响应 DATA 时，将客户端连接的待发送正文队列
限制为 8 MiB 和 1024 个待发 DATA 项，统计时先限制项数，避免微小分块导致无界扫描。
超限清除排队正文/trailers 并关闭两端连接，不按累计响应大小终止正常读取。
该资源保护同时作用于开启、关闭及压缩透传的已注入响应，同一客户端 HTTP/2 连接的其他
流也可能断开。它不是用户需要选择的重写模式，不承诺未注入流或所有进程内存受此上限约束。

为避免解压、重新压缩的处理成本与编码变换复杂度，任何其他 `Content-Encoding`，包括
`gzip`、`br`、`deflate`，其正文原样透传，不解压、不扫描、不重新压缩，也不因为编码
不受替换支持而拒绝；headers/trailers 仍按 `true` 替换。
**明确 tradeoff：即使开启改写，压缩 body 仍可能把真实 key 返回 Agent。只替换
headers/trailers 无法弥补正文泄漏；不能把压缩透传称为完整防泄漏保证。** 不强制上游返回
identity，也不改客户端 `Accept-Encoding` 协商。

SSE 可以按普通字节流处理：未压缩时替换物理字节连续的匹配，包括跨 network chunk 的
匹配；不解析 SSE 事件或 JSON，不识别多个 JSON 事件在消费端语义拼接出的 key。压缩 SSE
同样适用压缩正文透传边界。带 hash、签名或 framing 的二进制正文没有通用替换正确性保证。

未压缩正文改写仍须保持 HTTP 长度/framing 一致：有正文时移除原 Content-Length、ETag
和 Content-MD5，HTTP/1 使用 chunked，HTTP/2 使用原生 DATA；压缩正文不按解码结果改写
长度或校验值。HEAD、204、304 等无正文响应保持无正文，保留表示长度，不添加 chunked
framing。固定依赖不支持 HTTP/1 trailers，遇到此类协议错误仍中止，不能承诺成功转发。

响应改写或传输失败时中止流/连接，不退回未检查的响应，也不发送包含原始上游字节的
代理错误页。但 headers 和安全前缀可以已经发送，**已发响应不可撤回**，不能沿用旧完整
缓存路径的“不交付任何部分正文”保证。已注入请求的上游响应头解析、字段校验等协议错误
保持关闭连接边界；同步关闭底层 writer 的行为依赖固定 mitmproxy 版本，HTTP/2 同连接
其它流也可能断开。上传未结束而上游先结束响应时，不等待请求 EOF，应完成响应收尾并
停止上传；共享上游连接的其它流可能受影响。依赖升级时须通过内部 mock 黑盒重新验证
这些连接边界。

## 5. 执行投影、发布与清理

`<数据目录>/gateway/state.json` 只保存 enabled、generation 与 Scope 执行投影：owner UID、端口、
撤销 fence、Binding 责任与完整实例身份。它不保存另一套授权规则。恢复必须从 PAP 读取
canonical Scope、固定的 Policy revision 和 Binding，核对 lifecycle 与实例身份；不从 PID
单独恢复、不读取 latest Policy 替代快照。version 2 的 Binding 投影增加 `sharedCgroups`，
缺失时按仅有根路径读取。旧版本不认识此字段，不能直接回退二进制后沿用新投影；回退前
必须关闭网关并撤流，canonical Scope/Binding 和 Credential Store 不属于该投影的清理范围。

当前 daemon 在启动 Binding worker 前恢复 file Scope 的删除屏障，并在 blocking task 中执行
网关构造与 `GatewayService.recover()`；信号监听先启动，恢复期间收到停止请求后不开放 UDS。
开放 UDS 请求前再执行 `PapService.recover_scopes()`，不设置独立网关恢复 Job。SQLite 在 claim Apply 的同一事务中核对 canonical Scope 仍为 `Active`，
防止删除中的 Scope 在 discovery 清理前重新部署策略。网关投影不可读、损坏或版本不支持时，
构造失败先停止代理、撤除 nft 表并清除运行时快照，再返回错误；不依赖损坏投影判断是否撤流。
投影文件缺失而运行目录、活动代理或 nft 表仍存在时，同样先撤旧数据面；Credential Store 保留，
不从代理缓存重建授权。无法检查 nft 表也按故障撤流；任一清理失败均显式报告，且仍尝试其它清理动作。
宿主命令最长运行 5 秒，输出分别限制 64 KiB，超限终止进程组并回收子进程。
调度层最多允许一个凭据 Binding worker 等待网关控制操作，其余凭据 Binding 延后 admission，
不消耗部署或调度重试预算；文件 Binding 绕过该屏障，保留原有重试语义。
网关构造或恢复失败单独记录错误；Active 凭据 Scope 恢复失败保留 `Active`，不转入删除，
不阻塞原有 file-policy daemon readiness。
file Scope 恢复或 PAP 存储扫描失败仍阻止启动。
管理员排除故障后可用 `scope retry --scope-id <scopeId>` 重新 validate/prepare 并启动 discovery。

每次下发先持久化新的 generation，编号不复用；校验 canonical Scope 与账户隔离，写快照和
plan，最后发布 manifest，启动 unit，等待 loadedGeneration 匹配（最长 20 秒），拼接系统 CA
与网关 CA，再刷新 nft 并删除旧 generation。await 后重新验证 canonical 责任，不能使已删除
Scope 的过时请求恢复授权。准备与查询不需要进程存在，但查询需要真实 readiness。

运行目录 `/run/agent-sec-gateway` 为 root:agent-sec-gw 0751，manifest/plan/secrets 为 0640，
`status/` 为网关账户拥有的 0755，公开 `ca-bundle.pem` 为 0644。系统 CA 取首个存在的
`/etc/pki/tls/certs/ca-bundle.crt` 或 `/etc/ssl/certs/ca-certificates.crt`；网关 CA 必须由网关
账户拥有、单硬链接、有效 PEM。daemon 将网关写入文件视为不可信有界输入。

nft 独占 `inet agent_sec_gateway`，按实例 cgroup 路径分类，同 Scope 实例跳转到共享端口。
仅已接管且 canonical 有效的实例被引流；cgroup 重建必须刷新 inode 解析。回环不引流；
IPv4 TCP 80/443 引流，IPv6、UDP/443 与其他端口放行但不保护、不强制回退。已有 TCP
连接不受追溯保护。所有被引流 HTTPS 均 MITM，即使不含 fake 也要求信任 CA。

Scope 删除先永久关闭 admission 并撤销 fake，再清理 Binding 和执行投影；清理失败保留责任
以便重试。Policy Runtime 拥有单一 Scope 清理 worker，调用 `sweep_deleting_scopes()`，
在 Binding 清理完成后执行 Deleting Scope 的 final release，包括零实例 Scope；shutdown
等待其实际调用结束。网关发布等待不占用 Binding timer，file-only Scope 和 credential Scope
使用独立变更锁，混合 Scope 归 credential 域；不另设 gateway 恢复任务。
实例退出不退役共享 fake。全局 key retirement 对所有 Scope 生效。撤销不能确认
时停止代理、撤流并删除整个 runtime（包含 CA），不能保留可用旧 generation。

主动 disable 的成功顺序为 nft 撤流 → unit 停止 → 删除除公开 CA 外的运行文件 → 持久化
`enabled=false`，保留 key 和 canonical assignment。撤流失败不停止 unit、不删文件、不改
期望 enabled；停止失败保留运行文件与期望 enabled，但已撤流。撤流与停止均成功后，即使
运行文件清理失败也持久化 `enabled=false`，明确报告清理失败并要求 retry disable；保存失败
明确报错。

`GatewayService.enable()` 通过 cgroup v2 预检后，若恢复、发布或保存失败，将内存 enabled
改为 false 并尝试持久化 `enabled=false`，随后执行 fail-closed 清理：停止代理、撤流、删除
整个 runtime，不适用成功 disable 的 CA 保留；key 和 canonical assignment 保留。
禁用意图保存失败时，诊断必须明确指出 disabled intent 未持久化，并要求管理员在重启 daemon
前重试 `agent-sec-cli gateway disable`，否则磁盘中此前保存的 enabled 状态可能在重启后恢复。
cgroup v2 预检失败发生在 enabled 状态改变之前。失败撤销同样删除整个 runtime。
成功 disable 保留的 CA 位于 `/run`，主机重启仍丢失；直连 fake 不被替换，已有连接需重连。

### 5.1 responseRewrite 的持久化兼容边界

必填 bool 替代字符串是不兼容修订，不只是改作者 JSON 或标准 Policy 文件。旧 JSON
`PreparedPolicy`、Scope `policySnapshots`、Binding 内嵌 Policy，以及任何保存固定 Scope
快照的 `pinnedScope` 中，凡含旧 `buffered/none` 都不被新契约接受。SQLite 的表结构不变
也不能证明内嵌 JSON 可读；Repository 会严格反序列化这些快照。

**不能携带旧策略直接滚动升级。** 须单独安排配套备份及显式离线转换，或在旧版本仍能
管理这些记录时按原生命周期撤销、清理并重建全部相关持久化记录与 pinnedScope。转换须
覆盖所有内嵌副本并核对固定 revision/归属的一致性，不能只更新 latest Policy 或运行时 Plan；
将旧 `none` 改成 `false` 还须接受 headers/trailers 不再替换的行为变化。不可在常规加载、
恢复或 Policy update 中偷偷覆盖既有不可变 Scope/Binding 快照，也不自动兼容旧字段。
重新发布的 Plan 只能来自转换或重建后的 canonical 快照。

备份、转换/重建、回退与部署须作为独立迁移执行；没有自动迁移工具或旧格式滚动升级
保证。回退须使用与旧二进制/旧 addon 配套的备份和策略，不能直接沿用 bool 快照；
同时遵守本节已有的网关撤流与执行投影回退边界。

## 6. 交付与验证边界

发布验证须检查最终 RPM/raw 包中的策略和 addon，不以源码测试替代安装制品验收。

V2 RPM staging 只写 `DESTDIR`，包含 addon、policy 模块、gateway unit、daemon drop-in、installer
和 provenance。安装自动启动基础 daemon，但默认不启动代理、不创建网关账户、不下载
mitmproxy、不添加引流。未配置凭据 Scope、没有保存网关状态的初始安装，daemon 启动和
重启也不创建网关 runtime、凭据目录或 nft 表；默认关闭不跳过既有 assignment 或此前已启用
状态的同步恢复。管理员显式安装 standalone、导入 key、准备 Policy/Scope 后 enable。
最终卸载活动网关须先通过 daemon disable，失败拒绝卸载；升级不执行 erase 清理。手动安装
的 mitmproxy 与持久凭据保留。本设计不把源码 staging 当作最终 RPM 已构建或验收的证据。

raw 包交付相同网关静态资产。`anolisa --install-mode system install sec-core --backend raw` 按组件 manifest
安装 gateway unit、daemon drop-in 与 addon，unit 和卸载脚本中的路径随安装布局渲染。
基础 daemon 声明 `enable=false/start=false`，gateway unit 仅作为普通文件交付，不登记为
ANOLISA 托管服务，由 daemon 独占启停。安装不下载 standalone、不创建网关账户或引流。
raw 升级只更新文件，不因新增网关资产而启用或重启代理；运行中的代理须由管理员显式重启
以加载新 addon。raw 卸载在停止服务和删除文件前执行严格撤流钩子；活动 unit 或运行 manifest
存在时，disable 失败拒绝卸载，手动安装的 standalone 与持久凭据仍保留。
源码 `make uninstall` 也先检查活动代理、manifest 或残留 nft 表，成功 disable 后才删除文件；
并行卸载同样遵守此屏障。`DESTDIR` 暂存目录删除不操作宿主服务。

| 层级 | 验证入口 |
| --- | --- |
| Policy 与 Plan | `asc-policy-types/tests/authoring.rs`、`asc-gateway-plan/src/tests.rs` |
| Credential Store | `asc-credential-store/src/tests.rs` |
| addon | `tests/v2/gateway/test_policy.py`、`test_addon_blackbox.py` |
| Runtime 与权限 | `asc-gateway-runtime/src/tests.rs`；部署契约 DPROC-GW-001～003 |
| 安装态闭环 | `tests/v2/systemd/test_gateway.py`；DPROC-GW-004 |
| 默认关闭 | `tests/v2/e2e/test_daemon_process_e2e.py::test_default_start_and_restart_do_not_activate_gateway` |

本次响应修订的验收只使用内部 mock 上游，不以真实 GitHub 或其他外部 API 为验收条件。
这些入口用于分层验收，验证结果应分别报告；响应开关变更须覆盖以下边界：

| 验证对象 | 必须观察的结果 |
| --- | --- |
| Policy/Plan 与持久化类型 | bool `true/false` 接受；缺失、null、数字、字符串及旧 `buffered/none` 拒绝；旧 PreparedPolicy/Scope/Binding 内嵌快照不静默转换 |
| 请求与响应开关独立 | 两种 bool 都在请求头阶段授权并注入 Authorization；不等待/扫描/改写请求 body；`false` 的响应 headers/body/trailers 全部不替换 |
| 未压缩连续字节流 | identity、缺失及空 Content-Encoding；真实值/Authorization 任意跨 chunk 仍被替换；尾部有界、EOF 收尾、非终止空 DATA 不截断；超过旧 8 MiB 的正文不因累计大小拒绝、不整包缓存 |
| 压缩透传与协商 | `gzip/br/deflate` 正文字节与上游一致，不解压/扫描/重新压缩；Accept-Encoding 不变；`true` 只替换 headers/trailers，压缩 body 中 real 仍可返回 |
| SSE 与二进制边界 | 未压缩 SSE 的物理连续匹配可替换；跨 JSON 事件语义拼接不识别；hash/framing 不宣称通用协议正确性 |
| 收尾与失败 | HEAD/204/304 无正文及表示长度保持；响应长度/framing 一致；故障中止且不发送原始上游错误页，已发 headers/正文不可撤回；上游早结束不等待请求 EOF |
| HTTP/2 待发送队列 | 客户端窗口停滞时超过待发预算中止并回收队列，上游不被完整积攒；正常窗口更新的大响应仍成功；共享代理继续服务 |
| 标准 Git Policy | 原 buffered 项为 `true`、upload-pack 原 none 为 `false`；内部 mock clone/fetch/push，不能把成功例推导为所有二进制协议可改写 |

安装态 E2E 保留显式 `ASC_GATEWAY_E2E_USER` gate，只在允许修改宿主 systemd/nft 的专用环境
执行，不增加默认流量引流。测试使用独立 executable 路径与非 root owner，经真实 CLI 创建
Policy/Scope，先 enable/owner 查询再启动 worker，等待真实 Binding 接管。用本机
`198.18.0.1`、临时 CA 与 mock REST/Git 上游，验证上游收到 real，`true` 的未压缩回显返回
fake；`false` 与压缩 body 不能使用“Agent 响应必为 fake”的断言。还须验证 clone/fetch/push、
相同 secret 幂等、不同值拒绝覆盖、remove/add 后旧 fake 永久退役与新 Scope 配置、撤销、
多 Scope、多实例和空 Scope 删除。Token 随机生成，不访问外网 API；所有 Scope/Policy
资源 finally 清理。默认关闭测试只观察 `scopes=[]`、无 nft 表、无 runtime/凭据目录，不改变
宿主网关状态。文档矩阵或测试编写不等于已通过运行验证，安装态结果应独立报告。
