# SkillFS 容器 Peer 认证

[English](container-peer-authentication.md)

这是 [issue #2439](https://github.com/alibaba/anolisa/issues/2439) 的开发计划。
本文描述的是计划行为；在实现和验收完成前，不应将其视为已经可用的部署契约。

## 当前开发状态

开发分支只实现 SkillFS 所有的部分，包括共享认证原语、control server、notify
client、fail-closed 配置门禁、文档、回归测试和独立的 Python 标准库 probe。
固定 handshake/protected-frame vector 和真实 FUSE probe loop 用于锁定拟议的
跨语言字节合同。

本分支不修改任何 agent-sec-core source 或 test。Peer-side 实现及所有权由 sec-core
maintainer 在确认合同后负责。真实 sec-core 独立容器 fixture、security-integrated
Pod profile 和 ACK 证据仍未完成。在这些跨组件和部署验收项通过前，本 issue 必须
保持 open。

## 目标

让 SkillFS 和 agent-sec-core 在独立 PID、mount namespace 中运行时，仍能认证
私有 Unix socket 流量。现有 executable identity 认证继续作为宿主机默认模式，
语义保持不变。

第一版容器 profile 保留现有 `shared_path` resolver transport。SkillFS 和
agent-sec-core 以相同绝对路径挂载同一个物理 source，workload 只获得传播后的
FUSE view。

## 安全边界

可信域由 SkillFS 和 agent-sec-core 容器组成。Kubernetes Secret volume 和物理
source 只挂载到该可信域，私有 runtime volume 承载双方的 Unix socket。

非可信 workload 可能知道 socket 路径；负向测试还会故意让它以只读方式看到
runtime volume。只要没有 Secret，它仍必须认证失败，即使使用与可信 peer 相同的
UID、GID、进程名或 executable basename。Socket directory 的写权限仍只属于可信
域；任何能够写入该目录的进程都可以 unlink 或替换 endpoint，造成可检测的拒绝服务。

Node root、可信容器已被攻陷以及能够读取 Secret 的 peer 不在本次安全边界内。

## Profile

### Host executable profile

该 profile 继续作为默认模式。SkillFS 验证 `SO_PEERCRED`、`/proc/<pid>/exe`
路径及文件身份、配置的 UID/GID 和进程 start time。现有 CLI、配置、wire format
和失败语义均保持不变。

### Container HMAC profile

该 profile 必须显式启用，并与 executable identity 互斥。SkillFS 和
agent-sec-core 从同一个绝对路径、nonblocking、no-follow、有大小上限、权限严格的
普通文件加载 secret。Nonblocking open 让 FIFO 等非普通文件进入 metadata 校验并
立即失败，避免启动阻塞。UID/GID 仍可作为附加约束，但不能被当作 container
identity。

每次建立 authenticated notify 连接前，SkillFS 都要求 socket 的直接父目录不是
symlink、由自己的 effective UID 所有，且不给 group 或 other 任何权限。`0700`
是推荐默认值，`0300` 等仍可用的更严格 owner 权限同样有效。Endpoint 必须是 owner
匹配的 Unix socket，且不给 group 或 other 任何权限；因此 agent-sec-core
listener 应将其 bind 为 `0600`。由于 agent-sec-core 会创建父目录和 endpoint，
首版 profile 要求 SkillFS
与 agent-sec-core 使用相同的 effective UID。支持不同 UID 需要未来明确引入新的
endpoint ownership policy。这些 metadata 检查是第一层可用性和部署边界；HMAC
交换仍是 peer identity 与业务 frame 完整性的边界，也负责防御同 UID 进程。

每条连接在读取或 dispatch 现有业务请求前，先完成有界 challenge-response：

1. Client 发送有界 `auth.init` frame。
2. Server 发送密码学安全的随机 nonce。
3. Client 返回经过 domain separation 的 HMAC-SHA256 proof。
4. Server 使用 constant-time comparison 验证，并返回自己的 domain-separated
   proof。
5. Client 验证 server proof 后，才发送业务数据。
6. 每个 sender 先发送现有 raw NDJSON 业务 frame，再发送对应的 `auth.frame` tag；
   receiver 验证 tag 后才能解析或 dispatch 业务 frame。

Control 和 notify 流量使用不同 domain。每条连接只处理一次请求，因此重连和
进程重启都需要新的 nonce。认证失败直接关闭连接，不得 fallback 到 executable
identity 或 plain protocol。业务 frame 与新 nonce 和 sender direction 绑定，
socket proxy 无法在转发握手后篡改 request、response 或 acknowledgement。

握手使用总 deadline，而不是每收到一个字节就重新开始计时的 timeout。这样能限制
shutdown 延迟，也能避免 peer 通过缓慢发送不完整 frame 永久占住单请求 control
loop。Socket owner 和可选 UID/GID 约束仍是第一层可用性边界，连接建立后再由共享
secret 认证 peer。

认证 frame 使用 NDJSON，并固定为以下 envelope：

```json
{"authVersion":"1","type":"auth.init"}
{"authVersion":"1","type":"auth.challenge","nonce":"<base64>"}
{"authVersion":"1","type":"auth.proof","proof":"<base64>"}
{"authVersion":"1","type":"auth.ok","proof":"<base64>"}
<existing raw business JSON>
{"authVersion":"1","type":"auth.frame","proof":"<base64>"}
```

Nonce 是使用带 padding 的标准 Base64 编码的 32 字节随机值。Proof 是
`HMAC-SHA256(secret, domain || NUL || raw_nonce)`，同样使用带 padding 的标准
Base64 编码。Domain 固定为：

- `anolisa.skillfs.control.client.v1`
- `anolisa.skillfs.control.server.v1`
- `anolisa.skillfs.notify.client.v1`
- `anolisa.skillfs.notify.server.v1`

业务 payload 的 tag 输入为：

```text
domain || NUL || "frame" || NUL || raw_nonce ||
u64_be(payload_length) || raw_business_json
```

Tag 使用共享 Secret 计算 HMAC-SHA256，并沿用 sender 对应的 client/server domain。
Payload length 不包含 NDJSON newline。Sender 先发送 raw business JSON 和 newline，
再发送 `auth.frame` 行。Receiver 保留 raw bytes，constant-time 验证 tag 后才解析或
dispatch。这能避免跨 language JSON canonicalization，同时保持内部 control schema
v1 和 notify schema v2 不变。

Secret 和可重用 proof 禁止出现在日志、响应、audit event、protocol event 或
提交到仓库的部署资产中。

## 实施阶段

### 第一阶段：共享认证原语

- 在 SkillFS 和 agent-sec-core 中增加严格的 secret-file 加载与验证。
- 定义兼容的 challenge、proof 和 protected business frame、大小限制、timeout、
  domain string 和固定的跨语言 test vector。
- 增加 constant-time proof 验证和失败信息脱敏。

### 第二阶段：Control resolver

- 为 SkillFS control server 增加与现有模式互斥的 HMAC peer mode。
- 为 agent-sec-core resolver client 增加显式 socket 和 secret 路径。
- 在 dispatch `ping`、`status`、resolver 或 activation method 前完成认证，同时
  保持业务 schema 不变。
- 保持 Flat、Hermes、fd-anchored resolution 和错误映射语义不变。

### 第三阶段：Notify 方向

- 认证作为 agent-sec-core daemon client 的 SkillFS。
- hardened mode 启用时，仅 `skill_ledger.skillfs_notify_change` 强制认证；daemon
  其他 API 保持现有兼容行为。
- SkillFS 验证 daemon response，避免 fake listener 伪造通知已接受。
- 同时启用 container HMAC control 和 notify 时，如果遗漏 notify key，启动必须失败，
  避免只认证一半的 profile。

Notify retry 和 durable reconcile 属于独立工作。认证失败继续遵循现有规则，
普通 FUSE I/O 不受阻断，active mapping 保持不变。

### 第四阶段：部署和本地验收

- 增加独立的 security-integrated Pod profile，不修改 standalone Sidecar 示例。
- 分别使用 source、propagated FUSE、runtime socket 和 Secret volume。
- 不启用 `shareProcessNamespace`。
- 增加使用独立 namespace 的正向和负向本地容器测试。
- 验证 restart、readiness、resolver、notify、activation、workload 拒绝和干净卸载。

## 本地验收

在 `src/skillfs` 下执行：

```sh
cargo +1.86.0 fmt --all -- --check
cargo +1.86.0 clippy --workspace --all-targets -- -D warnings
cargo +1.86.0 test --workspace
cargo +1.86.0 doc --workspace --no-deps
scripts/test.sh
```

运行独立 probe 的固定 vector 检查和单方面真实 FUSE 计划：

```sh
python3 scripts/container-peer-auth-probe.py self-test
```

sec-core formatter、lint、type、pytest 和双方 container fixture 属于其后续实现。
这些 fixture 遇到认证或 namespace 条件不可用时必须失败，不能 skip。

必须覆盖的负向场景包括 missing、empty、short、oversized、symlinked、FIFO、权限
过宽和 owner 错误的 secret file；错误、畸形、过期或 replayed proof；转发握手后
修改业务 frame；认证 timeout；UID/GID 不匹配；向 HMAC mode 发送 plain request；
不安全的 notify socket 类型、owner、mode 或父目录；以及可以访问 socket、使用
相同 UID 但没有 Secret 的非可信 peer。

## ACK 后续验收

本地完成后，在 ACK 进行一次聚焦验证，并记录：

- Kubernetes 版本、runtime、node architecture、manifest revision 和 image
  digest；
- 每个容器能看到的 Secret、source、runtime 和 propagated volume；
- 未启用 shared process namespace 时独立的 PID 和 mount namespace；
- resolver、notify、activation、readiness 和两个 sidecar 的 restart 路径；
- 能访问 runtime socket 的非可信容器被拒绝；
- Pod 退出和 residual mount 清理。

ACK 结果属于 release evidence，不是 recurring CI evidence。在完成这项验证和
issue #2012 中其余发布门禁前，不应宣称 security-integrated profile 已发布。

## 延后事项

- `SCM_RIGHTS` 或 directory-fd resolver transport。
- 从 agent-sec-core 移除物理 source mount。
- 把 shared PID namespace 作为受支持的安全依赖。
- Durable notify queue 或 reconnect reconciliation。
- Multi-source registration、source hot refresh、CSI 和 rootless FUSE。
