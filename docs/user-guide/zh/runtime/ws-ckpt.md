# 工作区快照（ws-ckpt）

ws-ckpt 为 AI Agent 提供毫秒级工作区快照和回滚能力。它利用文件系统 COW（Copy-on-Write）技术创建即时快照，支持安全实验和快速恢复。

---

## 概述

AI Agent 修改代码、配置或数据文件时，误操作代价高昂。ws-ckpt 允许 Agent（和用户）：

- 在风险操作前创建即时快照
- 毫秒内回滚到任意历史检查点
- 比较检查点之间的差异
- 通过插件集成自动创建检查点

---

## 前置条件

- Linux（x86_64 或 aarch64）
- 工作区所在卷使用 btrfs 文件系统（用于原生 COW 快照），或任意文件系统（ws-ckpt 会自动创建 btrfs loop image）
- Agent 运行时：OpenClaw（>= 2026.2.13）或 Hermes（Plugin 模式）

---

## 安装

### 方式一：anolisa CLI（推荐）

```bash
sudo anolisa --install-mode system install ws-ckpt
```

### 方式二：YUM（Alinux，需配置 ANOLISA YUM 源）

```bash
sudo yum install ws-ckpt
```

### 方式三：源码编译（开发者）

```bash
cd src/ws-ckpt && make build
```

---

## 插件安装

为你的 Agent 运行时安装 ws-ckpt 插件：

```bash
# OpenClaw
ws-ckpt plugin install --runtime openclaw

# Hermes
ws-ckpt plugin install --runtime hermes

# 卸载
ws-ckpt plugin uninstall --runtime openclaw
```

`plugin install` 会先执行 detect 脚本检查前置条件（exit 2 = 缺前置依赖，中止；exit 1 = 未安装但可安装，继续），通过后再执行 install 脚本。脚本位于 `/usr/share/anolisa/adapters/ws-ckpt/<runtime>/`。

OpenClaw 插件要求 OpenClaw >= 2026.2.13；这是 config 写入路径首次避免固化 runtime defaults，并在写盘前恢复未修改 `${VAR}` 引用的版本。OpenClaw >= 2026.9.1 使用条件配置写入；2026.2.13 到 <2026.9.1 的版本仅在根配置不含 `$include` 时使用普通 JSON 写入，否则安装会在安装插件前中止，并要求升级 OpenClaw。版本无法解析、缺少写入能力或 allowlist 更新失败时，安装同样会中止，避免留下不完整的集成。卸载仍采用 best-effort：本地插件文件会被删除；若不存在安全的配置写入路径，则跳过 plugin unregister 和 allowlist 清理并输出告警。

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `ws-ckpt init -w <workspace>` | 初始化工作区 |
| `ws-ckpt checkpoint -w <workspace> -s <snapshot-id> -m <message> [--metadata <json>]` | 创建新检查点 |
| `ws-ckpt rollback -w <workspace> -s <snapshot> [--preview]` | 回滚到指定检查点 |
| `ws-ckpt rollback -w <workspace> -n <num-ancestors>` | 回滚 N 个祖先版本 |
| `ws-ckpt list [-w <workspace>] [--orphans] [--format table\|json]` | 列出所有检查点 |
| `ws-ckpt diff -w <workspace> -f <from> [-t <to>]` | 显示检查点间差异 |
| `ws-ckpt delete [-w <workspace>] -s <complete-id> [--force]` | 删除指定检查点 |
| `ws-ckpt status [-w <workspace>] [--format table\|json]` | 查看工作区状态 |
| `ws-ckpt cleanup -w <workspace> [--keep 20]` | 清理旧检查点 |
| `ws-ckpt config [-g \| -w <workspace>] [--enable-auto-cleanup] [--auto-cleanup-keep <N\|Nd>]` | 查看/编辑配置 |
| `ws-ckpt plugin install --runtime openclaw\|hermes` | 安装运行时插件 |
| `ws-ckpt plugin uninstall --runtime openclaw\|hermes` | 卸载运行时插件 |
| `ws-ckpt recover [-w <workspace> \| --all] [--force]` | 将工作区恢复为普通目录，或还原中断初始化的备份 |
| `ws-ckpt unregister -w <workspace> [--force]` | 仅在子卷丢失时解除注册；不恢复数据 |
| `ws-ckpt reload` | 重载 daemon 配置 |
| `ws-ckpt daemon [--mount-path ...] [--socket ...] [--log-level ...]` | 启动 daemon 进程 |

