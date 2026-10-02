# 更新日志

[English](CHANGELOG.md)

本文档记录 SkillFS 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
项目遵循 [Semantic Versioning](https://semver.org/spec/v2.0.0.html)。

## [未发布]

## [0.5.0] - 2026-09-20

### 新增

- `skillfs mount --config mount.toml` 现在可按顺序将多个 Skill 来源合并到同一挂载。
  同名 Skill 整个目录以靠前的来源为准；多来源挂载为只读，修改配置或新增、删除
  Skill 后需重新挂载
  ([#3200](https://github.com/agentic-os-org/ANOLISA/pull/3200))。
- 新增可选的 Kubernetes 部署方案，将只读技能包复制到私有可写来源，再由 Ledger
  扫描和激活。新增的 `--read-only` 选项禁止通过 Agent 可见的挂载写入；
  缺少有效激活状态的 Skill 保持隐藏
  ([#3182](https://github.com/agentic-os-org/ANOLISA/pull/3182))。

### 变更

- Debian 和 Alibaba Cloud Linux 4 Sidecar 镜像现在会检查实际 FUSE 读取，
  连续失败后尝试重新挂载，并限制重试次数。恢复后，工作负载需要重新打开失败的句柄
  ([#2701](https://github.com/agentic-os-org/ANOLISA/pull/2701))。
- 以只读方式打开的转换后 `SKILL.md` 现在会保留打开时的内容，来源修改后，
  新打开的句柄可读到更新。容量受限的缓存可在属性查询和多次打开之间复用转换结果。
  这类句柄另有独立的挂载级额度；新句柄若会超出 64 MiB 或 1,024 个句柄的上限，
  打开操作会返回 `ENOMEM`，关闭已有的这类句柄可释放额度
  ([#3202](https://github.com/agentic-os-org/ANOLISA/pull/3202))。

## [0.4.2] - 2026-08-27

### 新增

- 新增可选的 Alibaba Cloud Linux 4 Sidecar 镜像，为现有 Debian 镜像提供可复现的
  替代方案，并通过中英文文档说明两个镜像版本的构建和运行方式
  ([#2777](https://github.com/alibaba/anolisa/pull/2777)，
  [#2787](https://github.com/alibaba/anolisa/pull/2787))。

### 修复

- 启动 reconcile 现在会通过有界退避重试 notify daemon endpoint 的临时故障，
  当 daemon 延迟启动或重启时，隐藏 Skill 可自动恢复到正确状态
  ([#2790](https://github.com/alibaba/anolisa/pull/2790))。
- Flat normal-mode mount 现在会通过已记录的物理目录解析 categorized Skill，
  write、truncate、rename 和同步操作不再因 `ENOENT` 失败
  ([#2901](https://github.com/alibaba/anolisa/pull/2901))。

## [0.4.1] - 2026-08-21

### 新增

- 新增 Kubernetes Sidecar 部署方式，由特权 SkillFS 容器向非特权工作负载提供
  FUSE view，工作负载也可读取 `skill-discover` 提供的路径
  ([#2057](https://github.com/alibaba/anolisa/pull/2057))。
- 新增可选的双向 HMAC-SHA256 认证，通过 `--trusted-peer-key-file` 和
  `--notify-auth-key-file` 保护跨容器 namespace 的 control socket 与 notify
  socket，同时保持既有宿主机认证方式不变
  ([#2449](https://github.com/alibaba/anolisa/pull/2449))。

### 变更

- Kubernetes 参考部署现在会在一次 FUSE read 失败后将 Pod 标记为 NotReady，
  在连续两次 liveness 失败后仅重启 SkillFS Sidecar，使工作负载无需重启即可恢复
  ([#2705](https://github.com/alibaba/anolisa/pull/2705))。

### 修复

- Hermes 嵌套 Skill 现在可通过 control socket 使用 `category/skill` 这类
  layout-relative 标识更新 activation 状态
  ([#2407](https://github.com/alibaba/anolisa/pull/2407))。
- Activation metadata 权限、in-place backing alias 和 control listener 启动过程
  现在会 fail closed，避免暴露 metadata、接受不安全 source 或遗留不可用 endpoint
  ([#2407](https://github.com/alibaba/anolisa/pull/2407))。
- 随包分发的 `skillfs-mount` Skill 现在使用实际存在的分析脚本名称，并准确说明
  managed mount 与可写 source 行为
  ([#1798](https://github.com/alibaba/anolisa/pull/1798))。

## [0.4.0] - 2026-07-24

### 新增

- 可配置的 read-time transform 现在默认保持 directive compilation 启用，
  并新增可选 OS adapter，内置 Ubuntu/Alinux 规则且支持外部 catalog 覆盖
  ([#1484](https://github.com/alibaba/anolisa/pull/1484))。
- 新增 authenticated live-source resolver 和 notify v2 protocol，使 Skill Ledger
  可获得规范的 flat 或 Hermes Skill identity、event kind 和 changed path，
  同时不暴露 backing-root 细节
  ([#1517](https://github.com/alibaba/anolisa/pull/1517))。

### 变更

- Agent 可见访问检查现在遵循 activated snapshot 权限，live-source 权限继续控制写入
  ([#1517](https://github.com/alibaba/anolisa/pull/1517))。

### 修复

- SLS telemetry writer 现在会动态遵循 `/etc/anolisa/.telemetry_disabled`，
  且无法检查该 gate 时会 fail closed
  ([#1584](https://github.com/alibaba/anolisa/pull/1584))。
- Hermes symlink boundary、resolver path、socket ownership 和 peer authentication
  现在会在 discovery、read 和 mutation 过程中 fail closed
  ([#1517](https://github.com/alibaba/anolisa/pull/1517))。
- Control socket 前置条件诊断现在会统一显示公开参数名 `--control-socket`
  ([#1739](https://github.com/alibaba/anolisa/pull/1739))。

## [0.3.4] - 2026-07-16

### 修复

- CLI output pipe 提前关闭并触发 panic unwind 时，SLS ops logging 现在仍会准确保留
  一条 command record。

## [0.3.3] - 2026-07-10

### 新增

- 新增 Hermes workspace layout 兼容能力。SkillFS 现在可以识别 Hermes hub marker、
  保留 management path，并同时暴露嵌套的 `category/skill/SKILL.md` Skill 与顶层 Skill。
- Hermes 嵌套 Skill 现在支持 activation state、installer lifecycle write、
  notification、audit attribution、fallback snapshot 和 hidden visibility。

### 修复

- `skillfs validate --json` 现在会在 warning 和 error 中包含 source path，
  便于自动化流程定位无效 Skill。
- FUSE teardown 现在会限制 unmount 失败后的 cleanup，避免泄漏的测试 mount
  影响后续 session。

## [0.3.2] - 2026-07-03

### 修复

- CLI SLS ops logging 现在会记录 SkillFS mount 和 runtime operation。
- Runtime metric 现在会向 SLS consumer 发送实时 delta。

## [0.3.1] - 2026-07-03

### 新增

- Managed mount supervision 现在可以恢复 stale FUSE mount，并限制重复启动时的
  recovery retry。

### 变更

- 中英文 README 现在包含 managed mount、in-place operation、security boundary
  和 troubleshooting 指南。

### 修复

- Installer 完成后，post-publish grace read 会从 source path 读取 fallback Skill file。
- `skillfs validate` 现在会在 status summary 中报告 parse failure。
- In-place authoring 现在支持新 Skill 和 pending-install ownership change。
- Managed stop 和 runtime-dir 处理现在可避免 stale ownership 与无限 recovery retry。
- PrivateTmp 下 daemon-facing backing root 会在 mount 启动前被拒绝。
- FUSE smoke cleanup 现在能更可靠地处理残留 mount 和临时 path。

## [0.3.0] - 2026-06-26

### 新增

- 新增 agent Skill directory 的 runtime security integration。SkillFS 现在可从
  `.skill-meta/activation.json` 或 `user.agent_sec.skill_ledger.activation` xattr
  读取 activation decision，并将每个 Skill 显示为 current、hidden 或可信
  fallback snapshot。
- 新增面向外部 security daemon 的 file-change notification。通过
  `--activation-mode file`、`--notify-socket`、`--activation-events-log` 和
  `--activation-reload-mode poll`，SkillFS 可报告 Skill mutation、重新加载
  activation decision，并让已打开的 file handle 保持指向原 target。
- 新增用于写入 activation 的 trusted control socket。通过 `SO_PEERCRED`、
  executable identity 和 start-time check 验证的 daemon，可通过受限 request API
  更新 activation JSON 或 activation xattr，无需通过 agent-visible mount path
  写入 `.skill-meta`。
- 新增对常见 Skill 安装流程的 installer compatibility。Staging directory、
  direct write、quiet-timeout completion 和 post-publish grace window
  允许 installer 完成 Skill 写入后，再由 SkillFS 请求 security provider 扫描并激活。
- 新增面向 security daemon 的 in-place mount 支持。Ledger backing root 会以
  private bind 方式挂载并在启动时校验，使 scanner 读取真实 source tree，
  不经过 agent-facing FUSE view。
- Skill 的 canonical identity 现在基于 directory basename。Frontmatter `name:`
  继续作为 display metadata，不再改变 SkillFS store key 或 daemon-facing Skill ID。

### 变更

- `.skill-meta/**` 对普通 agent 隐藏，只能通过 trusted metadata path 或
  control socket 访问。
- Skill mutation notify 现在使用普通 filesystem event kind，包括 `create`、
  `write`、`rename`、`unlink`、`rmdir` 和 truncate event，不再使用单独的
  install-complete protocol event。
- POSIX passthrough 扩展了 symlink、hardlink、FIFO、path-length fallback、
  open-after-unlink、xattr 和 inode consistency 行为。

### 修复

- 结合 notify-triggered reload、polling 和 activation watcher convergence，
  避免 stale activation view。
- 通过 start-time 与 file-identity validation 加固 trusted-writer 和
  trusted-peer check，防止 process reuse 与 executable replacement。
- 修复 hidden Skill、fallback snapshot、staging path 和 backing-root propagation
  周边的 installer 与 daemon visibility 问题。

## [0.2.0] - 2026-05-09

### 新增

- 新增 Skill directory 上 `write`、`create`、`mkdir`、`rename`、`unlink`、
  `rmdir` 和 `setattr(size)` operation 的 FUSE write passthrough。
- 新增 background sync worker，在写入后重新解析 `SKILL.md`，并将 entry
  `upsert` 回 `SharedSkillStore`。
- 新建 Skill directory 现在会立即可见。`mkdir` 会先插入
  `ParseStatus::Degraded` placeholder，写入 `SKILL.md` 后由 sync worker
  替换为真实 entry。
- 新增通过 `/proc/self/fd/{n}` 访问底层 source 的 in-place mount mode，
  避免 over-mount self-loop。
- 新增 integration suite `crates/skillfs-fuse/tests/write_guard_tests.rs`，
  覆盖 normal 与 in-place write path。

### 变更

- Directory name 现在是权威 store key。`rename` 后，stale frontmatter `name:`
  不会再恢复旧 key。
- 读取 `SKILL.md` 时仍返回 compiled result，raw file 只用于 write 和 parse。
- Architecture doc 重构到 `docs/specs/skillfs-spec.md`、
  `docs/specs/core-spec.md` 和 `docs/specs/fuse-spec.md`。

### 移除

- 移除 `skillfs-core` 中 workspace 相关 code path 和未使用的 workspace config
  support（commit 6d604c7）。
- 移除旧的临时 test script，仅保留 `scripts/build.sh` 和 `scripts/test.sh`。

### 修复

- CLI tracing timestamp 现在使用本地 timezone，不再使用 UTC。

## [0.1.2] - 2026-04-29

### 新增

- 新增 read-only mount write protection，`mknod`、`symlink`、`link` 和 write
  callback 都会返回 `EROFS`。

### 修复

- Parser summary 截断现在遵循 multi-byte character boundary。

## [0.1.1] - 2026-04-29

### 新增

- 新增 `docs/skills/` 下的 `skillfs-mount` agent Skill，帮助用户设置、挂载和
  卸载 SkillFS instance。

## [0.1.0] - 2026-04-25

### 新增

- 首次发布 SkillFS workspace。
- `skillfs-core` 提供 `SKILL.md` parser、带 flat 与 categorized directory layout
  的 in-memory `SkillStore`、`skillfs-views.toml` configuration、条件式
  `compiler::compile` 和 environment probing。Parser status 包括 `Ok`、
  `Degraded` 与 `Error`。
- `skillfs-fuse` 提供 read-only FUSE filesystem，在 `/skills` 暴露配置的
  default view、始终可用的 virtual `skill-discover`，并在读取时编译 `SKILL.md`。
  Skill directory 中的其他 file 会透传到 physical source。
- `skillfs` CLI 提供 `mount`、`classify`、`validate` 和 `list` subcommand。
