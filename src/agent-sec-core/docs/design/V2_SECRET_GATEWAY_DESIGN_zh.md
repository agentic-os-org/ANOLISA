# V2 Secret Gateway 设计

| 属性 | 值 |
| --- | --- |
| 状态 | [TARGET V2][IMPLEMENTED] |
| 范围 | 最小可用版本：内置 `github` preset |
| 相关文档 | [部署契约 §14](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md#14-target-v2implemented-secret-gateway-进程权限与数据目录dproc-gw)、[daemon 协议 §18](DAEMON_PROTOCOL_V1_zh.md#18-target-v2implemented-secret-gateway-管理)、[用户指南](../../../../docs/user-guide/zh/agent-sec-core/secret-gateway.md) |

## 1. 目标与边界

Secret Gateway 让 Agent 进程只持有与真实 key 同形的 fake。Agent 把 fake 发往允许的
端点时，网关在请求上把它换成真实值；fake 在其他任何地方都没有用途。被 prompt 注入的
Agent 即使打印环境变量、写入文件或发往其他主机，泄露的也只是 fake。

本设计只支持内置 preset，用户不能自定义规则。内容检测（DLP）、审计导出和 OAuth
流程不在范围内。

| 组件 | 位置 | 职责 |
| --- | --- | --- |
| Credential Store | `v2/crates/asc-credential-store` | 保存真实值，签发 fake，为网关生成快照 |
| Gateway Plan | `v2/crates/asc-gateway-plan` | 把内置 preset 展开为每个 Agent 域的规则 plan |
| 数据面 addon | `v2/gateway/addon/` | 在 mitmproxy 中按 plan 判定请求、替换凭据并改写响应 |
| Gateway Runtime | `v2/crates/asc-gateway-runtime` | daemon 内的控制面：保存期望状态，下发 generation，管理 unit、scope 和 nftables |
| CLI | `v2/apps/asc-cli/src/commands/gateway.rs` | root 登记已有 UID/PID 与管理 Key，按 UID 隔离的 fake/CA 查询 |

## 2. Credential Store

### 2.1 存储布局与写入

- 目录为 `<数据目录>/credentials`，必须属于 effective user（root）且模式恰为 0700；
  `store.json` 必须为 0600。路径逐级打开，任何一级是符号链接都拒绝。
- 打开时持有目录上的独占 advisory lock，只允许一个写者；同时删除崩溃遗留的
  `.tmp-*` 临时文件，因为其中可能有真实值。`store.json` 上限 8 MiB。
- 每次修改在副本上完成，校验 2.4 的不变量并原子替换文件后才对内存生效。写入失败时
  磁盘和内存都保持原状；文件已替换、只是持久性未确认时，内存跟随磁盘，避免已删除的
  凭据在重启前仍被下发。

`store.json` 结构如下，拒绝未知字段：

```json
{
  "version": 1,
  "credentials": [
    {
      "id": "github-token",
      "carriers": ["header_token", "basic_password"],
      "fakePrefixes": ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"],
      "real": "<真实值，退役后省略>",
      "fakes": [{ "agent": "dev", "value": "<fake>" }],
      "retiredFakes": [{ "agent": "old", "value": "<fake>" }]
    }
  ]
}
```

### 2.2 值与名称

- 凭据 ID 和 Agent 域名都是小写 DNS label（1–63 字节，`a-z`、`0-9`、`-`，首尾不为 `-`）。
- 真实值为 1–512 字节的可见 ASCII，因为 header 和 Basic 载体只能不加引号地承载这些字符。
  它的类型不实现 `Clone`、`Serialize` 和 `Display`，`Debug` 固定输出 `<secret>`；比较不在
  第一个不同字节处提前返回，只可能泄露长度，而 fake 本来就与真实值等长。
- carrier 表示 fake 允许出现的位置：`header_token` 是 `Authorization: Bearer|token <值>`
  中的 token，`basic_password` 是 `Authorization: Basic` 的密码部分。
- 所有错误信息都不包含真实值或 fake；JSON 解析错误只报告位置。

### 2.3 fake 签发

fake 保留真实值最长匹配的已知前缀和总长度；前缀之后的每个字母或数字替换为同类的随机
字符（拒绝采样，保证均匀），标点保持不变。客户端的格式检查因此能接受 fake，但 fake
不带任何可读标记，网关只能通过精确查表识别它。

- 前缀之后少于 16 个字母或数字的真实值会被拒绝，避免 fake 可以从形状猜出。
- 每个（凭据，Agent 域）对只有一个 fake，重复签发返回同一个值。
- 候选值与任何已托管的真实值或 fake 相同时重新生成，最多 8 次。

### 2.4 生命周期与不变量

- `put` 写入或替换真实值，结果为 `Created`、`Unchanged` 或 `Replaced`。替换会保留所有已签发
  的 fake，因此轮换真实值后 Agent 无需更新。真实值与另一凭据的真实值或任何 fake 相同时拒绝。
- `retire_fake` 把某个 Agent 域的 fake 变为 tombstone，其他域继续使用该凭据；之后再签发会得到新值。
- `remove` 丢弃真实值，所有 fake 变为 tombstone。对已退役的凭据再次 `put` 会重新启用它，
  但旧 fake 仍是 tombstone。
- tombstone 永不清理，被撤销的 fake 因此始终被拒绝，而不会退化成“未知值”被放行。
  代价是 tombstone 占用全局配额：每个 Agent 域最多 64 个活跃 fake，全部 fake（含
  tombstone）最多 4096 个。

加载和每次写入时都校验以下不变量，违反即视为存储损坏：

- 每个凭据至少有一个 carrier；
- 已退役的凭据没有活跃 fake；
- 同一凭据对同一 Agent 域最多一个活跃 fake；
- 所有 fake 全局唯一；
- 各真实值互不相同，且不等于任何 fake。

### 2.5 网关快照

`materialize` 把选中凭据的 fake→真实值映射和全部 tombstone 写成快照：

```json
{
  "version": 1,
  "generation": 7,
  "entries": [
    { "fake": "<fake>", "credential": "github-token", "agent": "dev",
      "real": "<真实值>", "carriers": ["header_token", "basic_password"] }
  ],
  "retired": [{ "fake": "<fake>", "credential": "github-token", "agent": "old" }]
}
```

未选中的凭据不离开存储目录；tombstone 总是全部写入。目标目录必须属于 effective user
且没有 group/other 写权限，快照文件为 0640、属组为网关组，原子替换。

`list` 只返回凭据 ID、状态（`active`/`retired`）、carrier，以及每个 fake 所属的 Agent 域、
是否为 tombstone 和末 4 位字符。

## 3. Gateway Plan

### 3.1 Preset

用户只能选择内置 preset，规则变化以新的 preset 版本发布。`github@1` 管理凭据
`github-token`，carrier 为 `header_token` 和 `basic_password`，fake 前缀为 `ghp_`、`gho_`、
`ghu_`、`ghs_`、`ghr_`、`github_pat_`。它只允许以下请求携带该凭据：

| host | path | method | 响应改写 |
| --- | --- | --- | --- |
| `api.github.com` | `/**` | GET、HEAD、POST、PUT、PATCH、DELETE | buffered |
| `github.com` | `/**/info/refs` | GET | buffered |
| `github.com` | `/**/git-upload-pack` | POST | none |
| `github.com` | `/**/git-receive-pack` | POST | buffered |

上传接口、raw 对象、GitHub Enterprise 和通配子域名都不在规则中，发往这些地方的 fake
会被拒绝，而不是被替换。`git-upload-pack` 的响应是 packfile，不会回显 token 且可能很大，
因此不改写。

### 3.2 Plan 格式

一个 plan 描述一个 Agent 域哪些 HTTPS 请求可以携带哪个托管凭据。plan 不包含真实值和
fake，网关通过另行下发的快照解析 fake。`preset_plan` 的输出是确定的：preset 的顺序和
重复不影响编码。

```json
{
  "format": "agent-sec-gateway-plan/v1",
  "subject": { "kind": "agent", "name": "dev-agent" },
  "source": { "kind": "presets", "presets": ["github@1"] },
  "credentials": [{ "id": "github-token" }],
  "credentialRequestDefault": "block",
  "requestDefault": "allow",
  "rules": [
    {
      "index": 0, "phase": "request", "effect": "allow",
      "match": {
        "scheme": "https", "host": "api.github.com", "path": "/**",
        "method": { "in": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] },
        "credential": { "in": ["github-token"] }
      },
      "responseRewrite": "buffered",
      "because": "GitHub REST API with github-token"
    }
  ]
}
```

- 携带本域 fake 的请求，只有被某条 `allow` 规则点名该凭据时才放行；
  `credentialRequestDefault` 固定为 `block`。不携带托管凭据的请求按 `requestDefault` 处理，
  preset 生成的值为 `allow`。
- `host` 是精确的小写 DNS 名；`path` 必须以 `/` 开头，`*` 匹配一段，`**` 跨多段，其余段
  只能是字面量，不能是 `.` 或 `..`。
- 上限：每个 plan 256 条规则、64 个凭据；path 512 字节，host 253 字节，`because` 256 字节。
- Rust 和 Python 两侧都拒绝未知字段和重复键，并共用 `v2/fixtures/gateway/` 下的 fixture，
  保证解析和判定结果一致。

## 4. 数据面 addon

数据面是固定版本的 mitmproxy standalone 构建（当前 12.2.3），版本与各架构的 SHA-256
记录在 `v2/gateway/mitmproxy.provenance`。addon 由两部分组成：`asc_gateway_policy.py`
只用标准库、不做 I/O，负责解析与判定；`asc_gateway_addon.py` 负责接入 mitmproxy 钩子。
每个监听端口对应一个 Agent 域，任何不确定都拒绝，网关不会退化为直通。

### 4.1 启动约束

`configure` 拒绝以下选项，避免凭据被绕过或写进日志：

- `ssl_insecure`：透明模式按原始目标 IP 连接，上游证书校验是防止伪造 Host 的唯一手段；
- `stream_large_bodies`：流式 body 会跳过检查；
- `save_stream_file`，以及 `flow_detail` 不为 0：转储和详细输出会记录注入后的凭据；
- 缺少 `agent_sec_runtime_dir`。

### 4.2 加载 generation

addon 每 0.5 秒检查一次 `<运行目录>/active.json`，通过 inode、mtime 和大小判断是否变化。
一次完整的 generation 包括 manifest、其中列出的快照和每个监听端口的 plan：

```json
{
  "version": 1,
  "generation": 7,
  "secrets": "secrets-7.json",
  "listeners": [{ "port": 15001, "agent": "dev-agent", "plan": "plan-dev-agent-7.json" }]
}
```

- 所有文件以 `O_NOFOLLOW` 打开，必须是普通文件且不超过上限：manifest 64 KiB，plan 1 MiB，
  快照 8 MiB。
- 快照的 generation 必须与 manifest 相同；监听端口和 Agent 域各自唯一，最多 64 个；
  plan 的 subject 必须等于所属监听端口的 Agent 域，plan 引用的每个凭据都必须已为该域签发 fake。
- 加载失败时继续使用上一个 generation，同一失败 5 秒内不重试；manifest 被删除时清空
  generation，此后所有请求都被拒绝。

addon 把加载结果原子写入 `status/health.json`：

```json
{
  "version": 1,
  "loadedGeneration": 7,
  "listeners": [15001],
  "lastAttempt": { "generation": 7, "error": null },
  "caError": null
}
```

启动时 addon 还会把 mitmproxy CA 证书写到 `status/ca-cert.pem`，失败时在 `caError` 中报告。

### 4.3 请求判定

每个请求按以下顺序检查，命中即拒绝：

1. 没有已加载的 generation，或监听端口不在 manifest 中：`gateway_not_ready`。
2. `Authorization` 头多于一个：`duplicate_authorization`；Basic 无法解码或缺少 `:`：
   `malformed_authorization`。
3. 任一真实值出现在 path（原样或 URL 解码后）、任意 header 值、解码后的 Basic 用户名或
   密码、请求 body 中：`real_credential_exposed`。
4. fake 出现在 Basic 用户名中：`fake_in_username`；出示的是 tombstone：`retired_credential`；
   出示的是其他域的 fake：`subject_mismatch`；carrier 不在该凭据允许的范围内：`carrier_not_allowed`。
5. 非 TLS 连接上携带 fake：`plaintext_credential`。
6. Host 无法解析：`invalid_host`；SNI 与 Host 不同：`sni_host_mismatch`；absolute-form 请求
   目标与 Host 不同：`authority_mismatch`，因为它可能被路由到已验证主机背后的其他虚拟主机。
7. 带 fake 的请求缺少 SNI 或 Host：`sni_missing`；带 `Upgrade` 头：`upgrade_not_allowed`，
   因为升级后的协议无法检查响应。
8. 端口不是 443，path 含 `\`、编码后的 `/`、`\`、`.`，或含 `.`、`..` 段，或没有规则同时匹配
   host、method、凭据和 path：`credential_not_allowed`。

不携带托管凭据的请求在第 5、6 步之后按 `requestDefault` 放行（`no_credential`），否则为
`request_blocked`。命中规则时，addon 把 `Authorization` 改写为真实值，Basic 保留原用户名；
需要改写响应时把 `Accept-Encoding` 改为 `gzip`。

拒绝响应为 `{"error":"<code>","source":"agent-sec-gateway"}`，状态码通常为 403，
`sni_host_mismatch` 为 421，钩子内部异常为 500 的 `gateway_error`。日志只记录拒绝码、Agent 域、
method、host 和不含 query 的 path；拒绝码为 `real_credential_exposed` 时连 path 也不记录。

### 4.4 响应改写

只有替换过凭据的响应会被改写，其余响应直接流式转发。

- header 和 trailer 中出现的真实值一律改回 fake。
- `buffered` 规则的响应先缓冲：`Content-Length` 超过 8 MiB 时直接断开连接；只接受
  identity 或 gzip 编码，否则返回 502 `response_encoding_unsupported`；解码失败返回 502
  `response_encoding_invalid`；body 超过 8 MiB 返回 502 `response_too_large`。body 中的真实值
  改回 fake，改写后删除 `ETag` 和 `Content-MD5`。
- 响应钩子内的任何异常都会断开连接，未经检查的响应不会交付给 Agent。

## 5. 控制面（Gateway Runtime）

控制面运行在 root daemon 内，进程、权限和目录契约见部署契约 §14，RPC 见协议 §18。

### 5.1 期望状态

`<数据目录>/gateway/state.json`（0600，目录 0700）保存期望状态，拒绝未知字段：

```json
{
  "version": 1,
  "enabled": true,
  "generation": 12,
  "agents": { "dev-agent": { "uid": 1000, "port": 15001, "presets": ["github"] } }
}
```

- 每个 Agent 域是一个 UID 下的进程子集，绑定 `15001`–`15008` 中的一个监听端口，最多 8 个域；
  域名全局唯一。root 通过 `register` 创建域，Key 只关联已登记的域。`presets` 可为空，
  移除最后一个 preset 不删除域；`unregister` 撤销其全部 Key 并删除登记，不停止 Agent 进程。
- PID 是一次注册的接管目标，不是持久身份；状态不保存 PID，恢复时不按旧 PID 重新接管进程。
- `generation` 是最后一次交给数据面的编号，无论成败都不复用：数据面按 generation 报告失败，
  复用的编号可能匹配到过时的失败报告。

### 5.2 Agent 域与隔离

- Agent 域是 systemd scope `agentsec.slice/agent-sec-agent-<name>.scope`。Agent 独立启动，
  root 提供目标 UID、PID；daemon 核验身份后通过 D-Bus `StartTransientUnit` 或
  `AttachProcessesToUnit` 将目标进程及已有后代快照移入 scope。
- 管理操作仅限内核认证的 peer UID 0；注册请求中的 UID/PID 是目标参数，不是调用者身份。
  普通用户的 `status`、`credentials` 范围来自 peer UID，非 root 策略管理员也仅能查询所属 UID。
- 域 UID 不能是 root 或网关账户，也不能属于网关组。网关账户 `agent-sec-gw` 的主组必须与账户
  同名且没有成员，因为该组可以读取真实值快照。注册和每次下发均检查隔离条件。
- 已登记域不能直接更换 UID，须先注销。不能将另一域中的进程接管到当前域。
- cgroup 是流量分类边界，不是同 UID 进程之间的安全隔离；同 UID 下的多个域可以读取彼此的 fake。
- 同一个真实值按 preset 共享：同一 preset 在多个域中使用同一个凭据，每个域各有自己的 fake。

### 5.3 下发 generation

`enable`、启用状态下的 key 变更以及 daemon 启动恢复都走同一流程：

1. 网关账户必须存在，且所有域通过 5.2 的隔离检查。
2. 分配新的 generation，并在尝试前持久化。
3. 在运行目录写入 `secrets-<n>.json`、每个已配置 Key 的域的 `plan-<agent>-<n>.json`，最后写
   `active.json`；无 preset 的登记域不生成 plan，也不引流。
4. 启动 `agent-sec-gateway.service`，等待 `status/health.json` 的 `loadedGeneration` 等于 `n`，
   最长 20 秒；若数据面报告该 generation 加载失败，立即返回其错误。
5. 写入 `ca-bundle.pem`：系统信任库（`/etc/pki/tls/certs/ca-bundle.crt` 或
   `/etc/ssl/certs/ca-certificates.crt`，取第一个存在的）加上网关 CA；网关 CA 必须由网关
   账户拥有、只有一个硬链接且为 PEM 证书。
6. 重写 nftables 表，然后删除其他 generation 的 plan 和快照。

运行目录为 `root:agent-sec-gw 0751`，`status/` 归网关账户所有（0755）。manifest、plan 和
快照为 0640、属组为网关组，`ca-bundle.pem` 为 0644。`status/` 下的文件由网关写入，
daemon 按不可信输入读取，并限制大小。

unit 在启动前和停止后都会删除 `status/health.json`。如果保留上一个进程的报告，新进程
绑定监听端口之前，daemon 会误以为当前 generation 已加载。systemd 的 `active` 状态不代表
网关已加载 generation。

### 5.4 失败收敛

- 普通变更失败时，数据面继续使用上一个已加载的 generation，期望状态已保存，下次 enable
  或 daemon 启动会重试。
- 撤销 key 时先保存期望状态，再修改 Credential Store。如果数据面无法加载新 generation，
  旧 generation 中仍有被撤销的映射，因此停止 unit 并删除整个运行目录（旧快照也含被撤销的值）；
  清理本身失败时，错误信息会提示管理员手动停止网关。
- 首次 `enable` 失败、状态仍为未启用时，停止 unit、删除 nftables 表和运行目录，未启用的
  网关不会继续服务。
- `disable` 停止 unit、删除 nftables 表和运行目录，保留已配置的 key 供下次 enable。

### 5.5 流量引导

nftables 表 `inet agent_sec_gateway` 由控制面独占，每次整体替换：

```
table inet agent_sec_gateway {
  chain redirect_out {
    type nat hook output priority -100; policy accept;
    ip daddr 127.0.0.0/8 return
    ip6 daddr ::1 return
    socket cgroupv2 level 2 "agentsec.slice/agent-sec-agent-dev-agent.scope" tcp dport { 80, 443 } counter redirect to :15001 comment "asc:agent:dev-agent"
  }
  chain filter_out {
    type filter hook output priority 0; policy accept;
    socket cgroupv2 level 2 "agentsec.slice/agent-sec-agent-dev-agent.scope" udp dport 443 counter drop comment "asc:agent:dev-agent"
  }
}
```

- 只有已配置 Key 且 cgroup 当前存在的域才会出现在表中。nft 在加载时把 cgroup 路径解析为 inode，
  所以 scope 每次重新创建后都要重写表；启用状态下 `register` 移入进程后刷新引流。表加载时
  如果某个 cgroup 恰好消失，整个事务失败，此时重试一次。
- 发往回环地址的流量不重定向；IPv6 也会被重定向，但监听端口只在 `127.0.0.1` 上，
  因此 IPv6 连接直接失败。丢弃 UDP/443，让客户端从 QUIC 回退到 TCP。

### 5.6 已有进程注册

`register` 接受 root 提供的 `{agent, uid, pid}`。目标 UID 非 0，且满足网关账户隔离要求。
域首次登记时分配端口，重复登记允许接管同 UID 的新进程；UID 不符或目标已属于另一域时拒绝。
注册可在网关未启用、没有 Key 时执行，不等待 Agent 启动。

控制面获取目标及已有后代的有界快照，先核验各进程 UID 和实例，再逐个移入 scope 并复核。
先迁移父进程，让其后续新建的子进程继承；随后迁移快照中的已有后代。进程消失或身份改变时返回错误，
不把失败当作完整接管。迁移不是原子事务，已经迁移的进程不会被自动停止或恢复。

注册前的流量和已经建立的连接不受追溯保护；快照之后在尚未迁移的后代中并发产生的进程可能漏在域外。
管理员应在 Agent 空闲时登记，保护从注册和引流成功后的新连接开始。

### 5.7 fake 与 CA 查询

`credentials` 是只读操作，按 peer UID 授权：root 可读取任一域，其他调用者仅能读取所属 UID。
返回 `{agent, scope, environment}`：

- 每个 preset 的 fake：`github` 对应 `GITHUB_TOKEN` 和 `GH_TOKEN`；
- `ca-bundle.pem` 的路径：`SSL_CERT_FILE`、`REQUESTS_CA_BUNDLE`、`CURL_CA_BUNDLE`、
  `GIT_SSL_CAINFO`、`NODE_EXTRA_CA_CERTS`、`DENO_CERT`。

查询不迁移进程，不要求已启用或已加载 generation；CA 文件在 enable 成功后生成。
Agent 通过自身配置或重载机制使用返回值，查询不会修改运行中进程的环境。返回结果包含 fake，
其类型不实现 `Debug`。

### 5.8 key 输入

`gateway.key.add` 只传递 secret 文件的绝对路径，真实值不进入请求帧和进程参数。daemon
以 `O_NOFOLLOW` 打开该文件，要求它是调用方拥有的普通文件且对 group/other 不可读，
最多读取 514 字节，去掉一个结尾换行后作为真实值。

## 6. CLI 与 Agent 自主配置

`agent-sec-cli gateway` 的子命令对应协议 §18 的方法。root 通过
`register --agent <name> --uid <uid> --pid <pid>` 登记已有进程，`key add` 仅传已登记域、preset 和
secret 文件路径，不重复指定 UID。`--secret-file` 转为绝对路径后发给 daemon。CLI 默认超时为
60 秒，大于 daemon 的 45 秒 dispatch 预算。

Agent 和 gateway 分别启动，CLI 不 exec Agent。`credentials --agent <name>` 返回 fake 与 CA 的
JSON 配置，不安装系统 CA、不写用户 profile、不清理代理变量、不替换 Git credential helper。

Agent 用自身的凭据与 CA 配置或重载机制加载 `CredentialGrant.environment`。不能重载环境的
Agent 需自行重启，再由 root 登记新 PID；仅更新启动环境文件不会改变已有进程的环境。
HTTPS Git 由 Agent 自行配置 helper，以 fake 作为 Basic 密码。用户必须确保客户端不再携带真实 Token，
也不通过其他端口上的代理绕过透明引流。

## 7. 已知限制

- 只引导 TCP 80 和 443；发往其他端口的 fake 原样离开主机，对接收方没有用途。
- 网关只监听 IPv4 回环地址，Agent 域发出的 IPv6 HTTP(S) 连接会失败。
- 隔离边界是 UID：同一用户的多个 Agent 域可以互相读取 fake，同一用户在域外运行的进程
  不经过网关。
- tombstone 永不清理，fake 总数（含 tombstone）上限 4096。
- SSH Git 远端、GitHub Enterprise、`uploads.github.com` 和 raw 内容主机只会收到 fake。
- 网关只识别 fake 和真实值，不检查其他内容，也不记录流量。

## 8. 验证

| 层级 | 证据 |
| --- | --- |
| Credential Store | `asc-credential-store/src/tests.rs` |
| Plan 与 preset | `asc-gateway-plan/src/tests.rs`、`v2/fixtures/gateway/` |
| addon 判定 | `tests/v2/gateway/test_policy.py`（无需 mitmproxy） |
| addon 黑盒 | `tests/v2/gateway/test_addon_blackbox.py`（需 `MITMDUMP` 指向 mitmdump） |
| 控制面 | `asc-gateway-runtime/src/tests.rs`；部署契约 DPROC-GW-001～003 |
| 安装态端到端 | `tests/v2/systemd/test_gateway.py`（DPROC-GW-004），需 root、systemd 和 `ASC_GATEWAY_E2E_USER` |
| 默认关闭 | `tests/v2/e2e/test_daemon_process_e2e.py::test_default_start_and_restart_do_not_activate_gateway` |

addon 黑盒和安装态端到端会改动宿主的 nftables、systemd 与系统账户，只能在允许这些改动的
专用环境里人工执行：`test-e2e-rpm-v2` 用 `--ignore` 排除这两个文件，`tests/v2/conftest.py`
在 `CI` 为 `true` 或 `1` 时再按文件路径跳过。自动化运行只保留只读的默认关闭校验：daemon
首次启动和重启后 `gateway status` 为未启用且无域，`inet agent_sec_gateway` 表不存在，
`/run/agent-sec-gateway` 与凭据目录都没有创建。

端到端测试全部在本机闭环：在 `lo` 上增加 `198.18.0.1`，用临时 CA 签发 `api.github.com`、
`github.com`、`example.com` 的证书并启动本地 mock 上游，通过 runtime drop-in 让网关只信任该 CA，
上游证书校验保持开启。测试使用随机生成的 dummy Token，直接断言上游收到的是真实值、
Agent 看到的是 fake，不访问外网。
