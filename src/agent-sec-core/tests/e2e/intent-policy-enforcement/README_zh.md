# 意图—策略—执行：端到端演示与验收

本套件演示并验收意图 → Policy（PAP/PDP）→ Sandbox、AgentSight/ActPlane、Gateway 的安全闭环。`gateway.py` 独立验证 Gateway 凭据替换；`full.py` 通过 cosh 串起其余环节。

在装有待测组件、Python 3.11.6 的 Linux 开发机上，从套件目录运行：

```bash
cd src/agent-sec-core/tests/e2e/intent-policy-enforcement/
```

## 前提条件

- 已启动 agent-sec-core daemon，`agent-sec-cli` 可连通；PAP 策略列表为空；`/run/anolisa-task-policy` 不存在。
- 除 `intent.py` 外，其余入口以 root 启动：发布 PAP 策略、在 `/run` 建固定工作区、用 `runuser` 以测试账号运行探针与 cosh 都需要 root；命令统一传 `--user demo`（探针账号）。`intent.py` 不需要 root。
- **（仅 `gateway.py` 需要）管理员环境已登录可访问 GitHub 的真实账号。** Gateway 默认复用该凭据，按顺序取第一个可用来源：`GH_TOKEN`/`GITHUB_TOKEN` 环境变量、`gh auth token`、`git credential`（例如 `~/.git-credentials`）。因此默认运行无需输入，直接证明“注入后 `GET /user` 返回 200 且账号与直连一致”。若三者都不可用，先 `gh auth login` 或导出 `GH_TOKEN`；也可用 `--token-source prompt` 临时粘贴 Token。
- Gateway 验收使用专用、空闲测试主机；运行期间不要并发发布或修改 Policy/Scope。脚本保留已有凭据及其他资源引用的凭据；若发布结果复用了已有 pair，会在建立 Scope 前停止，避免 Scope 删除自动退役该凭据。
- 各入口的 `--check` 只做预检：不创建工作区、不发布策略、不执行探针、不读取凭据。

输出统一使用“仅预检/正式运行”标题、`── 阶段 ──` 分段和 `PASS`/`SKIP` 标记；失败打印 `[FAIL]` 并返回非零状态。正式运行失败时不打印成功摘要。

## 目录

```text
intent-policy-enforcement/
├── intent.py  sandbox.py  sight.py  gateway.py  full.py
├── fixtures/            # task.txt、task.cedar、gateway.cedar.in
├── support/             # 共享实现
└── checks/              # 无需服务的断言回归
```

## 一、意图服务：从任务生成策略

**准备。** daemon 启动环境设置 `ASC_INTENT_ENABLED=true` 并配置模型凭据，重启服务。[配置](../../../../../docs/user-guide/zh/agent-sec-core/policy-cli.md)

```bash
python3 intent.py --check
python3 intent.py
```

`--check` 为 `cat /etc/os-release` 申请权限，期望 `outcome=candidate` 并打印候选 Cedar，不生成策略。

正式运行把 [task.txt](fixtures/task.txt) 交给意图服务，检查草案形状：不含“不适合发布”的标记、恰好 3 条 `@advice` 确认、含 `@id("R2")`/`@id("R4")`/`@id("R5")`。通过后打印 `PASS intent`、SHA-256 和 `Consumer input: --policy .../task.cedar`。`// unresolved:` 只提示、不阻断。草案未审批未生效，由后续入口发布。

## 二、Sandbox：验证允许、裁剪和审批

**准备。** Sandbox 没有独立服务：`linux-sandbox` helper 随 CLI 安装在同目录，执行时由 `agent-sec-cli sandbox hook execute` 前台调起。需要启动的是提供 PAP/PDP 与授权回执的 agent-sec-core daemon。[配置](../../../../../docs/user-guide/zh/agent-sec-core/sandbox.md)

```bash
python3 sandbox.py --user demo --check
python3 sandbox.py --user demo
```

脚本不预启动 Sandbox 进程：每条命令都先经 `sandbox hook prepare` 申请权限，允许后再由 `sandbox hook execute` 交给 `linux-sandbox` helper 在受限命名空间中执行；探针以 `demo` 权限运行，但启动脚本本身需要 root。