### 示例

```bash
# 初始化工作区
ws-ckpt init -w /home/user/projects/my-project

# 创建检查点
ws-ckpt checkpoint -w /home/user/projects/my-project -s snap-001 -m "before refactor"

# 列出检查点
ws-ckpt list -w /home/user/projects/my-project

# 比较两个快照的差异
ws-ckpt diff -w /home/user/projects/my-project -f snap-001 -t snap-002

# 回滚到指定检查点
ws-ckpt rollback -w /home/user/projects/my-project -s snap-001

# 预览回滚（不实际执行）
ws-ckpt rollback -w /home/user/projects/my-project -s snap-001 --preview

# 清理旧检查点，保留最近 20 个
ws-ckpt cleanup -w /home/user/projects/my-project --keep 20

# 为工作区启用自动清理
ws-ckpt config -w /home/user/projects/my-project --enable-auto-cleanup --auto-cleanup-keep 7d
```

### cleanup 中断后的快照恢复

重启后，daemon 会删除普通、未 pinned 快照的缺失记录，并修复父子链关系。
Pinned 快照和 guarded evidence 保留 unavailable 状态；`list` 显示缺失标记，
`diff` 返回 `SnapshotNotFound`。当目标 subvolume 已不存在时，`delete` 同样返回
`SnapshotNotFound`，删除普通记录并保留 guarded evidence。Pinned 记录仍需要
`--force`。

磁盘上存在而索引中缺失的快照目录会登记为 **pinned 的恢复孤儿**。
原来的保护状态、消息、metadata、创建时间和祖先关系均未知；显示的时间为恢复时间，
并非原始创建时间。Count/Age 清理会跳过这些快照，创建新 checkpoint 和再次重启后
保护仍然有效。若 ID 仍有 guarded receipt，则不会将身份未经验证的孤儿关联到该 ID。

可以单独查询恢复孤儿，将其与普通 pinned 快照区分；确认不再需要后再显式删除：

```bash
ws-ckpt list -w "/path/to/workspace" --orphans --format json
ws-ckpt delete -w "/path/to/workspace" -s "complete-snapshot-id" --force
```

省略 `-w` 可查询所有工作区的恢复孤儿。JSON 包含完整 `id`、`workspace` 和
`meta.pinned`，表格同样显示保护状态。恢复孤儿在显式删除前会持续占用磁盘空间。

**delete 只接受完整 ID**，带或不带 `-w` 都不会退回短前缀匹配，`--force` 也不例外。
若多个工作区存在相同的完整 ID，必须指定 `-w`。其他命令保留原有前缀匹配能力。
使用 `--orphans` 和精确删除语义时，请同步升级 CLI 和 daemon。

恢复只对账已完成的删除，不会继续执行中断前尚未完成的清理计划；
需要再次运行 `cleanup` 完成普通快照的保留清理。

### 恢复中断的初始化与悬空注册

恢复确认信息由 daemon 提供：别名、父目录软链接和 `..` 会解析到同一个已注册工作区。
提示显示其注册路径和恢复时将删除的快照目录数量，包括尚未记录在索引中的快照。
执行使用已解析的工作区 ID，并再次检查目标和删除集合；若发生变化，请重新运行命令
并确认新的预览。`--force` 跳过交互提示，但仍执行预览与校验。请同步更新 CLI 和
daemon；不支持恢复预览的旧 daemon 无法执行此 CLI 流程。

正常情况下，`recover` 将已注册工作区复制回普通目录，再删除受管子卷和快照。
如果初始化被中断，按以下状态处理：

| 状态 | 处理方式 |
|------|----------|
| 只有 `<workspace>.pre-init-bak`，没有历史子卷 | 重新运行 `init`；它会先还原备份，再初始化。原路径必须不存在、为空目录或为 symlink。 |
| 未注册，但备份和迁移中的子卷都存在 | 运行 `recover -w <workspace>`，还原完整备份，保留可能不完整或包含较新内容的子卷及快照，并输出位置供检查。随后可以重新 `init`。 |
| 已注册，但 live 子卷已丢失 | `recover` 会明确报告缺失；运行 `unregister -w <workspace>` 解除悬空注册，保留仍存在的恢复资料。 |

