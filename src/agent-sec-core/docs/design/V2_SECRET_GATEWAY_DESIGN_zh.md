# V2 Secret Gateway 设计

| 属性 | 值 |
| --- | --- |
| 状态 | [TARGET V2][IMPLEMENTED] |
| 范围 | 最小可用版本：内置 `github` preset |

## 1. 目标与边界

Secret Gateway 让 Agent 进程只持有与真实 key 同形的 fake。Agent 把 fake 发往允许的
端点时，网关在请求上把它换成真实值；fake 在其他任何地方都没有用途。被 prompt 注入的
Agent 即使打印环境变量、写入文件或发往其他主机，泄露的也只是 fake。

本设计只支持内置 preset，用户不能自定义规则。内容检测（DLP）、审计导出和 OAuth
流程不在范围内。

| 组件 | 位置 | 职责 |
| --- | --- | --- |
| Credential Store | `v2/crates/asc-credential-store` | 保存真实值，签发 fake，为网关生成快照 |

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