脚本发布 [task.cedar](fixtures/task.cedar)，逐项核实：

| 结果 | 依据 |
|---|---|
| `PASS sandbox/1` | `execute` 回执 ALLOW、启动成功、退出码 0；构建日志写入，`.zshrc` 写入报 `Read-only file system`。 |
| `PASS sandbox/2` | `execute` 回执 MODIFY、退出码 1；源文件写入被拒，执行前后逐字节一致。 |
| `PASS sandbox/5-deny` | 写发布区得到 STEP_UP；`finish` 关闭请求、**不调用 `execute`**；发布文件保持原内容。 |

## 三、AgentSight / ActPlane：验证内核拦截

**准备。** 启动 PAP daemon、AgentSight 本地 API（`127.0.0.1:7396`）和加载 `agent-file-guard` profile 的 enforcer。[部署](../../../../../docs/user-guide/zh/agentsight/deployment.md)

```bash
python3 sight.py --user demo --check
python3 sight.py --user demo
```

脚本按普通账号探针 PID 建 Scope，Binding READY 后才发送操作：

- `PASS actplane/3`：回滚包完整，且有本轮被阻断的 `unlink`。
- `PASS actplane/4`：凭据读取被拒，且有本轮被阻断的 `read`。

复用意图生成的策略时，两个入口传同一个文件，并核对两份报告里 `PASS policy` 的 SHA-256 一致：

```bash
policy=/path/to/task.cedar
python3 sandbox.py --user demo --policy "$policy"
python3 sight.py --user demo --policy "$policy"
```

## 四、Gateway：独立验证凭据替换

**准备。** Gateway 资产与 daemon 已安装；准备 `nft`、`/usr/bin/curl`、`agent-sec-gw` 账号和 `/opt/agent-sec-gateway/current/mitmdump`；探针使用 IPv4 并能直连 GitHub。管理员环境需有可用的真实 GitHub 账号（见“前提条件”）。

```bash
python3 gateway.py --user demo --check
python3 gateway.py --user demo
```

正式运行：

1. 按“前提条件”取得真实 Token，调用 `key generate-fake` 生成替身；直连 `GET /user` 必须返回 200 及其 login，替身必须得到 401。
2. 等 Gateway ready，把 CA 与替身请求头放进普通账号的工作区。
3. 发布由 [gateway.cedar.in](fixtures/gateway.cedar.in) 渲染的凭据策略（schema revision 3），核对 PAP 回读字节与 SHA-256。
4. 按探针完整命令行参数和普通账号 UID 建 Scope，Binding READY 后才发送请求。
5. 普通账号只持有替身，经 Gateway 请求 GitHub，验收注入结果。

| 输出 | 依据 |
|---|---|
| `PASS gateway/controls` | 真实 Token 直连 200，替身直连 401。 |
| `PASS gateway/policy` | 凭据策略已发布，回读一致，报告记录 revision 与 SHA-256。 |
| `PASS gateway/6` | 经网关得到 **200 且账号与直连一致**；客户端日志只有替身、响应无真实 Token；Scope 包计数增加。 |

`gateway/6` 的 `github_status=200` 与 `account_matched=true` 写入报告；真实 Token 不出现在终端、报告或客户端日志。降权探针和验收读取子进程只接收最小环境，不继承管理员的 `GH_TOKEN`、`GITHUB_TOKEN` 或加载器配置。

发布前会在本轮报告目录写入 `gateway-recovery.json`，保存 Cedar 文件名、schema、来源登记摘要和运行前已有的 ID。若创建响应丢失，脚本按原始 Cedar 内容、schema、名称、revision 和运行前的 Policy 列表核对归属。找到唯一匹配项后继续清理。若只有凭据已登记，则用来源摘要在 daemon 的 `credentials/store.json` 中查找 pair；自定义数据目录的主机需让脚本的 `AGENT_SEC_DATA_DIR` 与 daemon 一致。

