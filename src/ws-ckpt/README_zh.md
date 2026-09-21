# ws-checkpoint

[English](README.md)

基于 btrfs 文件系统的 AI 工作区快照管理系统，支持秒级创建检查点和回滚，专为 AI Agent 场景设计。

## 特性

- **毫秒级快照创建和回滚** — 利用 btrfs COW 特性，微秒级完成快照操作
- **守护进程架构** — 特权操作封装在 daemon 中，上层调用无需 root 权限
- **Unix Socket IPC** — Bincode 二进制协议，高效通信
- **systemd 服务化** — RPM 一键部署，开机自启
- **快照列表查询** — 支持 table/json 多格式输出
- **快照差异对比** — 查看两个快照间的文件变更
- **运行时状态监控** — 守护进程和工作区健康状态一览
- **后台自动清理** — daemon 内置 auto-cleanup 调度，可按数量或时间维度保留
- **TOML 配置热重载** — 以 `/etc/ws-ckpt/config.toml` 为唯一入口，`ws-ckpt reload` 即时生效
- **容量与数量阈值告警** — 任一工作区快照数超 1000 或文件系统使用率超 90% 时统一告警

## 项目结构

```
ws-ckpt/
├── src/                       # Rust Cargo workspace
│   ├── Cargo.toml
│   ├── config.toml.sample     # 默认配置（安装为 /etc/ws-ckpt/config.toml，%config(noreplace)，另附 .sample 参考）
│   ├── crates/
│   │   ├── common/            # 共享类型、IPC 协议编解码
│   │   ├── daemon/            # 守护进程核心逻辑
│   │   └── cli/               # 命令行客户端
│   ├── systemd/               # systemd service 文件
│   └── skills/                # OS Skills
├── docs/                      # 项目文档
├── ws-ckpt.spec.in            # RPM 规格模板
├── build-rpm.sh               # RPM 打包脚本
└── .gitignore
```

## 快速开始

### 环境要求

- Linux（推荐 Alinux 4）
- btrfs 文件系统
- Rust 1.70+

工作区根目录不能是活跃的挂载点。初始化时会把原目录改名后作为备份，而 `rename(2)` 对
「自身是挂载点」的目录会返回 `EBUSY` —— 最常见的是 in-place 模式的 SkillFS 挂载。执行
`init` 或首次 `checkpoint` 前先卸载（`skillfs stop <PATH>` 或 `fusermount3 -u <PATH>`）。

### 编译

```bash
cd src
cargo build --release
```

### 安装（RPM）

```bash
# 打包
./build-rpm.sh

# 安装
sudo rpm -ivh ~/rpmbuild/RPMS/x86_64/ws-ckpt-*.rpm

# 启动服务
sudo systemctl start ws-ckpt
```

### 基本用法

```bash
# 初始化工作区
ws-ckpt init --workspace ~/my-workspace

# 创建检查点
ws-ckpt checkpoint --workspace ~/my-workspace -s initial -m "initial version"

# 再次修改后创建检查点
ws-ckpt checkpoint --workspace ~/my-workspace -s feature -m "add feature"

# 回滚前预览将恢复的文件变更
ws-ckpt rollback --workspace ~/my-workspace -s initial --preview

# 回滚到指定快照
ws-ckpt rollback --workspace ~/my-workspace -s initial

# 删除快照
ws-ckpt delete --workspace ~/my-workspace -s feature
```

### 快照管理

```bash
# 列出工作区所有快照
ws-ckpt list --workspace ~/my-workspace

# 以 JSON 格式输出
ws-ckpt list --workspace ~/my-workspace --format json

# 查看两个快照间的差异
ws-ckpt diff --workspace ~/my-workspace --from msg1-step1 --to msg1-step2

# 查看快照与当前工作区的差异（省略 --to）
ws-ckpt diff --workspace ~/my-workspace --from msg1-step1

# 清理旧快照，保留最近 5 个
ws-ckpt cleanup --workspace ~/my-workspace --keep 5
```

### 恢复中断的初始化

恢复确认使用 daemon 解析的工作区和快照数量。若执行前恢复目标或快照集合发生变化，
需要重新确认。使用此确认协议时，请同步更新 CLI 和 daemon。

