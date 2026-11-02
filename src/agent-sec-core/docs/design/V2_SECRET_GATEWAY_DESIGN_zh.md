# V2 Secret Gateway 设计

| 属性 | 值 |
| --- | --- |
| 状态 | [TARGET V2] |
| 范围 | GitHub 凭据；复用原生 Policy/Scope/Binding 与 process discovery |
| 相关文档 | [部署契约 §14](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md#14-target-v2-secret-gateway-进程权限与数据目录dproc-gw)、[daemon 协议 §18](DAEMON_PROTOCOL_V1_zh.md#18-target-v2-secret-gateway-管理)、[用户指南](../../../../docs/user-guide/zh/agent-sec-core/secret-gateway.md) |

## 1. 目标与授权对象

Secret Gateway 让 Agent 只持有与真实 GitHub Token 同形的 fake。网关只在 Policy 允许的 HTTPS
请求中把 fake 换成真实值，回显的真实值改回 fake。打印环境变量、写入文件或向其他主机发送
Agent 凭据只能泄露 fake，不能直接用于 GitHub 鉴权。

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
      "responseRewrite": "buffered"
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
- `responseRewrite` 为 `buffered` 或 `none`。可选 `because` 最长 256 字节且不含控制字符。

标准 GitHub Policy 随交付提供完整可复制 JSON，内容见用户指南；不依赖未确定的 package 路径：

| host | path | method | 响应改写 |
| --- | --- | --- | --- |
| `api.github.com` | `/**` | GET、HEAD、POST、PUT、PATCH、DELETE | buffered |
| `github.com` | `/**/info/refs` | GET | buffered |
| `github.com` | `/**/git-upload-pack` | POST | none |
| `github.com` | `/**/git-receive-pack` | POST | buffered |

上传接口、raw 内容、GitHub Enterprise 不在标准 Policy 中。Git upload-pack pack body 可能很大，
因此保持流式转发；refs 和 receive-pack 仍缓冲检查。

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

同一 Scope 的多实例共享 fake、端口与 Policy plan，各实例有独立 Binding 与执行 cgroup。
实例退出只撤除对应执行责任，不撤销共享 fake。匹配实例重启由 discovery 重新接管，fake
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
    "responseRewrite": "buffered",
    "because": "GitHub REST API with github"
  }]
}
```

`scopeId` 直接标识 Scope，`policies` 列出固定 revision，`credentials` 是凭据 ID 数组。
每条规则仅有 `source`、`match`、`responseRewrite`、`because`；`source.ruleIndex` 是作者
Policy 中的规则位置，数据面判定使用 rules 数组位置，不额外保存 index。`match.methods`
为方法数组，`match.credentialId` 为单个凭据 ID。

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

请求校验顺序保持：generation/listener → Authorization 重复与 Basic 格式 → 请求中真实值
泄露 → fake 的 carrier、Scope 与 tombstone → 明文 → Host/SNI/authority → Upgrade → HTTPS
端口与规范 path、Policy allow。常见拒绝码为 `gateway_not_ready`、`duplicate_authorization`、
`malformed_authorization`、`real_credential_exposed`、`fake_in_username`、`retired_credential`、
`subject_mismatch`、`carrier_not_allowed`、`plaintext_credential`、`invalid_host`、
`sni_host_mismatch`、`authority_mismatch`、`sni_missing`、`upgrade_not_allowed`、
`credential_not_allowed`。不携带托管凭据仍需通过有关校验，不是 TLS 透传。

拒绝响应为 `{"error":"<code>","source":"agent-sec-gateway"}`，通常 403；SNI/Host 不符为 421，
内部异常 `gateway_error` 为 500。日志只记录有界归属与请求信息、不含 Token；真实值泄露时
也不记录 path。

命中 allow 后只替换 Authorization token/Basic 密码，保留 Basic 用户名。仅注入后的响应需
反向改写：header/trailer 中真实值始终改回 fake；`buffered` body 限 8 MiB，只接受 identity/gzip，
解码/大小/编码不合格返回 502 或断开。改写后移除 ETag 和 Content-MD5。`none` 保持 body 流式。
响应钩子异常断开，不向 Agent 交付未检查响应。

## 5. 执行投影、发布与清理

`<数据目录>/gateway/state.json` 只保存 enabled、generation 与 Scope 执行投影：owner UID、端口、
撤销 fence、Binding 责任与完整实例身份。它不保存另一套授权规则。恢复必须从 PAP 读取
canonical Scope、固定的 Policy revision 和 Binding，核对 lifecycle 与实例身份；不从 PID
单独恢复、不读取 latest Policy 替代快照。

当前 daemon 在开放 UDS 请求前依次同步执行 `GatewayService.recover()` 与
`PapService.recover_scopes()`，不是后台恢复，也没有独立 gateway 恢复 Job。网关构造或恢复
失败单独记录错误；Active 凭据 Scope 恢复失败保留 `Active`，不转入删除，不阻塞原有
file-policy daemon readiness。file Scope 恢复或 PAP 存储扫描失败仍阻止启动。
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
以便重试。现有 reconciliation timer 调用 `sweep_deleting_scopes()`，在 Binding 清理完成后
执行 Deleting Scope 的 final release，包括零实例 Scope；不另设 gateway 恢复任务。
实例退出不退役共享 fake。全局 key retirement 对所有 Scope 生效。撤销不能确认
时停止代理、撤流并删除整个 runtime（包含 CA），不能保留可用旧 generation。

主动 disable 的成功顺序为 nft 撤流 → unit 停止 → 删除除公开 CA 外的运行文件 → 持久化
`enabled=false`，保留 key 和 canonical assignment。撤流失败不停止 unit、不删文件、不改
期望 enabled；停止失败保留运行文件与期望 enabled，但已撤流。清理与保存失败明确报错。
首次 enable 失败与失败撤销均删除整个 runtime，不适用成功 disable 的 CA 保留。保留 CA
位于 `/run`，主机重启仍丢失；直连 fake 不被替换，已有连接需重连。

## 6. 交付与验证边界

V2 RPM staging 只写 `DESTDIR`，包含 addon、policy 模块、gateway unit、daemon drop-in、installer
和 provenance。安装自动启动基础 daemon，但默认不启动代理、不创建网关账户、不下载
mitmproxy、不添加引流。未配置凭据 Scope、没有保存网关状态的初始安装，daemon 启动和
重启也不创建网关 runtime、凭据目录或 nft 表；默认关闭不跳过既有 assignment 或此前已启用
状态的同步恢复。管理员显式安装 standalone、导入 key、准备 Policy/Scope 后 enable。
最终卸载活动网关须先通过 daemon disable，失败拒绝卸载；升级不执行 erase 清理。手动安装
的 mitmproxy 与持久凭据保留。本设计不把源码 staging 当作最终 RPM 已构建或验收的证据。

| 层级 | 验证入口 |
| --- | --- |
| Policy 与 Plan | `asc-policy-types/tests/authoring.rs`、`asc-gateway-plan/src/tests.rs` |
| Credential Store | `asc-credential-store/src/tests.rs` |
| addon | `tests/v2/gateway/test_policy.py`、`test_addon_blackbox.py` |
| Runtime 与权限 | `asc-gateway-runtime/src/tests.rs`；部署契约 DPROC-GW-001～003 |
| 安装态闭环 | `tests/v2/systemd/test_gateway.py`；DPROC-GW-004 |
| 默认关闭 | `tests/v2/e2e/test_daemon_process_e2e.py::test_default_start_and_restart_do_not_activate_gateway` |

安装态 E2E 保留显式 `ASC_GATEWAY_E2E_USER` gate，只在允许修改宿主 systemd/nft 的专用环境
执行，不增加默认流量引流。测试使用独立 executable 路径与非 root owner，经真实 CLI 创建
Policy/Scope，先 enable/owner 查询再启动 worker，等待真实 Binding 接管。用本机
`198.18.0.1`、临时 CA 与 mock REST/Git 上游，证明上游收到 real、Agent 响应为 fake，以及
clone/fetch/push、相同 secret 幂等、不同值拒绝覆盖、remove/add 后旧 fake 永久退役与新 Scope
配置、撤销、多 Scope、多实例和空 Scope 删除。Token 随机生成，不访问
外网 API；所有 Scope/Policy 资源 finally 清理。默认关闭测试只观察 `scopes=[]`、无 nft 表、
无 runtime/凭据目录，不改变宿主网关状态。测试编写不等于已通过运行验证。