脚本会保留运行前已有或其他 Policy、Scope 引用的 pair。Scope 创建响应丢失时按本轮 Policy、UID 和脚本参数查找；最多查询 30 轮，等 Scope 从列表消失后才退役凭据。清理完成后删除恢复记录。若无法确认 Policy 最终是否提交、登记信息不唯一或服务不可达，终端报告 `Gateway cleanup incomplete`，并给出恢复记录路径。用记录里的来源与 ID 核对 `policy list`、`scope list`、`gateway key list`，再处理残留。恢复记录和报告均不含 Token；临时 Token 文件在退出时清除。

## 五、全流程：通过 cosh 验证意图、Policy、Sandbox 与 Sight

**准备。** 以上 daemon、Sandbox、AgentSight 依赖与 `tmux`、util-linux；普通账号已配置 cosh 模型和 agent-sec sandbox 扩展；主机没有其他 cosh 进程。全流程使用 command 模式，启动 cosh 时清除可选的 `AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS`。

沙箱根据本次 Bash 调用申请系统运行目录的读取和进入工作目录的权限。脚本与输出日志由命令候选单独申请。fixture 中的两份 Cedar 分别用于任务策略与 Gateway 凭据策略。

下面的 Linux 回归使用真实临时工作目录和 Case 1 的 PDP 测试策略，核对软链、空候选、单文件读写与 cwd 的隔离。设置 `ASC_SANDBOX_INTEGRATION=1` 和 `ASC_SANDBOX_HELPER` 后还会运行静态 helper，验证直接启动 shebang 脚本与越界写入结果。[回归测试源码](../../../v2/crates/asc-pep-runtime/tests/permission_input.rs)

```bash
cargo test --locked -p asc-pep-runtime --test permission_input task_policy_runtime_allows_narrow_case_one_launch -- --exact
```

```bash
python3 full.py --user demo --check
python3 full.py --user demo
```

正式运行无需任何按键：脚本生成并发布任务策略、启动 cosh、逐例发送任务，轮询回执与文件证据后自动进入下一例。可在第二个终端执行脚本打印的 attach 命令旁观（可选）。

覆盖的案例：1（允许写日志、越界写受限）、2（源文件修改被拒）、3（删回滚包被内核阻断）、4（原生读凭据被阻断）、5-fallback（降级只写构建报告）。

| 环节 | 验收 |
|---|---|
| 意图 / Policy | 生成任务 Cedar 并检查审批注解；cosh 的 shell 命令走真实命令意图，PDP 给出 ALLOW/MODIFY。 |
| Sandbox | 允许写日志、越界写受限、源文件不变；降级操作只写报告。 |
| Sight / ActPlane | 案例 3/4 的违规记录匹配本轮 Policy、Binding、操作和时间。 |
| Gateway | 由 `gateway.py` 覆盖（`PASS gateway/6`），不进 full。 |

完整结果为 `PASS 1`～`PASS 4`、`PASS 5-fallback`。需要人工审批的 `5-allow`/`5-deny` 与 Gateway 案例不在 full 中；任一 Binding 未 READY 或草案形状不符时脚本停止。

## 工作目录与排查

- 工作区固定为 `/run/anolisa-task-policy`（Cedar 匹配绝对路径）；路径已存在时脚本停止，不覆盖现有内容。
- 报告保存在启动时打印的 `Run report` 目录（`/var/tmp/anolisa-intent-report-*`、`/var/tmp/anolisa-task-policy-report-*`），`results.txt` 保存回执与证据。
- 诊断：`journalctl -u agent-sec-core -u agentsight-enforcer -u agent-sec-gateway`。Ctrl-C 会清理本轮资源并保留报告。
- 验证本地源码修改时，先用 `scripts/rpm-build.sh` 为相应组件编包并安装到测试机。

仅检查脚本自身断言（不启动服务、不请求 GitHub；凭据使用合成值，覆盖降权环境、策略登记后创建失败、创建或 Scope 响应丢失、启动失败清理、已有/共享 pair 保护、预设缺失和无效的启动失败路径）：

```bash
python3 -m unittest discover -s . -p check_assertions.py
```
