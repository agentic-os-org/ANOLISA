# Skill Ledger 的 SkillFS 集成设计：Canonical Path 与 Runtime Activation

本文从 Skill Ledger 视角定义 SkillFS 集成边界：Ledger 如何表达 Skill 身份、如何获取
本次操作的 I/O 根目录、如何接收变更通知，以及如何生成 SkillFS 消费的 activation
结果。SkillFS 的 mount、backing root 和路径映射属于 SkillFS 内部机制；本文只记录
Ledger 所依赖的外部行为保证。

本集成需要 SkillFS resolver provider、SkillFS notify v2 sender 与 Skill Ledger v2 consumer
协同部署。旧版 SkillFS 只发送 notify v1，不能直接与只接受 v2 的 Ledger 配合。

## 1. 目标与边界

集成的核心模型是：

```text
身份、配置、输出：canonicalSkillDir
实际文件操作：    ioSkillDir
展示名称：        skillName = basename(canonicalSkillDir)
```

| 组件 | 功能边界 |
| --- | --- |
| Skill Ledger | 管理 canonical Skill 身份、扫描与签名账本、用户决策和 activation target |
| SkillFS resolver | 回答某个 `canonicalSkillDir` 是否受管，并在受管时返回 Ledger 当前可访问的 live source |
| SkillFS notify | 使用 canonical 身份通知 Ledger 某个 Skill 可能已变更 |
| SkillFS runtime | 消费 Ledger 生成的 activation target，将选中的 snapshot 暴露到 canonical 视图 |

Ledger 不管理 mount session，不保存 canonical/live 映射，也不根据 basename、目录层级
或 manifest 内容猜测 live root。SkillFS 内部的 mount 组织、本地事件日志和 backing
root 布局不属于 Ledger 数据模型。

## 2. 路径模型

| 名称 | 语义 |
| --- | --- |
| `canonicalSkillDir` | 用户和宿主看到的绝对、词法规范化 Skill 路径；是配置、事件合并和结果输出的唯一权威身份 |
| `liveSkillDir` | SkillFS resolver 返回的、Ledger 当前可访问的 live source |
| `ioSkillDir` | Ledger 本次操作的实际根目录；SkillFS 模式下等于 `liveSkillDir`，host 模式下等于 `canonicalSkillDir` |
| `skillId` | notify v2 必填的非空字符串；Ledger 仅作为 opaque 诊断信息保留 |
| `skillName` | `canonicalSkillDir` 的 basename；保留 manifest v1 的叶子名语义，不是唯一身份 |

路径解析关系为：

```text
canonicalSkillDir
    │
    ├─ SkillFS managed ── resolver ──> liveSkillDir ──> ioSkillDir
    │
    └─ host / managed=false ──────────────────────> ioSkillDir
                                                       (= canonicalSkillDir)
```

`canonicalSkillDir` 只做绝对化和词法规范化，不通过 `realpath` 跟随 symlink，也不要求
在 daemon namespace 中可见。若 SkillFS source 本身是 symlink，canonical identity 必须保留
source 路径前缀。双方可在各自内部使用 realpath 做安全检查，但不能用它重写
canonical identity。canonical path 必须使用单个前导 `/`；`//skills/...` 等形式会被拒绝。

Hermes nested 和 mixed layout 只体现为不同的 canonical path，不进入 Ledger 的路径推断
逻辑。例如 `apple/apple-notes` 与 `google/apple-notes` 即使 basename 相同，也由各自完整
canonical path 区分。

`managedSkillDirs` 只保存 canonical path。Ledger 在开始顶层操作时解析一次 I/O 根，
后续 `scan`、`check`、`certify`、`show`、`decide`、`rollback`、`export`、`audit` 和 activation
流程复用同一个 `ResolvedSkillRoot`。所有文件读写使用 `ioSkillDir`；所有配置、结果、
错误和用户可见路径使用 `canonicalSkillDir`。

scanner finding 在签名前使用本次 `ResolvedSkillRoot`，将已知 `ioSkillDir` 及其
symlink-resolved alias 下的路径投影回 canonical 空间。相对路径和其他外部路径保持不变；
签名前若仍检测到已知 I/O root，则拒绝持久化该 finding。

## 3. SkillFS Resolver 合同

Resolver 复用 SkillFS trusted control socket 的 JSONL 业务协议。endpoint 按以下优先级
选择：

