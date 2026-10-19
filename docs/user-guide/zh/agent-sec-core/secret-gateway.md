# Secret Gateway

[English](../../en/agent-sec-core/secret-gateway.md)

Secret Gateway 让真实 API Key 不进入 Agent 进程。Agent 独立启动，由 root 登记其 UID 和 PID，
并通过 Agent 自身配置使用形态相同的 fake key。Agent 把 fake key 发往允许的端点时，网关在链路上替换为真实 key；
fake key 在其他任何地方都没有用处。即使 Agent 被提示词注入后打印环境变量、写入文件或发往其他主机，
泄露的也只是 fake key。

当前版本保护一种凭证：GitHub Token（`github` preset），覆盖 GitHub REST API 与 HTTPS Git。
不支持自定义规则、其他服务商、内容检测和审计导出。

## 工作方式

- **Agent 域**：一个 UID 下的进程子集，对应具名 systemd scope
  `agentsec.slice/agent-sec-agent-<name>.scope`。root 登记已有 Agent 的 UID、PID，daemon 核验
  进程身份后将其及已有后代快照移入 scope；之后在域内创建的子进程继承归属。
- **引流**：daemon 安装 nftables 表 `inet agent_sec_gateway`，把各 Agent 域发出的 TCP 80、443
  流量重定向到网关，并丢弃 UDP 443，使客户端从 HTTP/3 回退到 TCP。回环流量和 Agent 域之外的进程
  不受影响。
- **网关**：`agent-sec-gateway.service` 以无特权用户 `agent-sec-gw` 运行固定版本的 mitmproxy
  standalone 构建，使用 transparent 模式。每个 Agent 域独占 `15001`–`15008` 中的一个端口，
  因此最多配置 8 个域。
- **Key**：root daemon 把真实 key 存放在 `/var/lib/agent-sec/credentials`，只把网关所需条目发布到
  `/run/agent-sec-gateway`。真实 key 不会进入 Agent 的环境变量、命令行或日志。

`github` preset 仅对以下请求注入真实 Token：

| Host | Path | Methods |
|------|------|---------|
| `api.github.com` | 任意 | GET, HEAD, POST, PUT, PATCH, DELETE |
| `github.com` | `…/info/refs` | GET |
| `github.com` | `…/git-upload-pack` | POST |
| `github.com` | `…/git-receive-pack` | POST |

Agent 在 `Authorization` 头中以 Token 或 HTTP Basic 密码的形式出示 fake key。该头中没有 fake key
的请求原样转发。网关拒绝请求时返回 JSON 响应体
`{"error": "<code>", "source": "agent-sec-gateway"}`，下列错误码状态码为 `403`，
`sni_host_mismatch` 为 `421`，钩子内部异常 `gateway_error` 为 `500`。常见错误码：

| 错误码 | 触发条件 |
|--------|----------|
| `credential_not_allowed` | fake key 发往上表之外的 host、path 或 method |
| `plaintext_credential` | fake key 经明文 HTTP 发送 |
| `real_credential_exposed` | 请求头、path 或请求体中包含真实 Token |
| `subject_mismatch` | fake key 属于另一个 Agent 域 |
| `retired_credential` | fake key 已被 `key remove` 撤销 |
| `gateway_not_ready` | 网关尚未加载该域的配置 |

对允许的 REST 及 Git ref、push 响应，网关会把回显的真实 Token 替换为 fake key。Git fetch 的
pack 数据直接流式透传。

## 环境要求

- 带 systemd、cgroup v2 和 nftables（`nft`）的 Linux；x86_64 或 aarch64。
- Rust V2 核心的 root `agent-sec-core.service` daemon。V1 Python CLI 不包含网关；已发布的
  `anolisa install sec-core` 和 RPM 包目前也不包含。
