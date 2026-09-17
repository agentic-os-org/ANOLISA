# SkillGuard V2

[English](../../../en/agent-security/agent-sec-core/skillguard-v2.md)

SkillGuard 扫描 Skill 内容、签名扫描结果、保留可恢复的版本，并向 SkillFS 发布选定版本。
内置扫描全部在本地执行，不调用模型。Rust CLI 保留 `agent-sec-cli skill-ledger` 入口，
由一个 root daemon 执行业务操作。

本文说明 V2 核心能力。[Skill Ledger 指南](skill-ledger.md)说明 V1 及其 Agent 集成。
V2 Agent Hook 迁移单独交付；本次核心迁移不更新现有 Hook 的初始化检查、默认策略或启用状态。

## 安装边界

正式发布组件的常规入口为 `sudo anolisa --install-mode system install sec-core`，Alinux 用户还可使用
[安装指南](QUICKSTART.md#安装)中的 RPM 方式。这些入口选择已发布产物，不会选择尚未发布的 V2
迁移分支。安装正式版本不代表已获得 SkillGuard V2。

V2 RPM 需要在 Linux 上按仓库配方从本分支构建。完整配方还会构建保持现状的 sandbox 和插件包，
需要 Node.js 22.14 或更新版本、Rust 1.93 或更新版本、RPM 工具、systemd RPM 宏、C 编译器、
pkg-config 和 OpenSSL 开发头文件：

```sh
./scripts/rpm-build.sh agent-sec-core-v2
sudo yum install ./scripts/rpmbuild/RPMS/x86_64/agent-sec-cli-*.rpm
```

aarch64 使用对应架构目录。本阶段安装核心 CLI 包；元包还会拉入本次尚未迁移的 Agent 集成。
V1 和 V2 CLI 使用相同包名和可执行文件路径，安装 V2 会替换 V1。先停止 V1 写入者并保留匹配备份，
具体见下文部署回退。

仅从源码安装核心时，使用 Linux 和 Rust 1.93 或更新版本，在仓库根目录执行：

```sh
make -C src/agent-sec-core build-cli-v2
sudo make -C src/agent-sec-core install-core-v2
```

产物为 `src/agent-sec-core/target/v2/bin/agent-sec-cli` 和 `agent-sec-daemon`。
核心二进制不依赖 Python Ledger 运行时。`install-core-v2` 将两个二进制安装到 `/usr/bin`，
系统 unit 安装到 `/usr/lib/systemd/system`，初始配置以 0600 安装到 `/etc/agent-sec/skillguard.json`。
重装保留已有配置；RPM 使用 `%config(noreplace)`。安装过程不会初始化签名密钥或启用 Agent Hook。

RPM 遵循发行版的 systemd preset，可能启用该服务的开机启动。检查配置后，显式启动 root 系统服务：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now agent-sec-core.service
sudo systemctl status agent-sec-core.service
agent-sec-cli skill-ledger status
```

使用系统级 `systemctl`，不带 `--user`。unit 创建运行、状态和日志目录，保持密钥和日志私有，
只向 daemon 授予 `CAP_DAC_OVERRIDE`、`CAP_CHOWN` 和 `CAP_FOWNER`。
保留 `NoNewPrivileges`、`SystemCallFilter=@system-service`、native syscall 架构、
`MemoryDenyWriteExecute` 和内核保护，同时保留 HOME、`/tmp`、系统 Skill 目录及共享挂载的访问能力，
以完成扫描、元数据发布和回滚。daemon 不具有 `CAP_SYS_ADMIN`。自定义 SkillFS 挂载须对服务可见。

评估时使用隔离部署；V1 和 V2 不得同时写同一份 Skill 元数据。
实现状态与实际 Linux 交付验收分别记录在[迁移文档](../../../../../src/agent-sec-core/docs/design/SKILL_GUARD_PHASE_ONE_zh.md)。

`V2_CARGO_TARGET_DIR` 指定共享 Cargo 构建缓存，`V2_BIN_BUILD_DIR` 指定二进制暂存目录。
构建和安装时传入相同的暂存目录设置。已有 Linux 构建可用以下命令检查暂存安装布局及配置保留：

```sh
bash src/agent-sec-core/tests/packaging/test-skillguard-install.sh
```

该暂存检查不会启动 systemd，也不代表真实安装验收通过。

以前台进程运行开发 daemon 时，先创建 root 所有、模式 0755 的运行目录，以及 root 所有、模式
0700 的空状态目录。通过 `agent-sec-daemon serve --socket /run/agent-sec-core/daemon.sock`
指定绝对 socket 路径，可用 `--skillguard-config /etc/agent-sec/skillguard.json` 指定配置。
进程必须以 root 运行。运行目录必须预先存在；私有状态目录不存在时由 daemon 创建。

## 系统配置与信任

daemon 读取 root 所有的 `/etc/agent-sec/skillguard.json`；`--skillguard-config` 可指定其他绝对路径。
父目录必须可信，配置文件不得允许组或其他用户写入，也不得是符号链接或有多个硬链接。
默认文件缺失时使用以下默认值；格式错误或权限不安全会导致启动失败。

```json
{
  "stateDir": "/var/lib/agent-sec/skillguard",
  "managedSkillDirs": []
}
```

`managedSkillDirs` 保存具体 Skill 根目录，不支持父目录通配符。显式 scan 或 certify 会注册对应根目录。
`init`、`scan --all` 和 `check --all` 还会发现当前调用者默认位置下的 Skill，与 daemon 注册表合并。
传入一个根目录不会自动注册其兄弟目录。`status` 查询共享注册表，不依赖调用者 HOME。
默认发现跳过隐藏子目录，包括账本快照；`.hermes/skills` 等宿主目录下的正常 Skill 仍受支持。
发现范围包括 `$XDG_DATA_HOME/anolisa/skills` 下的直接 Skill 子目录，环境变量来自 CLI 调用者。
`XDG_DATA_HOME` 未设置、为空、为相对路径或含 `.`/`..` 路径段时，使用
`$HOME/.local/share/anolisa/skills`，与 ANOLISA 用户安装目录规则一致。`HOME` 未设置或为空时，
使用 CLI 当前用户的系统账户主目录。合法的自定义目录不存在时
直接跳过，不再回退。显式 Skill 路径及 `init --no-baseline` 不触发自动发现。

状态目录归 root 所有，模式为 0700；`signing-key.pk8` 为 0600。第一阶段允许所有本地用户操作所有
受管 Skill，文件归属不作为用户访问 ACL。仅内核 UID 0 可以换钥。系统密钥签名的是 daemon 计算的记录，
不是调用者提交的任意字节。

不导入 V1 历史、加密密钥或 keyring。新 manifest 绑定 Skill 的规范绝对路径身份。
移动到另一个身份后，需要重新建立信任；不同位置的同名 Skill 具有独立身份。

## 核心流程

以下命令由管理该 Skill 的普通用户执行。`init --no-baseline` 仅初始化共享密钥。
`scan` 也会初始化缺失的密钥；两者均不会静默替换损坏的密钥。

```sh
agent-sec-cli skill-ledger init --no-baseline
agent-sec-cli skill-ledger list-scanners
agent-sec-cli skill-ledger analyze /path/to/skill
agent-sec-cli skill-ledger scan /path/to/skill
agent-sec-cli skill-ledger check /path/to/skill
agent-sec-cli skill-ledger show /path/to/skill
agent-sec-cli skill-ledger audit /path/to/skill --verify-snapshots
agent-sec-cli skill-ledger status --verbose
```

`analyze` 只读，不初始化密钥，也不写账本。默认内置扫描器为 `code-scanner` 和 `static-scanner`。
可用 `scan --scanners code-scanner,static-scanner` 显式选择。内容变化时创建新版本；
内容不变时补齐缺少的扫描结果。`scan --force` 可替换相同内容的扫描结果，不凭空增加内容版本。

完整性状态共有六种：

| 状态 | 含义 |
| --- | --- |
| `none` | 内容尚无通过认证的扫描判定 |
| `pass` | 通过认证的内容具有通过扫描结果 |
| `warn` | 通过认证的内容具有警告发现项 |
| `deny` | 通过认证的内容具有拒绝发现项 |
| `drifted` | 当前内容与通过认证的记录不一致 |
| `tampered` | 账本元数据、签名或版本绑定未通过认证 |

执行错误和 `unmanaged` 诊断不属于额外的完整性状态。扫描成功表示扫描完成，不表示安全判定一定为 `pass`。

首次初始化密钥前，检查没有账本的 Skill 仍返回 `none`、退出 0，审计其空历史也成功。
两种查询均不创建密钥或元数据。已有账本但密钥缺失或无效时，不会按空历史处理。

## 外部结果、人工决策与恢复

通过 CLI 导入外部 JSON findings 报告。自定义 `skill`、`cli` 或 `api` 扫描器仅用于导入结果；
root daemon 不会执行其配置的命令。

```sh
agent-sec-cli skill-ledger certify /path/to/skill --findings /path/to/findings.json --scanner skill-vetter
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason 'Reviewed this version'
agent-sec-cli skill-ledger decide /path/to/skill --action always_allow
agent-sec-cli skill-ledger decide /path/to/skill --action block
agent-sec-cli skill-ledger decide /path/to/skill --action rollback --version v000001
agent-sec-cli skill-ledger decide /path/to/skill --clear
agent-sec-cli skill-ledger export /path/to/skill --version v000001 --output /path/to/empty-export
agent-sec-cli skill-ledger activate /path/to/skill
```

`allow` 批准选定版本；`always_allow` 保存持续的人工批准。`block` 隐藏暴露内容。
`rollback` 恢复选定的已认证快照并记录决策；`--clear` 清除人工决策。
回滚后的文件归 Skill 根目录所有者所有，普通用户仍可继续编辑；特权文件模式位会被清除。

导出向调用者所有的空目录写入 `snapshot/`、`manifest.json` 和 `findings.json`。
缺少的导出目录由 CLI 以调用者身份按 0700 创建；已有目录权限保持不变，非空目标目录会被拒绝。
`--delete-findings` 仅在认证成功且确认本地报告未变化后删除该报告。

`show` 展示 `active`、`pending` 或 `hidden` 选择。在 `pass_warn_only` 下，符合条件的已认证
`pass` 或 `warn` 版本可以自动暴露；最新内容不安全时可保留符合条件的旧版本，或呈现待审视图。
人工决策参与版本选择。`activation.activationPending=true` 表示账本已提交，但发布尚未完成。
检查 `show`，修复所报告的原因后用 `activate` 重试。daemon 重启会恢复支持的中断发布和回滚状态，
不承诺持久化通知队列。

启动恢复覆盖已登记 Skill。首次扫描若在版本写入后报告登记失败，应修复失败原因并对该 Skill
重试 `scan`；内容未变化时复用已验证版本，完成登记后再发布激活。首次请求失败时，不保证重启后
自动发现该目录。

## SkillFS 绑定

SkillFS 与普通 CLI 共用公共 socket。notify 握手和消息保留现有 HMAC 合同，普通 V2 JSON 仍严格解析。
公共 socket 必须归 root 所有、模式为 0666，直接父目录归 root 所有、模式为 0755。
SkillFS 还会检查祖先目录安全性与真实内核对端 UID。权限调整不允许明文降级或绕过 HMAC。

daemon 显式配置规范路径与 backing 路径的映射：

```json
{
  "stateDir": "/var/lib/agent-sec/skillguard",
  "managedSkillDirs": ["/home/alice/.openclaw/skills/demo"],
  "skillfs": {
    "authKeyFile": "/etc/agent-sec/skillfs-hmac.key",
    "mounts": [{
      "controlSocket": "/run/user/1000/skillfs/control.sock",
      "canonicalRoot": "/home/alice/.openclaw/skills",
      "liveRoot": "/home/alice/.openclaw/live-skills",
      "peerUid": 1000
    }]
  }
}
```

使用独立的随机 HMAC 密钥，长度 32–4096 字节，不得使用签名密钥。
daemon 的副本必须归 root 所有、模式 0600；向 SkillFS 用户提供内容相同、归其所有的独立 0600 文件，
不要将 daemon 的私有文件改为所有人可读。以上 UID 1000 示例中，SkillFS notify 的 `socket_path`
设为 `/run/agent-sec-core/daemon.sock`，`auth_key_file` 指向其用户私有副本。
control socket 仍归 UID 1000 所有，父目录 0700、socket 0600；control 的
`trusted_peer_uid=0`，`trusted_peer_key_file` 指向同一用户私有副本。必须使用 HMAC 认证，
不设置 `trusted_peer_exe`，因为可执行文件认证是另一种互斥的认证模式。
按 [SkillFS 运行时参考](../../../../../src/skillfs/docs/security/runtime-activation-implementation-plan.md)
配置 activation 和 backing root；其中旧示例的 daemon 端点和对端身份应替换为上述 V2 配置。

两个进程必须能看到配置中的绝对路径及相同 backing 文件系统对象，因此容器部署需要匹配的共享卷路径。
同一普通非原地挂载的 canonical/live 根可以完全相等，否则必须互不包含。
规范根目录和 live 根目录不得与其他映射交叠。resolver 失败时不会回退为扫描 FUSE 视图；
目录 inode 不匹配时会拒绝操作，不会给替换后的内容签发记录。

通知按 Skill 合并，然后依次触发扫描与激活。notify 确认仅表示已接受入队。
启用 SkillFS 时，`status` 包含 `skillfs` 的 queue/running/processed/failed 计数。
通过计数、公共审计和真实 FUSE 读取，分别确认受理、处理完成和可见发布；启动时会重新调度已注册的挂载 Skill。

## 输出、审计与限制

CLI 向 stdout 输出业务 JSON。`check` 遇到 `deny`、`tampered` 或执行错误时返回 1；
已完成的 scan/certify 和完整 analyze 即使发现风险也返回 0。
analyze 覆盖不足返回 1，输入无效返回 2。传输失败写入 stderr 并返回 1。
必须同时检查 verdict、activation 字段和进程退出码。

用 `--socket` 或 `AGENT_SEC_DAEMON_SOCKET` 指定端点。SkillGuard 默认超时 60 秒；
`--timeout-ms` 在服务端最大为 120 秒。超时不能证明写入是否已提交，重试修改前先查询状态和历史。
响应过大时返回 `ResponseTooLarge` 和 `operationMayHaveCommitted=true`，不会截断业务数据。

公共审计使用共享 Action Runtime 和私有 `/var/log/agent-sec` 事件存储。
投影包含操作、计数、判定、版本和错误类型，排除源代码、原始 findings、本地路径、人工原因和密钥。
CLI 响应保留完整业务数据。内置扫描限制为 2,000 个文件、总计 50 MiB、深度 32；
findings 导入限制为 2 MiB。覆盖失败会明确呈现，不会为部分内容签发完整扫描结论。

## 换钥与部署回退

```sh
sudo agent-sec-cli skill-ledger rotate-keys
agent-sec-cli skill-ledger scan --all
```

换钥先撤回已注册 Skill 的暴露，再替换密钥。此前签名随之失去信任，包括此前的 V2 签名。
重新扫描并激活以建立信任，不提供旧公钥验签回退。撤回失败时保留旧密钥，普通账本写入被阻止，
直到 root 重试或启动恢复完成。

替换已有部署前，停止旧 daemon 和 SkillFS 写入者，保留匹配的二进制、配置，并备份私有密钥和状态，
以及每个 Skill 的内容和 `.skill-meta`。使用副本开展迁移评估。
回退 V1 时先停止 V2，恢复对应的 V1 文件、密钥、配置和元数据，再启动 V1/SkillFS。
不要让 V1 读取 V2 manifest，也不要混用旧元数据与已修改的源内容。
仅保留二进制不等于可恢复状态，同时启动两个 daemon 也不是升级方式。