1. embedding caller 显式传给 `SkillFsResolverClient` 的 `socket_path`；
2. `AGENT_SEC_SKILLFS_CONTROL_SOCKET`；
3. per-effective-UID 默认路径：

```text
/run/user/<effective-uid>/skillfs/control.sock
```

最终 endpoint 必须与 SkillFS 的 `--control-socket` 一致。Ledger 不通过 notify 传递 endpoint、
mount id 或 generation，也不增加 retry、endpoint registry 或 resolver 结果缓存。

### 3.1 HMAC 配置与 key 文件合同

agent-sec-core 使用以下三个环境变量承载 SkillFS 集成配置：

| 环境变量 | 使用方 | 语义 |
| --- | --- | --- |
| `AGENT_SEC_SKILLFS_CONTROL_SOCKET` | Ledger resolver | 覆盖 control socket endpoint |
| `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` | Ledger resolver | 启用 control 双向 HMAC 认证 |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | agent-sec-core daemon | 启用 notify 双向 HMAC 认证 |

control key 在 resolver 第一次 resolve 时加载，并缓存到该 client 生命周期结束，不做热更新。
notify key 在 daemon bind socket 前加载；配置存在但 key 非法时 daemon 启动失败。双方使用
相同的严格 loader：路径必须为绝对路径，以 `O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK` 打开，
目标必须是当前 effective UID 所有的普通文件，不能设置任何 group/other 权限位，原始内容
长度必须在 32–4096 bytes 之间。读取内容不做 trim。symlink、目录、FIFO、device、owner
不匹配、权限过宽、过短或超长都属于配置错误。

control 和 notify 可以使用同一个 key 文件，但完整 ACS 部署必须同时配置两个方向，且推荐
使用方向独立的 key。key、proof、nonce 和未验证业务 payload 不得进入日志、响应、audit
event 或提交到仓库的部署资产。

### 3.2 HMAC wire contract

配置 control key 后，Ledger 是 client，SkillFS 是 server。读取 resolver 业务请求前先完成
四步 NDJSON challenge-response：

```json
{"authVersion":"1","type":"auth.init"}
{"authVersion":"1","type":"auth.challenge","nonce":"<base64>"}
{"authVersion":"1","type":"auth.proof","proof":"<base64>"}
{"authVersion":"1","type":"auth.ok","proof":"<base64>"}
```

nonce 是使用带 padding 的 canonical standard Base64 编码的 32-byte 随机值。握手 proof 为：

```text
HMAC-SHA256(secret, domain || NUL || raw_nonce)
```

control 使用固定 domain：

- client：`anolisa.skillfs.control.client.v1`
- server：`anolisa.skillfs.control.server.v1`

双方验证 proof 后，sender 先发送现有 raw NDJSON 业务 JSON 和 newline，再发送：

```json
{"authVersion":"1","type":"auth.frame","proof":"<base64>"}
```

业务 tag 的 HMAC 输入为：

```text
domain || NUL || "frame" || NUL || raw_nonce ||
u64_be(payload_length) || raw_business_json
```

`payload_length` 不包含 NDJSON newline。receiver 必须保留原始 payload bytes，并在解析 JSON
前 constant-time 验证 tag，不能重新序列化对端 JSON 后计算 MAC。所有 auth frame 上限为
4096 bytes；业务 frame 继续使用原有协议限额。整个 resolver 握手共享一个 monotonic
deadline `min(resolver timeout, 5s)`，不会因收到部分字节而重置。认证失败、EOF、超限或
timeout 都关闭连接，不 retry，也不降级到明文。

请求：

```json
{
  "schemaVersion": "1",
  "method": "skill.resolveLiveSource",
  "canonicalSkillDir": "/home/u/.hermes/skills/apple/apple-notes"
}
```

受管响应：

```json
{
  "schemaVersion": "1",
  "ok": true,
  "result": {
    "managed": true,
    "canonicalSkillDir": "/home/u/.hermes/skills/apple/apple-notes",
    "liveSkillDir": "/run/skillfs/backing/apple/apple-notes"
  }
}
```

未接管响应：

```json
{
  "schemaVersion": "1",
  "ok": true,
  "result": {
    "managed": false,
    "canonicalSkillDir": "/home/u/.hermes/skills/apple/apple-notes",
    "reason": "not_managed"
  }
}
```