- Agent 域内的程序须通过[Agent 凭据与 CA 配置](#agent-凭据与-ca-配置)信任网关 CA。
  固定证书或忽略所列变量的程序需使用自身支持的 CA 配置方式，否则无法经网关访问 HTTPS 主机。

## 安装

先按 [SkillSec V2 指南](skillsec-v2.md#安装边界)构建并安装 V2 核心，再在仓库根目录安装网关文件：

```sh
sudo make -C src/agent-sec-core install-gateway
sudo /usr/share/anolisa/sec-core/gateway/install-mitmproxy.sh
sudo systemctl daemon-reload
sudo systemctl restart agent-sec-core.service
```

`install-gateway` 安装网关 addon、`agent-sec-gateway.service` 以及 daemon drop-in
`agent-sec-core.service.d/gateway.conf`，后者为 nftables 增加 `CAP_NET_ADMIN`。
`install-mitmproxy.sh` 从 `downloads.mitmproxy.org` 下载固定的 mitmproxy 12.2.3 压缩包，校验
SHA-256 后安装到 `/opt/agent-sec-gateway`，并创建 `agent-sec-gw` 系统用户。无法访问 Internet 的
主机可在别处下载 `mitmproxy.provenance` 中指定的压缩包，再通过 `--archive PATH` 传入。

## 登记 Agent 与配置 Key

Agent 用自己的启动命令、服务或容器入口运行，不需要由 gateway 启动。管理操作只允许内核认证的
UID 0；非 root 策略管理员也不能登记、修改 Key 或启停网关。

先取得已有 Agent 的 PID 和实际 UID。以下示例假定 Agent PID 为 `4242`、UID 为 `1000`，
执行前替换为实际值；这些命令会修改 cgroup、凭据配置和网关服务状态：

```sh
sudo agent-sec-cli gateway register --agent dev --uid 1000 --pid 4242
sudo sh -c 'umask 077 && cat > /root/github.token'   # paste the token, then press Ctrl-D
sudo agent-sec-cli gateway key add github --agent dev --secret-file /root/github.token
sudo agent-sec-cli gateway enable
```

- `--agent` 是全局唯一的小写 DNS label，标识一个 UID 下的进程子集。
- `register` 核验目标进程身份，将其及已有后代快照移入该域的 cgroup，不启动或重启 Agent。
  UID 不能为 root、`agent-sec-gw`，也不能属于可读取真实 Key 的网关组。
- 注册不要求网关已启用，也不要求已配置 Key。Key 必须关联已登记的域；移除最后一个 Key
  保留域登记，`unregister` 才删除域并撤销其全部 Key。注销不会停止 Agent 进程。
- 同一域可以再次登记同 UID 的新 PID；更换域 UID 必须先注销。进程重启后，需 root 登记新 PID。
- 真实 Token 文件必须由 root 所有，group/other 不可读。daemon 直接读取文件，Token 不经过 CLI socket。
- 每个域有各自的 fake。同一 preset 的所有域共用一个真实 Token：用不同 Token 执行 `key add`
  会为所有域轮换真实值，已有 fake 继续可用。

启用期间，Key 变更立即发布；无法确认撤销结果时 daemon 停止网关，避免旧 Key 继续生效。
配置在 daemon 重启后保留，已启用的网关在后台恢复；PID 不持久化，也不会在恢复时按旧 PID 接管进程。

只读与管理命令：

```sh
agent-sec-cli gateway status             # read-only: only your UID's domains; root sees all
agent-sec-cli gateway credentials --agent dev  # read-only: your domain's fakes and CA paths
sudo agent-sec-cli gateway key list       # read-only, root only: abbreviated fakes
sudo agent-sec-cli gateway key remove github --agent dev
sudo agent-sec-cli gateway unregister --agent dev
sudo agent-sec-cli gateway disable        # stops the gateway, removes routing and runtime files
```

## Agent 凭据与 CA 配置

`credentials` 返回 JSON `{agent, scope, environment}`，只包含该域的 fake 和 CA 路径，不移动进程。
普通用户只能读取自己 UID 的域，root 可读取全部；请求不能指定其他调用者 UID。

在 Agent 自身的凭据配置或环境配置中加载返回的 `environment`：

- `GITHUB_TOKEN`、`GH_TOKEN`：该域的 fake Token；
- `SSL_CERT_FILE`、`REQUESTS_CA_BUNDLE`、`CURL_CA_BUNDLE`、`GIT_SSL_CAINFO`、`NODE_EXTRA_CA_CERTS`、
  `DENO_CERT`：`/run/agent-sec-gateway/ca-bundle.pem`，由系统 bundle 加网关 CA 组成，enable 成功后可用。

注册无法修改已经运行进程的环境。使用 Agent 支持的配置重载机制加载 fake 和 CA；只支持启动时
读取配置的程序需自行重启，并登记新 PID。gateway 不写用户 profile，不安装系统 CA，不替换 Git helper，
也不清理代理变量。HTTPS Git 需自行配置 credential helper，以 fake 作为密码；避免 helper 使用真实 Token。
检查并去除会绕过透明引流的 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及小写形式。

只配置 fake/CA 不会自动归类进程；只有登记到域内的进程及其后代的新连接才会经过网关。

## 限制

- 注册只接管已有后代快照，不保证覆盖归类期间在尚未迁移的进程中并发创建的后代。建议在 Agent
  空闲、尚未开始任务时登记；注册前的流量和已经建立的连接不受追溯保护。进程移动失败可能已完成
  部分迁移，错误不代表已完整接管；核查进程后重试登记。
- Agent 和 gateway 启动顺序独立，但只有注册、Key 配置、enable 及 Agent fake/CA 配置均完成后，
  新连接才具有完整的注入效果。未被引流的 fake 可能离开主机，不能把这种情况称为请求已被阻断。
- 只引流 TCP 80 和 443。发往其他端口的 fake key 原样离开主机，对接收方无用。
- 网关只监听 IPv4 回环地址，因此 Agent 域发往 80、443 的 IPv6 连接会失败；随后改用 IPv4 的
  客户端仍可正常工作。
- 以同一用户运行的多个 Agent 域彼此不隔离：该用户可读取所有这些域的 fake key。
- 已撤销的 fake key 会一直保留记录以便继续拒绝；存储总共最多容纳 4096 个 fake key，
  已撤销的也计入在内。
- SSH Git 远端、GitHub Enterprise、`uploads.github.com` 和 raw 内容主机只会收到 fake key。
- 网关只检测 fake key 和真实 Token 的值，不过滤其他内容，也不记录流量；拒绝日志不包含 Token 值。