`ws-ckpt recover -w <workspace> --force` 也能恢复尚未注册的工作区：还原
`.pre-init-bak`，并保留迁移中的子卷供检查。已注册工作区恢复成功时会归档遗留备份，
避免阻断下一次 `init`。若已注册工作区的子卷被外部删除，使用
`ws-ckpt unregister -w <workspace> --force` 解除悬空注册；该操作不恢复数据，
会保留快照和备份。两条命令的 `--force` 都仅跳过交互确认。
详见[恢复说明](../../docs/user-guide/zh/runtime/ws-ckpt.md#恢复中断的初始化与悬空注册)。

### 状态与配置

配置分两层：**全局**（`/etc/ws-ckpt/config.toml`，daemon-wide 默认值）和**局部**（`/var/lib/ws-ckpt/indexes/<ws_id>/policy.toml`，per-workspace 覆盖）。`ws-ckpt config` 子命令的作用域由 scope 决定：

- 不带 scope：打印只读 overview（全局配置 + workspace 覆盖统计），修改类 flag 会被拒绝
- `-g` / `--global` 查看或修改全局配置文件
- `-w <workspace>` / `--workspace <workspace>` 查看或修改单个工作区的 `policy.toml`

```bash
# 查看系统状态
ws-ckpt status --workspace ~/my-workspace

# 查看全局配置
ws-ckpt config -g

# 启用周期性 auto-cleanup（全局）
ws-ckpt config -g --enable-auto-cleanup

# 全局保留策略（按数量或时间维度）
ws-ckpt config -g --auto-cleanup-keep 10
ws-ckpt config -g --auto-cleanup-keep 30d

# 单个工作区的覆盖（仅 auto_cleanup / auto_cleanup_keep 可 per-ws 覆盖）
ws-ckpt config -w ~/my-workspace                       # 三栏视图: effective / local / global
ws-ckpt config -w ~/my-workspace --auto-cleanup-keep 5 # 仅这个 ws 保留 5 份
ws-ckpt config -w ~/my-workspace --disable-auto-cleanup
ws-ckpt config -w ~/my-workspace --reset               # 删除局部 policy.toml,沿用全局

# 手工修改 config.toml / policy.toml 后热生效
ws-ckpt reload
```

`config -g` 写入全局配置后会校验 daemon 重载了相同的设置——若有字段不一致会以非零退出并逐项列出。Kubernetes sidecar 部署下 CLI 与 daemon 运行在不同容器，`/etc/ws-ckpt` 必须挂在两容器共享的卷上，否则全局设置无法送达 daemon。

## 命令总览

| 命令 | 说明 |
|------|------|
| `init` | 初始化工作区 |
| `checkpoint` | 创建快照检查点 |
| `rollback` | 预览或回滚到指定快照 |
| `delete` | 删除工作区或单个快照 |
| `list` | 列出工作区所有快照 |
| `diff` | 查看两个快照间的文件变更 |
| `cleanup` | 手动清理旧快照 |
| `status` | 查看守护进程和工作区状态 |
| `config` | 查看或修改 daemon 配置（写入 `/etc/ws-ckpt/config.toml`） |
| `reload` | 通知 daemon 重新加载 `config.toml` |
| `recover` | 将工作区恢复为普通目录，或还原中断初始化的备份 |
| `unregister` | 仅在子卷丢失时解除注册，保留快照和备份 |
| `plugin` | 安装/卸载 ws-ckpt Agent runtime（openclaw/hermes）插件 |

OpenClaw adapter 要求 OpenClaw >= 2026.2.13。若检测到的版本或配置能力无法安全更新工具 allowlist，安装会中止，避免留下不完整的集成。

## 组件

| 组件 | 状态 | 说明 |
|------|------|------|
| Daemon | 基本完成 | 在 root 权限在运行，实际操作文件系统 |
| CLI | 基本完成 | init / checkpoint / rollback / delete / list / diff / cleanup / status / config / plugin |
| plugin | 基本完成 | OpenClaw / Hermes 插件，自动 checkpoint/rollback |
| skills | 初版 | `src/skills/ws-ckpt/SKILL.md` 初版完成，目前仅适配openclaw |

## OpenClaw Skill

ws-ckpt 提供了一个配套 [OpenClaw](https://github.com/alibaba/anolisa) 的 skill 定义，位于 `src/skills/ws-ckpt/SKILL.md`。

如需从源码手动安装，可将目录复制到 OpenClaw skill 路径下：

```bash
cp -r src/skills/ws-ckpt <your-openclaw-skills-dir>/ws-ckpt
```

## 开发

```bash
# 运行测试
cd src
cargo test --workspace

# 代码检查
cargo clippy --workspace -- -D warnings
```

## 文档

- [使用文档](../../docs/user-guide/zh/runtime/ws-ckpt.md)
- [RPM 打包](docs/RPM-PACKAGING.md)

## License

Licensed under the Apache License, Version 2.0; see [LICENSE](../../LICENSE).

ws-ckpt interacts with the Linux kernel btrfs filesystem (GPL-2.0) solely through the
public system call interface, and invokes `btrfs-progs` (GPL-2.0) exclusively as
independent executable processes. No source code, object code, or header files from any
GPL-licensed component are incorporated, statically linked, or dynamically linked into
ws-ckpt. Such interaction constitutes an independent and separate work within the meaning
of GPL-2.0 Section 2 ("mere aggregation") and imposes no copyleft obligation on ws-ckpt.