Resolver 只查询路径，不执行 scan、decision、activation 或其他写操作。所有成功响应
必须包含 boolean `managed` 并原样回显 `canonicalSkillDir`。`managed=true` 时还必须返回
Ledger namespace 中绝对、词法规范化且可访问的 `liveSkillDir`。

| Resolver 结果 | Ledger 行为 |
| --- | --- |
| 未配置 control key，且 socket 不存在（`ENOENT`） | 保留 legacy 行为，进入 host 模式，`ioSkillDir = canonicalSkillDir` |
| 已配置 control key，且 socket 不存在（`ENOENT`） | 返回 `skill_root_resolve_failed`，不得进入 host 模式 |
| `ok=true` 且 `managed=false` | 校验 canonical echo 后进入 host 模式 |
| `ok=true` 且 `managed=true` | 校验 canonical echo 与 `liveSkillDir`，本次 I/O 只使用 live 目录 |
| `managed` 缺失或不是 boolean | 协议错误，返回 `skill_root_resolve_failed` |
| `ok=false`、连接拒绝、权限错误、timeout、认证失败、非法响应或其他错误 | 返回 `skill_root_resolve_failed`，不降级到 host |

Ledger 不重试、不缓存也不持久化 resolver 结果。未配置 HMAC 时，socket 不存在进入 host
模式是为无 SkillFS 部署保留的 legacy 可用性策略；显式或默认 socket 都维持该行为。配置
control key 即表示部署要求可信 SkillFS 必须可达，因此 socket 缺失、连接失败、认证失败或
协议错误全部 fail-closed，不能回退 host 或明文。通过认证的 `managed=false` 是 SkillFS
明确返回的可信未覆盖结果，仍可进入 host 模式。

## 4. SkillFS Notify v2 合同

SkillFS 发现受管 source 变化后，调用现有 daemon method：

```text
skill_ledger.skillfs_notify_change
```

通知复用 daemon Unix socket，并保持单连接、单请求语义。daemon socket 由
`AGENT_SEC_DAEMON_SOCKET` 指定，未指定时使用
`$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock`。SkillFS 当前会发送本地生成的 `id`，但
daemon 不消费该字段，而是为请求生成自己的 `request_id`。

未配置 `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` 时，现有明文 notify 保持兼容，但收到
`auth.init` 会直接关闭连接。配置 notify key 后，只有
`skill_ledger.skillfs_notify_change` 强制使用 HMAC；同一 daemon 的 health、prompt scan 等
其他 API 继续使用现有明文协议。此时明文 notify 在 handler 和业务 audit 前拒绝，不允许
认证失败后重试明文。

daemon 按第一帧分流连接：

- 合法 `auth.init` 且已配置 notify key：进入 HMAC notify；
- 合法 `auth.init` 但未配置 key：关闭连接；
- 包含 `authVersion`、`nonce` 或 `proof` 等认证字段但不是合法 `auth.init`：关闭连接；
- 其他普通请求：进入既有明文 daemon 路径，再由 method policy 拒绝不允许的明文 notify。

HMAC notify 中 SkillFS 是 client，daemon 是 server，复用 3.2 节 wire contract，并使用：

- client：`anolisa.skillfs.notify.client.v1`
- server：`anolisa.skillfs.notify.server.v1`

daemon 使用密码学安全的 32-byte nonce。握手总 deadline 为
`min(request_read_timeout_ms, 5s)`；auth frame 上限为 4096 bytes，业务 frame 仍使用 daemon
现有限额。配置 notify key 后，尚未收齐、无法安全分类的第一帧也受这个 pre-auth deadline
约束；完整且确认不是认证帧的普通请求仍进入既有明文路径。连接级 NDJSON reader 必须保留
一次 read 中多出的 bytes，确保 coalesced
payload/tag 不丢失。daemon 先验证 raw request payload 后才解析、记录 audit 或 dispatch，且
authenticated path 只允许 `skill_ledger.skillfs_notify_change`。成功响应和业务错误响应都先
写 raw JSON，再用 notify-server domain 的 `auth.frame` 签名；SkillFS 验证后才把通知视为
已接受。握手、proof、tag、EOF、timeout 或 frame size 校验失败都直接关闭连接，不 dispatch
也不返回未认证的错误 payload。

下面示例是握手后受 `auth.frame` 保护的原始业务 payload；业务 schema 本身不变。

Hermes nested skill 请求示例：

