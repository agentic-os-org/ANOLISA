# SkillSec V2

[English](../../en/agent-sec-core/skillsec-v2.md)

SkillSec 扫描 Skill 内容、签名扫描结果、保留可恢复的版本，并向 SkillFS 发布选定版本。
内置扫描全部在本地执行，不调用模型。Rust CLI 保留 `agent-sec-cli skill-ledger` 入口，
由一个 root daemon 执行业务操作。

本文说明 V2 核心及 Hook 适配器。[Skill Ledger 指南](skill-ledger.md)保留 V1 参考和
各宿主的匹配、策略控制规则。适配器沿用现有语言、默认策略和启用状态，通过 CLI 调用 Rust daemon。

## 安装边界

正式发布组件的常规入口为 `sudo anolisa --install-mode system install sec-core`，Alinux 用户还可使用
[安装指南](index.md#安装)中的 RPM 方式。这些入口选择已发布产物，不会选择尚未发布的 V2
迁移分支。安装正式版本不代表已获得 SkillSec V2。

V2 RPM 需要在 Linux 上按仓库配方从本分支构建。完整配方还会构建保持现状的 sandbox 和插件包，
需要 Node.js 22.14 或更新版本、Rust 1.93 或更新版本、RPM 工具、systemd RPM 宏、C 编译器、
pkg-config 和 OpenSSL 开发头文件：

```sh
./scripts/rpm-build.sh agent-sec-core-v2
sudo yum install ./scripts/rpmbuild/RPMS/x86_64/agent-sec-cli-*.rpm
```

aarch64 使用对应架构目录。本阶段安装核心 CLI 包；元包还会拉入 Agent 集成。插件包应与本次 Hook 迁移使用相同修订，
仅升级 CLI 不会更新进程中已加载的插件。
V1 和 V2 CLI 使用相同包名和可执行文件路径，安装 V2 会替换 V1。先停止 V1 写入者并保留匹配备份，
具体见下文部署回退。

仅从源码安装核心时，使用 Linux 和 Rust 1.93 或更新版本，在仓库根目录执行：

```sh
make -C src/agent-sec-core build-cli-v2
sudo make -C src/agent-sec-core install-core-v2
```

产物为 `src/agent-sec-core/target/v2/bin/agent-sec-cli` 和 `agent-sec-daemon`。
核心二进制不依赖 Python Ledger 运行时。`install-core-v2` 将两个二进制安装到 `/usr/bin`，
系统 unit 安装到 `/usr/lib/systemd/system`，初始配置以 0600 安装到 `/etc/agent-sec/skillsec.json`。
重装保留已有配置；RPM 使用 `%config(noreplace)`。安装不启用 Agent Hook；服务启动后，对已发现 Skill 的后台扫描可能初始化签名密钥。

V2 systemd 服务单元名为 `agent-sec-core.service`，可执行文件／进程名为 `agent-sec-daemon`。
V1 使用同名单元作为用户服务，V2 将其作为系统服务运行。

在运行 systemd manager 的主机上，RPM 会在首次安装和 V1 升级到 V2 时启用并启动 root
系统服务。安装前先检查配置，随后验证 daemon 是否可用：

```sh
sudo systemctl status agent-sec-core.service
agent-sec-cli skill-ledger status
```

如果 RPM 安装在没有 systemd manager 的镜像或容器中，或者服务后来被停止或禁用，请显式启动：

```sh
sudo systemctl enable --now agent-sec-core.service
```

使用系统级 `systemctl`，不带 `--user`。unit 创建运行、状态和日志目录，保持密钥和日志私有，
向 daemon 授予 `CAP_DAC_OVERRIDE`、`CAP_CHOWN`、`CAP_FOWNER` 和 `CAP_SYS_PTRACE`。
前三项支持 SkillSec 目录访问与恢复；`CAP_SYS_PTRACE` 支持进程发现读取受保护的 procfs 可执行文件和 namespace 链接。
保留 `NoNewPrivileges`、`SystemCallFilter=@system-service`、native syscall 架构、
`MemoryDenyWriteExecute` 和内核保护，同时保留 HOME、`/tmp`、系统 Skill 目录及共享挂载的访问能力，
以完成扫描、元数据发布和回滚。daemon 不具有 `CAP_SYS_ADMIN`。自定义 SkillFS 挂载须对服务可见。

评估时使用隔离部署；V1 和 V2 不得同时写同一份 Skill 元数据。
实现状态与实际 Linux 交付验收分别记录在[迁移文档](../../../../src/agent-sec-core/docs/design/SKILL_SEC_PHASE_ONE_zh.md)。

`V2_CARGO_TARGET_DIR` 指定共享 Cargo 构建缓存，`V2_BIN_BUILD_DIR` 指定二进制暂存目录。
构建和安装时传入相同的暂存目录设置。已有 Linux 构建可用以下命令检查暂存安装布局及配置保留：

```sh
bash src/agent-sec-core/tests/packaging/test-skillsec-install.sh
```

该暂存检查不会启动 systemd，也不代表真实安装验收通过。

以前台进程运行开发 daemon 时，先创建 root 所有、模式 0755 的运行目录，以及 root 所有、模式
0700 的空状态目录。通过 `agent-sec-daemon serve --socket /run/agent-sec-core/daemon.sock`
指定绝对 socket 路径，可用 `--skillsec-config /etc/agent-sec/skillsec.json` 指定配置。
进程必须以 root 运行。运行目录必须预先存在；私有状态目录不存在时由 daemon 创建。


原位 SkillFS backing 挂载要求内核支持 `open_tree`、`move_mount`，并可访问 `/proc/self/fd`。
操作不支持或被拒绝时直接失败；daemon 无需增加挂载 capability。

## Agent Hook 接入

安装配套插件或修改可信启动环境后，重启 Agent。使用自定义 socket 时，确保
`AGENT_SEC_DAEMON_SOCKET` 与 daemon 一致。加载 Skill 前先运行
`agent-sec-cli skill-ledger status`，若启动尚未建立 V2 信任，再显式扫描该 Skill；V1 密钥或 manifest
不能建立 V2 信任。

| 宿主 | 查询 | 命中 Skill 时的初始化 | 默认策略 |
| --- | --- | --- | --- |
| Codex | `check` | 幂等 `init --no-baseline` | `ask`，在 UserPromptSubmit 以警告呈现 |
| Qoder | `check` | 无 | `ask` |
| Qwen Code | `show` | 幂等 `init --no-baseline` | `ask` |
| Cosh-NG | `show` | 幂等 `init --no-baseline` | `ask` |
| OpenClaw | `show` | 幂等 `init --no-baseline` | `ask` |
| Hermes | `show` | 无 | `observe` |

四类执行初始化的适配器在每次命中调用时询问 daemon，不检查 HOME 密钥文件，不扫描 Skill，
不重置不安全的密钥，也不换钥。OpenClaw 不在插件注册时初始化，不缓存进程级就绪结果。
禁用或未命中的 Hook 不调用 CLI。Qoder、Hermes 保持只读，启动尚未完成扫描时需显式建立信任。
宿主确认仅允许当前操作继续，不写入 Ledger 人工决策。

`check` 消费者接受退出码 1 的有效风险结果，不把它当作通信失败。`show` 消费者要求
退出码 0 及有效发布摘要，再按 `message` 应用原有策略。已激活的 `warn` 版本或显式人工决策
可能没有消息。`hidden` 由 SkillFS 发布视图执行；Hook 不根据该字段单独拒绝直接文件系统访问。
未受管 Skill 不在这一受管发布路径内。

CLI 缺失、daemon 不可用、超时或响应无效时记录诊断。Codex、Cosh-NG、Qwen Code、OpenClaw、
Hermes 保持执行错误时 fail-open；Qoder 与原来一样把错误交给配置策略处理。
初始化失败时，本次调用在 `check`／`show` 之前停止，不回退 Python。诊断不复制 CLI 原始 stderr。

Cosh-NG 为整个 SkillSec Hook 预留 10 秒：初始化最多 3 秒，`show` 最多 5 秒，
另留 2 秒用于启动及结果处理。`capabilities` 环境变量视图仍显示单次查询的 5 秒预算，
不表示宿主 manifest 的整体超时。

适配器到 daemon 的测试与真实 Agent 验收分别记录。当前证据及宿主限制见
[迁移文档](../../../../src/agent-sec-core/docs/design/SKILL_SEC_PHASE_ONE_zh.md#hook-后续交付)。

## 系统配置与信任

daemon 读取 root 所有的 `/etc/agent-sec/skillsec.json`；`--skillsec-config` 可指定其他绝对路径。
父目录必须可信，配置文件不得允许组或其他用户写入，也不得是符号链接或有多个硬链接。
默认文件缺失时使用以下默认值；格式错误或权限不安全会导致启动失败。

```json
{
  "stateDir": "/var/lib/agent-sec/skillsec",
  "enableDefaultSkillDirs": true,
  "managedSkillDirs": []
}
```

默认目录自动启用，旧配置未填写 `enableDefaultSkillDirs` 时也默认开启。daemon 启动时授权：

| 位置 | 发现范围 |
| --- | --- |
| `/usr/share/anolisa/skills/*`、`/usr/local/share/anolisa/skills/*` | 直接 Skill 子目录 |
| `<home>/.local/share/anolisa/skills/*` | ANOLISA 用户安装目录 |
| `<home>/.openclaw/skills/*`、`<home>/.copilot-shell/skills/*` | 直接 Skill 子目录 |
| `<home>/.hermes/skills/**` | 递归 Skill；根目录自身有 `SKILL.md` 时也包含自身 |
| `<home>/.qoder/skills/*`、`<home>/.codex/skills/*`、`<home>/.agents/skills/*`、`<home>/.qwen/skills/*` | 直接 Skill 子目录 |

`<home>` 来自本地 `/etc/passwd` 的绝对 HOME 字段，包含 root 和服务账户。重复 HOME 合并；
非法 HOME、`/`、符号链接 HOME 及 `/dev/null` 等已存在的非目录会跳过并记录诊断。
尚不存在的 HOME 保留授权范围。账户变更后需重启 daemon。仅由 NSS 提供的账户、非标准
`XDG_DATA_HOME`、`CODEX_HOME`、`QWEN_HOME` 及项目目录需显式配置或通过已认证 SkillFS 接入；
调用者环境变量不能扩大授权范围。

`managedSkillDirs` 用于追加管理员授权的绝对路径：

| 配置项 | 授权及发现的 Skill |
| --- | --- |
| `/srv/skills/demo` | 该精确 Skill |
| `/srv/skills/*` | 直接、非隐藏 Skill 子目录 |
| `/srv/skills/**` | 递归 Skill；根目录自身有 `SKILL.md` 时也包含自身 |

用 `sudoedit /etc/agent-sec/skillsec.json` 编辑现有配置，保留其他设置，再执行
`sudo systemctl restart agent-sec-core.service` 生效。`enableDefaultSkillDirs: false` 仅关闭
默认目录；显式配置和已认证 SkillFS 挂载继续有效。显式列表为空不会关闭已启用的默认目录。
关闭范围不会删除既有 Skill 元数据或重置信任。不支持其他 glob 语法或 `~`；发现跳过隐藏后代，
不跟随符号链接。所有本地用户仍可操作全部受管 Skill，本次不引入用户 ACL。

`init`、`scan --all`、`check --all`、`status` 统一由 daemon 发现，合并默认目录与
`managedSkillDirs`。调用端显式路径在 Skill I/O 前校验；一个越界路径即以 `PermissionDenied`
拒绝整个请求。`init --no-baseline` 和单路径请求不发现目录。

启动扫描在现有 worker 中异步执行，与已到期的 SkillFS 通知交替处理，socket 无需等待扫描。
挂载内 Skill 来自认证通知和配置中的精确身份，直接经已认证 resolver 解析，不遍历 FUSE。
已有标准范围内新增 Skill 立即获得授权，
在下次聚合命令或 daemon 重启时发现；不增加目录 watcher，也不在普通 Skill 调用时自动扫描。
宿主 Hook 继续按原策略控制调用；安装本身不保证拦截所有直接文件读取。

单次默认发现最多读取 10,000 个目录项、加入 1,024 个新 Skill 身份；这些限制不截断显式配置。
单目录失败或预算耗尽时保留已完成的发现，返回含 `root`、`error` 的 `discoveryErrors`，退出码为 1，
状态不能显示健康。启动时记录发现不完整并继续扫描成功的部分。发现超时仍直接失败。

`scan` 和 `certify` 成功后，包括内容未变化的 noop，将 canonical 精确 Skill 路径记入 daemon
实际使用的配置文件。普通目录已被 `managedSkillDirs` 模式覆盖时不重复添加；SkillFS 路径始终
保存精确项，使重启后
无需遍历 FUSE 就能解析该目录。这些项同时授予普通目录访问权限：移除 SkillFS 挂载或关闭
默认目录发现，都不会撤销已保存路径；还须移除对应的 `managedSkillDirs` 项并重启 daemon。
保存失败在 `warnings` 中提示，不丢弃扫描结果、findings 或激活结果。修复配置错误后重试扫描即可补存发现信息。

启动扫描、`scan --all` 和 `init` baseline 对 `/usr/share/anolisa/skills` 或
`/usr/local/share/anolisa/skills` 的直接宿主 Skill 子目录复用只读跳过规则：daemon 无法写入
账本位置（包括只读挂载）时返回 `status: skipped`、`reasonCode: readonly_system_skill`、
`persisted: false`；启动任务不再尝试激活该项。跳过不表示安全或认证成功。
root 仍可写的 `0555` 目录正常扫描。显式扫描、用户目录写入失败、SkillFS 失败及其他存储错误
仍报错；跳过的 Skill 需放入可写的受管副本后才能建立信任。

状态目录归 root 所有，模式为 0700；`signing-key.pk8` 为 0600。第一阶段允许所有本地用户操作所有
受管 Skill，文件归属不作为用户访问 ACL。所有用户均不支持换钥。系统密钥签名的是 daemon 计算的记录，
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

`analyze` 要求路径在授权范围内，但仍只读，不初始化密钥，也不写账本。默认内置扫描器为 `code-scanner` 和 `static-scanner`。
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
`--delete-findings` 仅在认证成功且确认本地报告未变化后，以 CLI 调用者权限删除该报告。
成功删除返回 `findingsDeleted: true`；清理失败返回 `findingsDeleted: false` 和
`findingsDeleteError`，保留已提交的认证及其成功退出码。daemon 失败或响应格式错误时不会删除输入。

`show` 展示 `active`、`pending` 或 `hidden` 选择。在 `pass_warn_only` 下，符合条件的已认证
`pass` 或 `warn` 版本可以自动暴露；最新内容不安全时可保留符合条件的旧版本，或呈现待审视图。
人工决策参与版本选择。`activation.activationPending=true` 表示账本已提交，但发布尚未完成。
检查 `show`，修复所报告的原因后用 `activate` 重试。daemon 重启会恢复支持的中断发布和回滚状态，
不承诺持久化通知队列。

普通操作保留逐 Skill 的回滚恢复和校验。启动采用配置发现，无法再被发现的 Skill 不保证自动恢复；
恢复路径后可对其重试显式的已授权操作。不再用独立私有登记表补充全局恢复覆盖。

## SkillFS 绑定

SkillFS 与普通 CLI 共用公共 socket。notify 握手和消息保留现有 HMAC 合同，普通 V2 JSON 仍严格解析。
公共 socket 必须归 root 所有、模式为 0666，直接父目录归 root 所有、模式为 0755。
SkillFS 还会检查祖先目录安全性与真实内核对端 UID。权限调整不允许明文降级或绕过 HMAC。

daemon 显式配置规范路径与 backing 路径的映射：

```json
{
  "stateDir": "/var/lib/agent-sec/skillsec",
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
按 [SkillFS 运行时参考](../../../../src/skillfs/docs/security/runtime-activation-implementation-plan.md)
配置 activation 和 backing root；其中旧示例的 daemon 端点和对端身份应替换为上述 V2 配置。

两个进程必须能看到配置中的绝对路径及相同 backing 文件系统对象，因此容器部署需要匹配的共享卷路径。
同一普通非原地挂载的 canonical/live 根可以完全相等，否则必须互不包含。
规范根目录和 live 根目录不得与其他映射交叠。resolver 失败时不会回退为扫描 FUSE 视图；
目录 inode 不匹配时会拒绝操作，不会给替换后的内容签发记录。

通知按 Skill 合并，然后依次触发扫描与激活。notify 确认仅表示已接受入队。
启用 SkillFS 时，`status` 包含 `skillfs` 的 queue/running/processed/failed 计数。
通过计数、公共审计和真实 FUSE 读取，分别确认受理、处理完成和可见发布；启动时会重新调度配置中保存的精确挂载 Skill。

## 输出、审计与限制

CLI 向 stdout 输出业务 JSON。`check` 遇到 `deny`、`tampered` 或执行错误时返回 1；
已完成的 scan/certify 和完整 analyze 即使发现风险也返回 0。
analyze 覆盖不足返回 1，输入无效返回 2。传输失败写入 stderr 并返回 1。
必须同时检查 verdict、activation 字段和进程退出码。

用 `--socket` 或 `AGENT_SEC_DAEMON_SOCKET` 指定端点。SkillSec 默认超时 60 秒；
`--timeout-ms` 在服务端最大为 120 秒。超时不能证明写入是否已提交，重试修改前先查询状态和历史。
响应过大时返回 `ResponseTooLarge` 和 `operationMayHaveCommitted=true`，不会截断业务数据。

公共审计使用共享 Action Runtime 和私有 `/var/log/agent-sec` 事件存储。
投影包含操作、计数、判定、版本和错误类型，排除源代码、原始 findings、本地路径、人工原因和密钥。
CLI 响应保留完整业务数据。内置扫描限制为 2,000 个文件、总计 50 MiB、深度 32；
findings 导入限制为 2 MiB。覆盖失败会明确呈现，不会为部分内容签发完整扫描结论。

## 系统密钥与部署回退

V2 初始化一把系统签名密钥并持续复用。不支持密钥轮转：`rotate-keys` 和 `init --force-keys`
在目录发现、扫描或密钥修改前失败，直接 RPC 请求也会被拒绝。普通 `init --no-baseline` 保持
幂等，已有密钥不安全或损坏时不会自动替换。

不再维护私有 `managed-skills.json` 登记表或 `key-rotation.json` 事务。未发布实现留下的文件
不读取、不迁移、也不删除。不要通过手动删除签名密钥模拟轮转；已有签名历史依赖该密钥。

替换已有部署前，停止旧 daemon 和 SkillFS 写入者，保留匹配的二进制、配置，并备份私有密钥和状态，
以及每个 Skill 的内容和 `.skill-meta`。使用副本开展迁移评估。
回退 V1 时先停止 V2，恢复对应的 V1 文件、密钥、配置和元数据，再启动 V1/SkillFS。
不要让 V1 读取 V2 manifest，也不要混用旧元数据与已修改的源内容。
仅保留二进制不等于可恢复状态，同时启动两个 daemon 也不是升级方式。