恢复备份不会覆盖非空目录或普通文件。若原路径已有其他数据，先检查并移走这些数据。
已注册工作区恢复成功后若仍有 `.pre-init-bak`，会将其归档为
`.pre-init-bak.recovered`（位置已占用时追加数字后缀），保留内容并输出位置，
避免阻断下一次 `init`。若归档失败，提示会列出备份位置，需要先检查并移走备份再初始化。
`recover --all` 只处理已注册工作区；未注册的中断初始化必须通过原工作区路径指定。

```bash
ws-ckpt recover -w /path/to/workspace --force
```

`unregister` 接受已注册的路径或 workspace ID；当 live 子卷仍存在时会拒绝操作。
它不会恢复数据，也不会删除快照或 `.pre-init-bak`。原索引移至
`<state-dir>/indexes/<ws_id>.unregistered`，避免下一次初始化继承旧元数据；若该位置
已被占用，命令会拒绝覆盖。命令会列出实际存在的保留位置，供后续手工恢复或清理使用。
若 daemon 在归档索引后、持久化注销前退出，启动时会先还原该索引及其策略，
再加载仍在注册记录中的工作区。
仅指向缺失子卷的受管 symlink 会被移除，原路径上的其他文件或目录会保留。
若快照或索引归档仍占用旧 ID，下一次初始化会分配新的 ID。

```bash
ws-ckpt unregister -w /path/to/workspace --force
mkdir -p /path/to/workspace
ws-ckpt init -w /path/to/workspace
```

两条命令默认要求交互确认；`--force` 仅跳过确认，不允许覆盖冲突数据，也不会让
`unregister` 接受仍有 live 子卷的工作区。

### diff 输出标记

| 标记 | 含义 | 颜色 |
|------|------|------|
| `+` | 新增文件/目录（Added） | 绿色 |
| `-` | 删除文件/目录（Deleted） | 红色 |
| `M` | 内容修改（Modified） | 黄色 |
| `R` | 重命名（Renamed） | 青色 |

> diff 内置智能解析器，自动将 btrfs 底层的临时 inode 引用（如 `o261-118-0`）解析为真实文件路径，并对同一文件的多个操作去重合并。预览回滚（`rollback --preview`）使用相同的标记含义。

---

## 配置

### Daemon 配置

daemon 配置文件位于 `/etc/ws-ckpt/config.toml`，为系统级 daemon 进程配置。

不存在用户侧全局配置文件。自动检查点和清理行为通过各插件配置控制：

### OpenClaw 插件配置

```json
// ~/.openclaw/ws-ckpt.json
{
  "autoCheckpoint": true,
  "workspace": "/home/user/projects/my-project"
}
```

### Hermes 插件配置

```bash
hermes config set plugins.ws-ckpt.workspace /home/user/projects/my-project
```

### CLI 配置

配置分两层：**全局**（`/etc/ws-ckpt/config.toml`，daemon-wide 默认值）与**局部**（per-workspace `policy.toml` 覆盖）。`ws-ckpt config` 不带 scope 时打印只读概览；`-g` 查看/修改全局；`-w` 仅可覆盖 `auto_cleanup` 与 `auto_cleanup_keep`，其余字段（interval / image / health check）为 daemon-wide，只能通过 `-g` 设置；`-w <workspace> --reset` 删除该工作区的覆盖，回退到沿用全局。

```bash
# 启用自动清理，保留 7 天内的检查点
ws-ckpt config -w /home/user/projects/my-project --enable-auto-cleanup --auto-cleanup-keep 7d

# 全局配置
ws-ckpt config -g --enable-auto-cleanup --auto-cleanup-keep 20
```

全局配置文件的读取方是 daemon，因此 `config -g` 不止于写文件：写入 `/etc/ws-ckpt/config.toml` 后，它会请求 daemon 重载，并把 daemon 实际加载到的配置与刚写入的逐项比对。只要有任何不一致，命令会列出每个差异字段并以非零退出，而不是报成功。