```json
{
  "id": "skillfs-01HX...",
  "method": "skill_ledger.skillfs_notify_change",
  "params": {
    "schemaVersion": 2,
    "canonicalSkillDir": "/home/u/.hermes/skills/apple/apple-notes",
    "skillId": "apple/apple-notes",
    "eventKind": "write",
    "paths": ["SKILL.md"]
  },
  "trace_context": {},
  "timeout_ms": 5000
}
```

| `params` 字段 | 约束 | Ledger 用途 |
| --- | --- | --- |
| `schemaVersion` | number，必须为 `2` | v1 不兼容，其他版本明确拒绝 |
| `canonicalSkillDir` | 绝对、词法规范化、单个前导 `/`；不支持 `~` 或 `//` | 唯一处理键；接收阶段不检查目录存在或 `SKILL.md` |
| `skillId` | 必填非空字符串；SkillFS 发送完整 id | 记录为 `reportedSkillId`；不解析格式、不参与处理 |
| `eventKind` | `mkdir` / `create` / `write` / `rename` / `unlink` / `rmdir` / `setattr` / `truncate` / `reconcile` | 诊断与事件合并 |
| `paths` | 相对 canonical Skill 根；不得为空字符串、绝对路径或包含 `..`；数组可为空 | 描述可能变化的文件 |

对 SkillFS 而言，通知只有在响应 `ok=true`、`data.schemaVersion=2` 且
`data.accepted=true` 时才算被 Ledger 接受。daemon 还会返回自己生成的 `request_id`、
队列状态和 per-skill 诊断，但这些字段不影响 SkillFS 的接受判定。关键响应字段为：

```json
{
  "request_id": "4d57d0ea-...",
  "ok": true,
  "data": {
    "schemaVersion": 2,
    "accepted": true
  }
}
```

通知成功只表示 daemon 已接收或入队，不表示 scan 或 activation 已完成。仅包含
`.skill-meta/**` 的事件在调用 resolver 前返回 `accepted=true, ignored=true`，避免 Ledger
写 metadata 形成通知循环。其余事件按 `canonicalSkillDir` debounce 和合并；事件可重复、
乱序或合并，worker 始终根据当前 I/O 目录重新计算状态。

## 5. Ledger 执行与 Activation 输出

daemon 对每个 canonical Skill 执行：

1. 调用 resolver 一次，得到本次操作的 `ResolvedSkillRoot`。
2. 在同一 context 中对 `ioSkillDir` 执行 scan 和账本校验。
3. 在签名前把 scanner findings 中的已知 I/O path 投影回 canonical 空间。
4. 根据 manifest、当前状态、用户决策和 activation policy 选择 target。
5. 在 `ioSkillDir` 中写入 activation metadata，所有结果和诊断使用 `canonicalSkillDir`。

Ledger 原子写入：

```text
<ioSkillDir>/.skill-meta/activation.json
```

并尽力在 `<ioSkillDir>` 上同步写入 `user.agent_sec.skill_ledger.activation` xattr。两者使用相同
UTF-8 JSON payload：

```json
{
  "schemaVersion": 1,
  "target": ".skill-meta/versions/v000002.snapshot"
}
```

`target` 为相对 `ioSkillDir` 的 `.skill-meta/versions/<id>.snapshot` 路径；
`target: null` 表示不暴露该 Skill。
`__pending_decision__.snapshot` 是保留的安全审查占位 target，不对应 manifest，也不进入
版本链。完整的版本、决策和 exposure 规则见 [Skill Ledger 主设计](SKILL_LEDGER_zh.md)。

activation target 由 Ledger 独立选择，SkillFS 不参与策略判定。当前 `pass_warn_only`
优先遵循用户决策；无决策时使用可信 latest snapshot、历史可信 fallback 或安全审查占位。
`block` 和 fail-safe 场景使用 `target: null`。daemon 在创建密钥、扫描和更新
activation 前校验配置；错误记入 Job 状态，本次处理停止，修正后可处理后续通知。
策略只激活 snapshot，不激活 source/current workspace。

错误边界：

- resolver 失败记录 per-skill `status=skipped`、`reasonCode=skill_root_resolve_failed`，job 保持
  `running`。
- scanner、路径或 activation 失败记录 per-skill `status=error`，不把整个 job 标记为不健康。
- 只有 worker loop、初始化或队列基础设施故障可以使 job 进入 `state=error`。
- startup reconcile 只枚举 `managedSkillDirs`，不把默认发现目录扩大为后台写入范围。

