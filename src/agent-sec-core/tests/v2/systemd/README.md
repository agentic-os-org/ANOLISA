# V2 systemd 本阶段验收

本阶段覆盖前台进程、信号退出、有界 drain、runtime namespace、V2 RPM system unit。
持久化、业务重启恢复、readiness/健康检查、raw/container/Helm 不在本阶段。
契约与兼容变更见 [DPROC §8.1](../../../docs/design/DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md)。

## 运行检查

从 `agent-sec-core` 目录执行；Python 测试使用项目的 Python 3.11.6 环境。

```bash
(cd v2 && cargo build -p asc-daemon -p asc-cli --locked)
PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/pytest tests/v2 -ra
sudo env PATH="$PWD/v2/target/debug:$PATH" "$PWD/agent-sec-cli/.venv/bin/python" -m pytest tests/v2/systemd --require-systemd -v
```

所有测试均由 pytest 收集。普通容器/非 root 环境下 systemd 测试显示 SKIPPED，
`-ra` 展示原因；最后一条用 `--require-systemd` 强制验收，条件不足直接失败。它使用随机
`/run/asc-systemd-*` unit/runtime、复制的二进制和已有 root 账户，验证专用非 root
服务身份；不操作已安装的 `agent-sec-core.service`。finally 停止测试服务并清理测试资源。
SIGSTOP 故障注入会等待完整 45s stop timeout，整套通常需约一分钟。
该测试验证 system manager 生命周期；RPM 安装/升级脚本的目标发行版验收仍须在打包环境执行。

| Gate | 可执行证据 | 本次状态 |
| --- | --- | --- |
| DPROC-002/003 信号与 drain | `e2e/test_daemon_process_e2e.py`、Rust bootstrap/runtime tests | PASS |
| DPROC-012 安全目录/锁/socket | `e2e/test_daemon_process_e2e.py` | PASS；跨 UID owner 需 system 主机补证 |
| DPROC-V2-UDS-ACCESS-1 普通 UID 连接、管理拒绝、目录/锁保护 | `e2e/test_daemon_process_e2e.py` 跨 UID 用例 | 当前非 root SKIPPED；root RPM CI 可执行，无需 systemd |
| DPROC-014 同 namespace 单实例、仅 staging system unit | process E2E、`packaging/test_systemd.py` | PASS |
| Unit 语法 | staging 后 `systemd-analyze verify` | PASS |
| DPROC-V2-HARDENING-1 默认加固指令 | `packaging/test_systemd.py` | PASS；目标 systemd 下 syscall/W^X 业务兼容性待验收 |
| DPROC-013 system restart、journal、stop timeout、start limit | `systemd/test_lifecycle.py` | SKIPPED：当前会话无 root 权限/system manager |
| 持久化/业务恢复、readiness/持续健康检查 | 后续单独任务 | DEFERRED |

本次 Rust workspace 测试共 237 项通过（0 ignored），clippy、fmt、Python lint/格式化
及 26 项 Python 进程/打包测试通过。

进程测试通过只表示真实 binary/UDS 生命周期；RPC 成功只验证当前 PAP 请求可服务，不是
一个新增的 readiness 契约，也不证明策略已在 AgentSight 生效。

## 部署配置和兼容边界

V2 RPM 安装 `/usr/lib/systemd/system/agent-sec-core.service`，以 `root:root` 运行，
不创建专用账户或安装 sysusers 文件。
也可用 `make install-systemd-system DESTDIR=<staging>` 审阅生成物；该 target 仅 staging。
RPM 宏负责 system service 的 preset/卸载/升级生命周期，不自动启用用户 session 服务。
V2 spec 暂保留原有依赖声明及 wheel 专用 RPM 设置，发布前单独审计。源包只收录对应代际的 unit 模板。
V2 RPM CI 同时检查 system unit 的 root 身份配置，并断言 user unit 不存在；
`test-e2e-rpm-v2` 收集整个 `tests/v2/`，包含打包和 systemd 生命周期测试。容器中的 unit 检查仍不等于 system manager 验收。
默认启用 `SystemCallFilter=@system-service`、`SystemCallArchitectures=native` 和
`MemoryDenyWriteExecute=true`，并清空 capability 集合（issue #2861）。上述 pytest 生命周期
测试直接使用该模板，覆盖加固下的启动、RPC 和退出；目标发行版还须验证当前业务路径，
检查 journal 中的 SIGSYS、权限错误或内存执行限制错误。后续引入 JIT 或新增 syscall
依赖时必须重新验收。当前未完成真实 systemd 验收，不据此关闭 issue。

运维切换前停止、禁用原 V1 user service，安装 V2 包，然后按部署策略执行：

```bash
sudo systemctl enable --now agent-sec-core.service
sudo agent-sec-cli --socket /run/agent-sec-core/daemon.sock policy list
sudo journalctl -u agent-sec-core.service
```

运行目录 0755、socket 0666，普通用户无需加入服务组即可连接；目录仅服务账户可写，
锁保持 0600。需要非 root 管理员时，通过 unit drop-in 的 `--policy-admin-uid <UID>`
参数授权（覆盖 ExecStart 时先写空 `ExecStart=`），daemon-reload 并重启。
普通用户能连接不等于有 PAP 管理权限；扫描方法尚待迁移。
默认不授予 capability；AgentSight token 的服务账户读取权限需要单独配置，不能依赖
daemon 以 root 运行。服务配置重载未交付，SIGHUP 不 reload。

正常停止上限预算为 2s UDS drain、30s reconciliation join、1s Tokio shutdown；systemd
在 45s 强制终止整个 control group。锁文件刻意保留，避免不同 inode 各持一个 flock。
异常退出后仅在确认 socket 已失效时回收它。不会恢复已经丢失的内存 PAP 状态。

回滚：停止并禁用 V2 system service，恢复原 V1 包和 user service 配置；不要同时运行两代
服务。本次未变更 V1 raw 发布路径，也未实施状态迁移。直接消费者为 daemon binary、
V2 CLI、RPM staging；RuntimeLease 仅作为 binary 私有实现管理锁生命周期，library serve
仍由嵌入方负责 runtime namespace。依赖新增 rustix，Cargo.lock 同步。

尚存独立交付缺口：V2 RPM 仍打包 V1 hook/skills，当前 Rust CLI 不支持其全部命令，
例如 `scan-code`；文件/import 检查不证明插件安全功能可用。插件迁移和默认交付范围
需要单独收敛，不能以本阶段 system-service 检查通过宣称完整 V1 替代。

## 消融检查（2026-09-10）

| 移除项 | 可执行结果 | 最终决定 |
| --- | --- | --- |
| RuntimeLease 的 library 导出、长期保留的目录 fd | 精简后进程/打包回归验证 | 删除；仅 binary 使用，目录 fd 在 openat 完成后释放 |
| 锁文件 PID 写入 | 单实例和 SIGKILL 重启仍由 flock/inode/RPC 验证 | 删除重复状态和对应的实现细节断言 |
| 非阻塞 flock | 单实例拒绝测试失败：第二进程持续运行，等待退出超时 | 恢复并保留 |
| 存活 socket 探测 | live listener 保护测试失败：第二 daemon 替换 socket 并持续运行 | 恢复并保留 |

两个临时变体均只在测试目录运行，实验后已恢复正常代码并重新编译。
锁的持有对象仍保留到外层 Tokio shutdown 完成；目录权限、nofollow、锁文件验证、
失效 socket 的 inode 复核和有界退出不因精简而删除。真实 systemd 验收状态仍以上表为准。