这一点在 Kubernetes sidecar 部署中尤其重要：CLI（app 容器）与 daemon 运行在不同容器、各自独立的文件系统里。需把 `/etc/ws-ckpt` 挂到两容器共享的卷上（`emptyDir` 即可）；否则每一条 `config -g` 设置都会静默停留在 daemon 的内置默认值。随附的 `k8s-sidecar-example.yaml` 已经接好了这个共享卷。

---

## 重要注意事项

> **警告**：ws-ckpt 配置的工作区路径**不能**是：
> - 根路径（`/`）
> - daemon mount_path 内部的路径
> - 活跃的挂载点（见下文）
> - Agent 启动目录或其父目录（在 plugin 层校验）
>
> 这些约束由 daemon 代码强制执行。使用无效路径将被拒绝。

### 工作区根目录不能是挂载点

初始化工作区时会把原目录改名后作为备份，而 `rename(2)` 对「自身是挂载点」的目录会返回
`EBUSY`。这与文件系统类型无关，不只是 FUSE。

最常见的情况是 in-place 模式的 SkillFS 挂载 —— 此时 source 和 mountpoint 是同一个目录。
先卸载再操作：

```bash
skillfs stop /path/to/workspace      # in-place SkillFS 挂载
fusermount3 -u /path/to/workspace    # 其他 FUSE 挂载
```

该约束作用于 `init`，以及在未纳管路径上首次执行的 `checkpoint`（会自动初始化）。工作区
初始化完成之后，后续的 `checkpoint`、`rollback`、`list`、`diff` 都不受影响。

被拒绝的只有工作区根目录本身。工作区**内部**的嵌套挂载不会阻止 `init`，但结果通常不是
你想要的：挂载会留在 `init` 改名移走的备份目录上，新工作区里只有挂载内容的普通副本 ——
后续写入落在副本上而不是挂载的文件系统里，两边会静默分叉。初始化前先卸载嵌套挂载，
或让挂载点保持在工作区目录树之外。

### 回滚 OpenClaw 工作区可能触发安全阻断

OpenClaw 会把工作区 setup 状态记录在工作区之外。恢复较旧快照后，工作区内容可能与
OpenClaw 近期状态不一致，此时 OpenClaw 会停止运行，而不是重新种入文件：

```
WorkspaceVanishedError: OpenClaw workspace appears to have disappeared ...
Refusing to reseed BOOTSTRAP.md over a recently attested workspace.
```

一次 agent 对话成功后，建议立即创建并记录一个基线 checkpoint：

```bash
ws-ckpt checkpoint -w /path/to/workspace
```

相比首次成功对话之前的快照，恢复时应优先选择这个 checkpoint，或之后已经用 agent 验证过的
checkpoint。OpenClaw 会把工作区内容与版本相关的 setup 状态组合判断。任何单个文件
（包括 BOOTSTRAP.md）的存在都不能单独证明快照一定会被接受。恢复后，运行使用该工作区的
OpenClaw agent，确认不再出现 `WorkspaceVanishedError` 即可；后续 provider、凭据或 runtime
错误应单独处理。

以下恢复步骤只适用于已经复现的版本。其他 OpenClaw 版本应使用该版本自带的恢复说明，不要
根据相邻版本推断。