## 6. Ledger 依赖的 SkillFS 外部保证

Ledger 不依赖 SkillFS 的内部模块划分，只依赖以下可观测行为：

- SkillFS 维护 canonical path 到 live source 的映射，并由 resolver 返回当前有效的 live 目录。
- notify v2 使用 `canonicalSkillDir` 和完整 `skillId`，不向 Ledger 发送 live/backing path。
- SkillFS 从 live source 读取 activation metadata，并将选中的 snapshot 暴露到 canonical 视图。
- SkillFS 优先读取 xattr；文件系统不支持 xattr 时可以回退到 `activation.json`。
- activation target 为 `null`、越界、不是 snapshot、不存在，或两个有效来源不一致时，
  SkillFS fail-safe，不暴露该 Skill。
- SkillFS 不解析 Ledger manifest、`scanStatus`、activation policy、findings 或用户决策。

SkillFS 的本地 protocol event log 仅是内部诊断机制，不是 notify v2，也不是 Ledger 的
canonical 身份来源。Ledger v2 不读取该日志。

## 7. 部署边界与限制

- SkillFS 与 Skill Ledger 必须协调升级：Ledger 明确拒绝 notify v1；HMAC 联动还要求双方都
  实现相同的 auth version、domain、大小限制和 fixed vector。
- 完整 ACS HMAC profile 必须同时配置 control 和 notify key。SkillFS 使用
  `--trusted-peer-key-file` 与 `--notify-auth-key-file`，agent-sec-core 使用对应的
  `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` 与 `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE`。
  两个方向可以复用一个文件，但推荐使用独立 key。
- SkillFS `--control-socket` 必须与 `AGENT_SEC_SKILLFS_CONTROL_SOCKET` 一致。未配置 control
  key 时，默认或显式 endpoint 的 `ENOENT` 都保留 legacy host fallback；配置 key 后任何
  socket 缺失、连接或认证故障都 fail-closed。
- 容器化部署需要用 shared volumes 承载 daemon socket、control socket、双方对应的 key，
  以及 daemon 可见的 source/backing path。SkillFS 与 resolver peer 仍需在相同绝对路径看到
  physical source。应按部署拓扑尽量缩小 source、key 和私有 runtime volume 的可见范围。
- SkillFS 与 Ledger 必须使用相同 numeric UID；key 文件必须由该 effective UID 所有，socket
  路径及父目录也必须授权实际调用的 daemon、CLI 和 SkillFS 进程。
- Kubernetes Secret volume 是 symlink-based projection，不能直接作为 key path。部署需要先
  将 Secret bytes 复制到 `emptyDir` 等私有共享卷，生成 owner 正确、mode `0600` 的普通
  non-symlink 文件，再把双方配置指向复制后的路径。
- control key 在 resolver client 生命周期内缓存，notify key 在 daemon bind 前加载；本次不
  支持 key 热更新。轮换 key 需要协调更新双方文件并重启对应进程。
- 本次不处理 `.skill-meta` 视图策略、Agent 与 Ledger 的 UID/权限隔离、socket ACL/shared GID、
  Kubernetes manifest、notify 持久化或 daemon 通用认证。尤其在 Agent 与 Ledger 同 UID、同
  容器运行时，Agent 对 Ledger key/daemon socket 的潜在访问仍是独立的部署安全限制。
- 既有 `managedSkillDirs` 中的 live/backing path 必须迁移为 canonical path；Ledger 不自动猜测。
- 本次不引入 mount registry、mountId、generation、endpoint 传递或 canonical/live 缓存。
- canonical source 可以是 symlink；notify、resolver request 和 canonical echo 均保留绝对、词法
  规范化的 source 前缀。realpath 只用于双方各自的内部安全检查。
- `managedSkillDirs` 中的 glob 仍依赖 daemon namespace 的目录可见性；不可见的 canonical
  Skill 可能需要等待下一次 SkillFS notify。后续可通过 canonical replay 或只读枚举接口补齐。
- 无 SkillFS 时 canonical path 直接作为 I/O 路径；有 SkillFS 时差异只存在于 resolver 和
  `ResolvedSkillRoot` 内部，CLI、hook、Cosh、OpenClaw 与 Hermes 不接触 backing root。
