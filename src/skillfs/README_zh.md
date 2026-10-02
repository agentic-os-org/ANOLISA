# SkillFS

[English](README.md) | **中文**

SkillFS 是面向 agent skill 的本地 FUSE 文件系统。它解析 `SKILL.md`，按 view
组织技能，并通过挂载后的文件系统暴露编译后的 `SKILL.md`，同时让普通 skill
文件继续由真实 source 树承载。

[![Rust](https://img.shields.io/badge/Rust-1.86+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

## 能力

- 解析标准 `SKILL.md` 文件。
- 支持平铺 skill 目录和分类目录布局。
- 通过 `skillfs-views.toml` 选择默认 view 和 secondary views。
- 在挂载后的 agent 视图中直接显示默认 view 的 skills。
- 始终暴露虚拟 `skill-discover` skill，让 agent 能发现 secondary views 中的
  skills，并打开其中提供的路径。
- 读取 `SKILL.md` 时执行条件块编译和命令归一化。
- 将普通文件和子目录透传到底层物理 source 树。
- 支持 normal mount 和 in-place mount。
- 支持挂载期间的物理写透传；`SKILL.md` 变化会重新解析并更新内存 store。
- 为普通 passthrough 路径提供 Linux POSIX 兼容基线：fd-backed I/O、
  create/mkdir mode 处理、长路径 fallback、open-after-unlink 句柄、受限
  symlink/hardlink 策略、FIFO 创建，以及保守的 `user.*` xattr 透传。
- 提供可选外部安全集成面：decision-command activation、activation 文件/xattr
  消费、notify socket 事件、protocol JSONL 事件、active mapping reload、
  startup reconcile、可信写进程身份校验、可信 control socket 写入，以及
  managed mount recovery。

## 行为矩阵

| 操作 | normal mount | in-place mount | 说明 |
| --- | --- | --- | --- |
| `readdir` | 虚拟视图 | 虚拟视图 | 可见性由 views 和 store 决定。 |
| 读 `SKILL.md` | 编译后内容 | 编译后内容 | 使用 `compiler::compile`。 |
| 读其他文件 | 透传 | 透传 | 读取物理 source 文件。 |
| 写 `SKILL.md` | 透传 + store reparse | 透传 + store reparse | 目录名是 store 权威 key。 |
| `create` 普通文件 | 透传 | 透传 | 不更新 store。 |
| `mkdir` skill 目录 | 立即可见 | 立即可见 | 异步 reparse 前先插入 degraded placeholder。 |
| `rename` skill 目录 | 可见性立即切换 | 可见性立即切换 | 旧名无空窗移除。 |
| `unlink` `SKILL.md` | 从 store 移除 | 从 store 移除 | skill 从虚拟视图消失。 |
| `rmdir` skill 目录 | 从 store 移除 | 从 store 移除 | 同时清理 inode 映射。 |
| `setattr(size)` | 支持 truncate | 支持 truncate | 其他 metadata 操作在允许时保守透传。 |
| `symlink` | 受限透传 | 受限透传 | 仅允许同 skill 内相对目标。 |
| `link` | 受限透传 | 受限透传 | 仅允许同 skill 内普通文件。 |
| `mkfifo` | 透传 | 透传 | 仅 FIFO；device/socket node 拒绝。 |
| `xattr user.*` | 透传 | 透传 | 仅普通 passthrough 路径。 |

## 范围

- 公开 CLI 命令是 `mount`、`stop`、`classify`、`validate` 和 `list`。
- skill 可见性由 `skillfs-views.toml` 控制。
- FUSE 支持挂载期间写透传，但只有 `SKILL.md` 变化会触发 store 同步。
- 权威 skill key 是目录名，不是 rename 后可能滞后的 frontmatter `name:`。
- in-place mount 会 over-mount source 目录。受控的 skill 写入可以通过
  SkillFS 执行，但会 rename 或替换挂载目录本身的工具，例如 workspace
  checkpoint/init/rollback 工具，必须在 mount 前或 unmount 后执行。

## 架构

```text
physical skills dir
  └─ skill-name/SKILL.md
            │
            ▼
    skillfs-core
      - parser
      - store
      - views
      - compiler
            │
            ▼
      skillfs-fuse
            │
            ▼
     mounted /skills view
```

## 写路径与一致性

SkillFS 是一个混合文件系统：虚拟目录视图 + 物理写透传。

- `readdir` 仍由虚拟 view 控制。
- 读取 `SKILL.md` 默认返回编译后内容；当 directive stage 关闭且无其他 stage 时，
  返回选定目标的 raw 内容。
- 其他文件直接读写底层文件系统。
- 写入、创建或 rename 后写入 `SKILL.md` 会重新解析文件并更新
  `SharedSkillStore`。
- `mkdir` 和 skill 目录 `rename` 走立即一致路径，先同步更新 store，之后由
  异步 reparse 用真实条目替换 placeholder。
- in-place mount 使用 `/proc/self/fd/{n}` 访问底层 source，避免递归进入
  自己的 FUSE over-mount。

## 快速开始

### 构建

```bash
cargo build --release
```

### 常用命令

```bash
# 验证 skills
cargo run -p skillfs -- validate /path/to/skills

# 列出 skills
cargo run -p skillfs -- list /path/to/skills

# 生成或查看 skillfs-views.toml
cargo run -p skillfs -- classify /path/to/skills

# 挂载 FUSE 文件系统
cargo run -p skillfs -- mount /path/to/skills /path/to/mountpoint

# opt-in managed mount：由脱离调用方的 supervisor 保持挂载存活
cargo run -p skillfs -- mount /path/to/skills /path/to/mountpoint --managed

# 停止 managed mount 并清除 desired mounted state
cargo run -p skillfs -- stop /path/to/mountpoint
```

### 挂载配置

使用 `skillfs mount --config /path/to/skillfs-mount.toml`，通过 `mountpoint` 和
有序 `sources` 数组聚合目录。同名 skill 整体选用较早的来源，多源挂载只读。
路径必须为绝对路径。TOML 示例、支持的参数和 OpenClaw 接入限制见
[配置参考](../../docs/user-guide/zh/runtime/skillfs.md#挂载配置)。

### Managed Mount 模式

默认 `mount`（包括 `--foreground`）保持原有前台行为：进程阻塞，
`SIGTERM` 或 `Ctrl+C` 会干净卸载。如果启动它的进程（例如 gateway）重启并终止
子进程，挂载也会随之消失。

`--managed` 是 opt-in，用于需要跨 gateway restart 保持挂载的场景：

- client 写入 managed state，使用 `setsid` 在独立 session 中启动 detached
  supervisor，等待挂载 ready 后返回。
- supervisor 用相同的 source、mountpoint、config、security、audit、
  activation、trusted-writer、control socket 和 logging 选项启动前台 FUSE
  worker。
- 如果 worker 在 desired state 仍为 `mounted` 时意外退出，supervisor 会在有界
  backoff 后重新挂载。
- 只有 `skillfs stop <MOUNTPOINT>` 会清除 desired mounted state，终止
  supervisor/worker 并卸载。`stop` 是幂等的，对已经卸载的路径重复执行也是安全的。
- 如果 supervisor 被 `kill -9` 强制终止，可能留下仍在服务但无人监控的 orphan
  worker。重新启动 `mount --managed` 前，先执行 `skillfs stop <MOUNTPOINT>` 清理
  残留 state、process 和 mount。

managed state 存放在按用户隔离的运行时目录下：优先
`$XDG_RUNTIME_DIR/skillfs/`，其次 `/run/user/<uid>/skillfs/`，两者都不可用时
回退到 `/tmp/skillfs-<uid>/`。实例 id 从规范化后的 mountpoint 派生，因此
`mount` 与 `stop` 对同一挂载点始终指向同一实例。

### In-place Mount 与 Workspace Snapshot

SkillFS 使用 in-place mount 时，会替换挂载目录本身的工具需要在 mount 前或
unmount 后执行。例如，`ws-ckpt checkpoint -w <MOUNTPOINT>` 如果作用在活跃的
SkillFS mountpoint 上，可能会因为 `Device or resource busy` 失败。

通过 SkillFS 进行的写入，包括 install/update/remove skills，挂载期间仍然支持。

## `skillfs-views.toml`

skill 选择由 `skillfs-views.toml` 控制：

```toml
[[view]]
name = "major"
default = true
description = "Skills shown directly in /skills"
skills = ["github", "notion", "slack"]

[[view]]
name = "other"
default = false
description = "Skills exposed via skill-discover"
skills = ["apple-notes", "blogwatcher"]
```

挂载后：

- `/skills` 显示 default view 中的 skills。
- `skill-discover/SKILL.md` 列出 secondary views 中的 skills 及其
  可读 `source_path`。

## `SKILL.md` 格式

```markdown
---
name: my-skill
description: Brief description
version: 1.0.0
tags: [tooling, example]
enabled: true
---

# My Skill

Detailed instructions.

## Parameters

- `input` (string, required): Input value
- `options` (object, optional): Extra options

## Returns

- `result` (string, required): Result value
```

## 条件编译

FUSE 读取 `SKILL.md` 时，SkillFS 会执行 `compiler::compile`，支持：

- `<!-- @if os == darwin -->`
- `<!-- @if has_command("uv") -->`
- `<!-- @else -->`
- `<!-- @endif -->`

没有条件块时，SkillFS 也会执行少量启发式命令归一化，例如：

- `pip install` -> `uv pip install`
- `python -m venv` -> `uv venv`
- `npm install` -> `pnpm install` / `yarn install`

## 读时转换流水线

在解析出激活目标（live source、trusted snapshot 或 hidden）之后，`SKILL.md`
读取会经过一条固定顺序的转换流水线：

```text
激活目标 -> 读取选中字节 -> [directive/compiler stage]
  -> [可选 os_adapter stage] -> Agent 可见字节
```

两个 stage 都是可选且相互独立的，固定顺序为 `directive -> os_adapter`：

- **directive** stage 即上文的条件编译，**默认开启**；开启时始终第一个执行，因此
  现有挂载与旧版本逐字节一致。可通过 `[transforms.directive] enabled = false`
  关闭（见下文）。
- **os_adapter** stage 为 opt-in，仅作用于 `SKILL.md`，第二个执行。它在
  Ubuntu/Debian 与 Alinux/Anolis 风格之间改写发行版相关的字面量（包管理器、
  `-dev`/`-devel` 包名、service 单元名、文件系统路径）。

任意组合都合法：两个 stage（默认 + 启用适配器）、仅 directive（默认）、仅
适配器（关闭 directive），或两者都关闭——空流水线原样返回选中的字节。初始化
诊断会报告实际启用的 stage 列表，包括空列表。

| Directive | OS adapter | Agent 可见的 `SKILL.md` |
| --- | --- | --- |
| 开启（默认） | 关闭（默认） | 旧版 compiler 输出 |
| 开启 | 开启 | compiler 输出，再执行 OS 适配 |
| 关闭 | 开启 | 对选中的原始字节执行 OS 适配 |
| 关闭 | 关闭 | 选中的原始字节 |

转换绝不修改源文件、snapshot、激活元数据或规则文件。Hidden 技能保持 `ENOENT`
且不进入流水线；snapshot 读取只转换 snapshot，绝不回退到 live source。flat 与
Hermes nested `SKILL.md` 使用同一条流水线。只有 `SKILL.md` 会被适配——其他
Markdown、shell、Python 与配置文件原样透传。

### 关闭 directive stage

directive/compiler stage 默认开启，除非显式关闭：

```toml
[transforms.directive]
enabled = false
```

当 `[transforms.directive]` 段不存在时，directive 编译保持开启，因此现有配置不受
影响。关闭它只影响 compiler stage；OS 适配器仍是独立的 opt-in 项。

### OS 适配器配置

OS 适配器复用现有的 `--config <PATH>` TOML 文件（不新增 CLI flag），默认关闭，
必须显式启用。启用且未设置 `rules_path` 时使用内置规则目录：

```toml
# /etc/skillfs/skillfs-security.toml
[transforms.directive]
enabled = true

[transforms.os_adapter]
enabled = true
target_os = "alinux" # auto | ubuntu | alinux
# rules_path = "/etc/skillfs/ubuntu-alinux.custom.yaml"
```

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --config /etc/skillfs/skillfs-security.toml
```

SkillFS **内置一份 311 条规则的 Ubuntu/Alinux 规则目录**，通过 `include_bytes!`
从仓库资产嵌入二进制，因此源码构建、RPM 与容器中无需额外文件即可工作。其中 257 条
为 `auto_apply: always`，54 条为 `auto_apply: never`；编译后面向 Alinux 有 223 条
active substitution，面向 Ubuntu 有 192 条。适配器仍是 opt-in。

- `target_os = "auto"` 在挂载启动时读取一次 `/etc/os-release` 的精确 `ID`
  检测宿主：`ubuntu`/`debian` 映射为 Ubuntu，`alinux`/`anolis` 映射为 Alinux。
  检测是 **fail-closed** 的：不参考 `ID_LIKE`，因此 RHEL 系衍生版（Rocky、
  AlmaLinux、CentOS 等）不会被静默判定为 Alinux，无法识别的宿主会拒绝挂载。
  其他发行版请显式设置 `ubuntu` 或 `alinux`。
- `rules_path` 是**可选的外部覆盖**。省略即使用内置目录；设置非空路径则改为加载
  该外部只读规则文件。留空（空串/空白）会被拒绝，而不会当作默认值。SkillFS 在挂载
  启动时一次性加载并校验所选规则；读时热路径只做内存内替换——全程不调用模型、网络
  或子进程。
- TOML 不能逐条启用规则。`rules_path` 会完整替换内置目录，而不是与它合并。若要
  保留默认规则，同时启用一条受保护规则或增加本地映射，请从源码 checkout 复制
  `crates/skillfs-core/assets/ubuntu-alinux.yaml`，编辑副本并配置其绝对路径。将选中
  规则的 `auto_apply: never` 改为 `always`，或追加一条字段完整的自定义规则。
  规则文件只在挂载启动时加载一次，修改后需要重新挂载。当前没有 catalog overlay、
  hot reload 或 export 命令。

内置目录中，高置信度规则为 `auto_apply: always`；中/低置信度规则为
`auto_apply: never`，因此会保护匹配 span，但不会执行替换。规则文件
（内置或外部）是一个顶层 YAML 序列，每条规则声明两侧 OS 的字面量、`direction`
以及显式的 `auto_apply` 资格标记：

```yaml
- ubuntu: "apt-get install -y "
  alinux: "dnf install -y "
  direction: bidirectional          # bidirectional | ubuntu_to_alinux_only | alinux_to_ubuntu_only
  match: literal                    # literal | token —— 可选，默认 literal
  auto_apply: always                # always | never —— 必填
```

- `auto_apply` 在每条规则上都是**必填**的（外部覆盖文件同样如此）。只有
  `auto_apply: always` 的规则会被应用，且仅在目标允许的方向上生效。缺少
  `auto_apply` 的规则文件会在挂载启动时被拒绝，并给出指明出错规则序号的错误信息。
- `confidence` 与 `notes` 作为人类可读注解被接受，但不影响任何行为——资格完全
  由 `auto_apply` 决定。
- `match` 是可选字段，默认 `literal`，因此既有规则文件继续按子串匹配。
  `match: token` 会在两个方向上检查 source 字母数字边缘的 ASCII 字母数字边界：
  `cron` 可在 EOF、空白、换行或标点前匹配，但不会命中 `micron`、`crontab`、
  `cronutils` 或 `cron2` 的内部。
- 替换是对原始字节的单遍非级联扫描：每个位置优先匹配最长（最具体）的模式，因此
  `apache2` 与 `apache2-utils` 这类重叠模式不会连锁改写，且与文件顺序无关。
- 不生效的模式（`auto_apply: never`、identity、方向不允许）仍会参与匹配并原样输出，
  从而保护整个 span，使更短的可用规则无法改写其内部（例如 `never` 的
  `/etc/init.d/apache2` 不会被 `apache2` 规则改写）。protection 按
  `(source, match)` 去重：substitution 只移除相同 source 和 mode 的 protection。
  不同 mode 可以共存；只有 substitution 自身的 mode 命中当前输入时才优先，否则
  命中的 protection 仍会保留整个 span。
- 多对一的正向映射（多个 Ubuntu 写法 → 同一个 Alinux 包）必须**显式**解决反向
  歧义：恰好将一条标为 `bidirectional`（作为规范反向），其余标为
  `ubuntu_to_alinux_only`。两条在反向目标上冲突的 `bidirectional` 规则会被判定
  为歧义并拒绝。

当 `enabled = true` 时，缺失或不可读的外部 `rules_path`、留空的 `rules_path`、
YAML 格式错误、非法的 `direction`/`auto_apply`/`match` 值、重复或冲突的模式，或
`target_os = "auto"` 无法识别宿主，都会在挂载开始前以可执行的错误信息失败，而不是
静默禁用适配器。

## 项目结构

```text
crates/
  skillfs-core/   parser, store, views, compiler, env, watcher
  skillfs-fuse/   FUSE 文件系统与 POSIX passthrough 层
  skillfs-cli/    mount / stop / classify / validate / list
container/        Sidecar 镜像：Dockerfile、entrypoint、preflight、mount probe
deploy/kubernetes/  Kubernetes Sidecar manifest 与示例 Skill source
docs/specs/       实现规格
docs/security/    external decision 与 runtime activation 文档
docs/testing/     POSIX 验收与 external harness 文档
docs/skills/      随仓库分发的 agent-facing SkillFS skill
scripts/          build.sh、test.sh 与可选 POSIX harness
```

## 测试脚本

- [scripts/build.sh](scripts/build.sh)
  - 执行 workspace build。
- [scripts/test.sh](scripts/test.sh)
  - 创建临时 skill source 目录和 `skillfs-views.toml`。
  - 验证 FUSE mount 启动成功。
  - 验证 `/skills` 暴露 default-view skills。
  - 验证 `skill-discover` 提供的路径可读取 secondary skills。
  - 验证 skill 目录中物理文件的 passthrough read。
  - 验证通过 `SIGTERM` 干净卸载。
- [scripts/posix/run_pjdfstest.sh](scripts/posix/run_pjdfstest.sh)
  - 可选 external POSIX harness；普通 `cargo test` 不依赖它。

## Kubernetes Sidecar

SkillFS 可以通过特权 Sidecar 向非特权工作负载提供 FUSE view。部署需要
Kubernetes 1.29+、`/dev/fuse`，并允许 Sidecar 使用特权模式。

Sidecar supervisor 检测 FUSE 读取失败，并在容器内重新挂载。可通过
`SKILLFS_SUPERVISOR_*` 配置恢复预算；默认值和恢复边界见下方 Sidecar 用户指南。

对于只读安装的 Skill，指南还提供 Ledger 部署示例，将初始内容复制到私有可写
源目录，在挂载前扫描，并对 Cosh 隔离安装包原始目录。缺失或无效的 activation
仍保持隐藏。
该示例通过 `skillfs mount --read-only` 在内核边界拒绝 FUSE 写入，Ledger 仍可写入
独立的私有源目录。
Ledger RPC 探针控制启动与就绪状态，并在 daemon 无响应时触发重启。

```bash
cd src/skillfs
IMAGE=registry.example.com/anolisa/skillfs-sidecar:$(git rev-parse --short=12 HEAD)
DOCKERFILE=container/Dockerfile
# Alibaba Cloud Linux 4 使用 container/Dockerfile.alinux4。
docker build -f "$DOCKERFILE" -t "$IMAGE" .
docker run --rm "$IMAGE" skillfs --version
docker push "$IMAGE"
kubectl apply -f deploy/kubernetes/00-namespace.yaml
kubectl apply -f deploy/kubernetes/10-example-configmap.yaml
sed "s|skillfs-sidecar:dev|$IMAGE|g" deploy/kubernetes/20-pod.yaml |
  kubectl apply -f -
```

部署、`skill-discover` 验证、故障排查和清理见
[Kubernetes Sidecar 用户指南](../../docs/user-guide/zh/runtime/skillfs-kubernetes-sidecar.md)
（[English](../../docs/user-guide/en/runtime/skillfs-kubernetes-sidecar.md)）。

## 测试覆盖

`crates/skillfs-fuse/tests/` 覆盖：

- normal 和 in-place mount 下的 compiled `SKILL.md` read、write passthrough、
  store reparse、mkdir/rename/unlink/rmdir 可见性，以及 stale frontmatter 防回归；
- POSIX open/create、metadata、目录流、长路径 fallback、open-after-unlink、
  safe symlink/link/FIFO 和 `user.*` xattr；
- `.skill-meta`、lifecycle namespaces、security mode、audit runtime、source
  drift、install inbox、staging/direct install flows、trusted writer、trusted
  metadata view、activation consumer、control socket server behavior、notify、
  runtime reload、startup reconcile 和 post-publish grace paths。

`crates/skillfs-cli/tests/` 覆盖 CLI parsing 和 startup gates，包括 managed
mount supervision、activation/notify option compatibility、backing-root
requirements、trusted writer executable validation，以及 control-socket
trusted peer configuration。

`skillfs-core` 通过单元和集成测试覆盖 parser、store、compiler 与 watcher 行为。

## 功能亮点

- 虚拟 views 与物理文件系统解耦：目录可见性由 view 控制，文件内容仍来自真实
  source 树。
- `SKILL.md` 读写刻意分离：agent 读取编译后内容，写入更新原始 source 文件。
- rename 后目录名是统一权威 skill key，避免 stale frontmatter 把旧 skill 名重新注入。
- in-place mount 使用预打开 source dir fd，让 SkillFS 写透传时不会递归进入自己的
  FUSE mount。
- active mapping 可以把 `/skills/<name>` 暴露为 current source、trusted
  snapshot 或 hidden，已打开 fd 保持 open-time target pinning。

## 安全集成

SkillFS 不在文件系统核心中执行扫描、签名校验或风险判断。外部 provider 决定
一个 skill 应暴露为：

- `current`：服务 live source；
- `fallback`：服务可信 `.skill-meta/versions/*.snapshot`；
- `hidden`：从 agent-facing 视图隐藏该 skill。

当前支持两条集成路径：

- 兼容 decision-command 模式：
  `--security --decision-command <COMMAND>` 会执行
  `<COMMAND> scan <skill_dir> --json`，再执行
  `<COMMAND> resolve <skill_dir> --json`。
- activation-file 模式：
  `--security --activation-mode file` 消费
  `.skill-meta/activation.json` 或
  `user.agent_sec.skill_ledger.activation`；配置后会向外部 daemon 发送 notify
  事件，并在 activation 变化后重新加载 active mapping。

相关安全能力：

- `.skill-meta/**` 对不可信 lookup/list/read 路径隐藏，普通 mutation 会被拒绝。
  可信 exact-path access 可把 metadata 操作路由到 live source。
- `--audit-log <PATH>` 写稳定 JSONL audit events。
- `--security-mode` 要求 `SOURCE` 和 `MOUNTPOINT` 指向同一目录，使普通
  userspace 访问都经过 FUSE policy 和 audit。
- `/.skillfs-inbox/<skill>/...` 是 hidden 或 new skill 的安装/修复入口；
  写入落到 source，完成信号可触发外部安全流程。
- `--notify-socket <PATH>` 将 debounce 后的 skill mutation 通知发给外部 daemon。
  Notify v2 使用 canonical 路径和完整 flat/Hermes `skillId` 标识 Skill；
  live/backing 路径通过独立 resolver 解析。in-place notify mount，以及任何配置了
  `--ledger-backing-root` 的 notify mount，都要求配置 authenticated control-peer
  mode，确保 daemon 访问 source 前 resolver 已可用。
  `--notify-auth-key-file <PATH>` 可以为实现拟议合同的容器 peer 启用双向 HMAC
  认证。内部 notify v2 业务 payload 保持不变，request 和 acknowledgement 由
  session-bound tag 保护。Authenticated notify 还要求 owner 匹配的 socket 位于
  owner 匹配且不给 group/other 任何权限的目录下，目录推荐使用 `0700`；socket
  同样不能给 group/other 任何权限。首版 profile 要求 SkillFS 与 sec-core 使用相同
  effective UID。本次 SkillFS 改动不实现 peer-side sec-core 支持，该工作仍由 #2439
  跟踪。
- `--activation-events-log <PATH>` 将 activation protocol events 写成 JSONL。
- `--activation-reload-mode poll` 在 notify events 后重读 activation state，
  无需 remount 即可更新 resolver。
- startup reconcile 会通过 notify worker 为已知 skill 入队，并在暂时性投递失败时
  重试直至 daemon 确认，因此 SkillFS 先于 daemon 启动时仍能自行收敛。不可重试的
  错误会被报告，认证结果不确定时则使用所有 skill 共享的有界 endpoint 预算，避免
  重试风暴。重试与可观测性细节见
  [Reconcile Delivery Durability](docs/security/runtime-activation-implementation-plan.md#reconcile-delivery-durability)。
- `--ledger-backing-root <PATH>` 为 in-place activation/notify mount 提供
  daemon 可见的 source 视图，因为公开 source path 已经是 FUSE over-mount。
  推荐使用 `/run/user/$UID/skillfs-ledger/...` 或
  `/run/skillfs-ledger/...` 作为 daemon-facing root。不要使用 `/tmp` 或
  `/var/tmp`：packaged `agent-sec-core.service` 以 `PrivateTmp=true` 运行，
  宿主 tmp 路径对 daemon 不可见，因此会在启动阶段被拒绝。
- `--trusted-writer-exe <PATH>` 是推荐的 mount-path 可信写门禁。它验证
  `/proc/<tgid>/exe`、`(dev, ino)` 和进程 start time，以降低 PID reuse 和
  process-name spoofing 风险。
- `--trusted-writer <NAME>` 是已废弃的兼容门禁，基于 Linux TGID `comm`；
  进程名可伪造，不应用作生产可信依据。
- `--control-socket <PATH>` 在恰好配置一种 peer mode 时启动可信 Unix socket
  control plane。`--trusted-peer-exe <PATH>` 保持宿主机 executable 认证；
  `--trusted-peer-key-file <PATH>` 启用显式容器 HMAC profile，以 session-bound tag
  保护 control request/response，并要求显式 socket path。可信 peer 可通过
  `meta.writeActivation`、`meta.setActivationXattr` 等方法写 activation JSON 或
  xattr。
- control plane 是 opt-in 且需认证的。endpoint 按优先级解析：CLI
  `--control-socket` > 配置 `[control_socket].path` > 默认的每用户 endpoint
  `/run/user/<uid>/skillfs/control.sock`。executable peer 未给显式 path 时使用默认
  endpoint；HMAC mode 始终要求显式 path；仅给显式 path 而未配置 peer mode 为配置
  错误。默认 endpoint 绝不 fallback 到 `/tmp` 或 `/var/tmp`，第二个实例也绝不
  unlink 处于活跃状态的 endpoint。
- `skill.resolveLiveSource` 是只读查询，将调用方给定的 canonical Skill 目录
  映射到物理 live/backing source。返回 `managed=true`（含推导出的
  `skillId`、`relativeSkillDir`、`liveSkillDir` 以及实际 live 目录的
  `(device, inode)` identity）、对 managed root 之外的合法路径返回
  `managed=false`，或返回 structured error。skillId 由 canonical 相对路径推导，
  因此 flat（`my-skill`）和 Hermes nested（`apple/apple-notes`）布局都会解析
  为完整 id。无需 `register`、`mountId` 或 `generation`。

## 文档

- [Kubernetes Sidecar 部署指南](../../docs/user-guide/zh/runtime/skillfs-kubernetes-sidecar.md) -
  在 Kubernetes 中构建、部署、验证和清理 SkillFS。
- [docs/specs/skillfs-spec.md](docs/specs/skillfs-spec.md) - 架构、运行时一致性边界和部署场景。
- [docs/specs/core-spec.md](docs/specs/core-spec.md) - `skillfs-core` 实现。
- [docs/specs/fuse-spec.md](docs/specs/fuse-spec.md) - `skillfs-fuse` 实现。
- [docs/specs/posix-phase1-spec.md](docs/specs/posix-phase1-spec.md) - POSIX passthrough 基线。
- [docs/testing/posix-phase1-acceptance.md](docs/testing/posix-phase1-acceptance.md) - POSIX 验收清单。
- [docs/testing/posix-external-harness.md](docs/testing/posix-external-harness.md) - external POSIX harness 用法。
- [docs/security/external-decision-protocol.md](docs/security/external-decision-protocol.md) - decision-command JSON 协议。
- [docs/security/runtime-activation-implementation-plan.md](docs/security/runtime-activation-implementation-plan.md) - activation、notify、reload 与 backing-root 集成。
- [docs/design/control-socket-resolver.md](docs/design/control-socket-resolver.md) - control socket 默认 endpoint 与只读 `skill.resolveLiveSource` resolver（SkillFS S1）。
- [docs/design/notify-v2.md](docs/design/notify-v2.md) - daemon change notification 的 canonical Skill 身份和 live-root 解耦（SkillFS S2）。
- [docs/skillfs-filesystem-capability-record.md](docs/skillfs-filesystem-capability-record.md) - 长期维护的 filesystem capability record。
- [POSIX_FS_TEST_MATRIX.csv](POSIX_FS_TEST_MATRIX.csv) - POSIX 测试矩阵与当前覆盖。
- [POSIX_FS_REFERENCES.md](POSIX_FS_REFERENCES.md) - POSIX、FUSE 和项目参考资料。

## 验证

这些命令等价于 CI 检查。修改 SkillFS 代码并提交 PR 前应执行。

```bash
# 1. 格式化：必须无 diff
cargo fmt --all --check

# 2. Clippy：在 -D warnings 下必须零 warning
cargo clippy --workspace --all-targets -- -D warnings

# 3. workspace 内单元与集成测试
cargo test --workspace

# 4. 端到端 FUSE mount 测试：需要 fuse3 和 /dev/fuse；在 macOS 或无 /dev/fuse 的容器中会自动跳过
scripts/test.sh

# 5. Rustdoc：修改公共 API 或 doc comments 时必须执行；也有助于尽早发现 intra-doc link 失效
cargo doc --workspace --no-deps
```

注释风格、模块布局、依赖策略、错误处理和 commit 规范见 [AGENTS.md](AGENTS.md)。