- OpenClaw 2026.7.1（使用文件存储 attestation）：删除该工作区的
  attestation 文件。首先从 agent 的启动命令、service 或部署配置中取得该 agent 进程实际
  使用的 effective home 与状态目录。不要根据恢复 shell 的 `$HOME` 猜测，也不要通过扫描
  `.openclaw*` 目录推断。例如，以
  `OPENCLAW_HOME=/srv/oc openclaw --profile team ...` 启动的 agent 通常使用 `/srv/oc` 和
  `/srv/oc/.openclaw-team`；显式配置的 `OPENCLAW_STATE_DIR` 优先级更高。

  以下命令会提示输入这三个准确的绝对路径，只检查已验证版本使用的三个位置，并仅删除带有
  OpenClaw attestation marker 的文件；如果没有删除任何有效记录，命令会失败退出：

  ```bash
  IFS= read -r -p 'Workspace path used by the agent: ' WS
  IFS= read -r -p 'Effective OpenClaw home: ' OC_HOME
  IFS= read -r -p 'Effective OpenClaw state directory: ' OC_STATE_DIR
  node - "$WS" "$OC_HOME" "$OC_STATE_DIR" <<'NODE'
  const crypto = require("crypto");
  const fs = require("fs");
  const path = require("path");

  const HEADER = "openclaw-workspace-attestation:v1\n";
  const MAX_BYTES = 2048;
  const [workspaceInput, homeInput, stateDirInput] = process.argv.slice(2);
  const inputs = [workspaceInput, homeInput, stateDirInput];
  if (inputs.some((value) => !value || !path.isAbsolute(value))) {
    console.error("Workspace, effective home, and state directory must be absolute paths.");
    process.exit(1);
  }

  const workspace = path.resolve(workspaceInput);
  const home = path.resolve(homeInput);
  const stateDir = path.resolve(stateDirInput);
  const hash = crypto.createHash("sha256").update(workspace).digest("hex");
  const targets = [...new Set([
    path.join(stateDir, "workspace-attestations", `${hash}.attested`),
    path.join(home, ".clawdbot", "workspace-attestations", `${hash}.attested`),
    `${workspace}.attested`,
  ])];

  let removed = 0;
  let failed = false;
  for (const target of targets) {
    let stat;
    try {
      stat = fs.lstatSync(target);
    } catch (error) {
      if (error.code === "ENOENT") {
        console.log(`not present: ${target}`);
      } else {
        failed = true;
        console.error(`FAILED: ${target} (${error.message})`);
      }
      continue;
    }

    if (!stat.isFile() || stat.size > MAX_BYTES) {
      console.log(`skipped: ${target} (not an OpenClaw attestation file)`);
      continue;
    }

    let content;
    try {
      content = fs.readFileSync(target, "utf8");
    } catch (error) {
      failed = true;
      console.error(`FAILED: ${target} (${error.message})`);
      continue;
    }
    if (!content.startsWith(HEADER)) {
      console.log(`skipped: ${target} (not an OpenClaw attestation file)`);
      continue;
    }

    try {
      fs.unlinkSync(target);
      removed += 1;
      console.log(`removed: ${target}`);
    } catch (error) {
      failed = true;
      console.error(`FAILED: ${target} (${error.message})`);
    }
  }
  if (failed || removed === 0) {
    if (removed === 0) {
      console.error("No valid attestation record was removed; verify all three input paths.");
    }
    process.exit(1);
  }
  NODE
  ```

  删除后，运行使用该工作区的 OpenClaw agent。如果仍被阻断，应核对三个输入值，而不是继续
  删除其他状态目录。

- OpenClaw 2026.8.1（使用 SQLite 存储 attestation）：不要修改 SQLite 数据库，也不要依赖
  其私有 schema。回滚到一次成功 agent 对话后创建的 checkpoint，然后重试 agent：

  ```bash
  ws-ckpt rollback -w /path/to/workspace -s <known-good-snapshot-id>
  ```

  如果没有已知可用的 checkpoint，目前没有能够立即、无破坏地只解除这个工作区阻断的命令。
  错误信息还会提到 `openclaw reset --scope full`，但该命令会删除所有 agent workspace 和
  整个 OpenClaw 状态目录，包括凭据、会话及已安装的 plugin，因此不建议用于本场景。

---

## 自然语言用法（Agent 驱动）

安装 ws-ckpt skill 后，Agent 可通过自然语言操作检查点：

| 意图 | 示例表达 |
|------|----------|
| 创建检查点 | "保存工作区"、"开始前先做个快照" |
| 回滚 | "撤销所有修改"、"恢复到上一个好的状态" |
| 列出检查点 | "显示所有保存的状态"、"列出我的检查点" |
| 差异对比 | "上次保存后改了什么？" |

---

## 常见问题

**Q：文件系统不是 btrfs 怎么办？**
A：ws-ckpt 会在宿主文件系统上创建 btrfs loop image 并进行 loop mount，在任意文件系统类型上提供完整的 COW 快照功能。

**Q：能同时管理多个工作区吗？**
A：可以。每条命令通过 `-w` 指定工作区路径，或通过插件配置管理多个工作区。

**Q：检查点占用多少磁盘空间？**
A：使用 btrfs COW 时，仅存储变更的块。每个检查点的典型开销 < 工作区大小的 5%。
