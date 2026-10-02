# SkillFS

SkillFS 是面向 Agent Skill 的 FUSE 虚拟文件系统。它把物理 skill 源目录映射成
稳定的运行时视图，读取 `SKILL.md` 时返回编译后内容，普通文件仍由底层 source
树承载。

SkillFS 不做业务层安全判断。agent-sec-core 或 Skill Ledger 等外部组件负责扫描
skill 并写入 activation 状态。SkillFS 消费这些状态，将每个 skill 暴露为 live、
fallback snapshot 或 hidden。

## 适用场景

适合使用 SkillFS 的场景：

- 需要给 Agent 提供稳定的挂载路径；
- 需要隔离 source workspace 和 Agent 可见视图；
- 需要 default view 过滤，并通过 `skill-discover` 发现 secondary skills；
- 生产访问需要 in-place policy 和 audit 覆盖；
- 需要对接 Skill Ledger，提供 fallback / hidden 运行时视图；
- 需要保护 `.skill-meta`，避免普通 Agent 进程读取元数据。

不要直接对已有 hub workspace 做 in-place mount，尤其是该 workspace 同时包含
`.hub` 目录、外部 manifest 或 registry 元数据时。推荐分离 hub workspace 和
干净的 SkillFS source root。

## 环境要求

| 条件 | 说明 |
| --- | --- |
| OS | FUSE mount 需要 Linux |
| FUSE | FUSE3（`libfuse3-dev`、`fuse3` 或等价包） |
| 设备 | `/dev/fuse` 必须可用 |
| Rust | 源码构建需要 1.86+ |

macOS 可以运行 `validate`、`list`、`classify` 等不依赖 FUSE 的命令，但不能挂载
SkillFS。

## 安装

```bash
# 推荐包安装
sudo anolisa --install-mode system install skillfs

# 开发者源码构建
cd src/skillfs
cargo +1.86.0 build --release
```

## Source 布局

SkillFS 期望 source 目录下每个子目录对应一个 skill：

```text
/path/to/skills/
  demo-weather/
    SKILL.md
    scripts/
      run.sh
  demo-search/
    SKILL.md
    config.json
```

目录名是权威运行时 skill id。`SKILL.md` frontmatter 里的 `name` 字段是展示元数据，
不会覆盖目录 key。

不要把 `.skill-meta` 当作普通 Agent 数据使用。它保存 SkillFS 和 ledger 元数据，
对普通调用方隐藏。

## 快速开始

```bash
# 验证源目录中的 skills
skillfs validate /path/to/skills

# 列出所有 skills
skillfs list /path/to/skills

# 生成 skillfs-views.toml
skillfs classify /path/to/skills

# 挂载虚拟文件系统
skillfs mount /path/to/skills /mnt/skillfs --foreground
```

normal mount 后，Agent 读取：

```text
/mnt/skillfs/skills/<skill-name>/SKILL.md
```

前台测试挂载可以用 `Ctrl+C` 停止，也可以执行：

```bash
fusermount3 -u /mnt/skillfs
```

## 挂载布局

### Normal Mount

Normal mount 使用不同的 source 和 mountpoint 目录：

```bash
skillfs mount /path/to/skills /mnt/skillfs --foreground
```

Agent 通过 `<MOUNTPOINT>/skills` 访问 skill。直接写 source 目录会绕过 SkillFS
policy 和 audit；通过挂载路径写入时会透传到底层 source 树。

适合本地开发、兼容性检查，以及 source workspace 由其他进程管理的环境。

### In-place Mount

In-place mount 使用同一个目录作为 source 和 mountpoint：

```bash
skillfs mount /path/to/skills /path/to/skills \
  --foreground \
  --security-mode \
  --audit-log /var/log/skillfs/audit.jsonl
```

SkillFS 会 over-mount source 目录，因此普通用户态访问会经过 FUSE policy 和
audit。In-place mount 不额外增加 `/skills` 层：

```text
/path/to/skills/<skill-name>/SKILL.md
```

生产安全集成建议使用 in-place mount。会替换或 rename mountpoint 目录本身的工具，
例如 workspace checkpoint 或 rollback 工具，必须在 mount 前或 unmount 后运行。

### Managed Mount

`--managed` 会启动 detached supervisor，将 desired state 保持为 mounted，并在
worker 意外退出后重新挂载：

```bash
skillfs mount /path/to/skills /mnt/skillfs --managed
skillfs stop /mnt/skillfs
```

`skillfs stop <MOUNTPOINT>` 会清除 desired state、终止 supervisor 和 worker，并
执行 unmount。该命令是幂等的，挂载已经停止时重复执行也安全。

Managed mode 还会在 worker 异常退出后检测 stale/dead FUSE endpoint，清理后按
有界重试重新挂载。默认 foreground mount 行为不变：收到 `SIGTERM` 或 `Ctrl+C`
时仍会退出并 unmount。

### 挂载配置

通过 TOML 文件聚合显式指定的源目录：

```toml
mountpoint = "/mnt/skillfs"
sources = [
  "/path/to/workspace/skills",
  "/path/to/managed/skills",
  "/path/to/bundled/skills",
]
```

```bash
skillfs mount --config /path/to/skillfs-mount.toml
```

源按优先级从高到低排列。同名 skill 目录整体选用第一个来源，包括脚本和资源；
不合并被遮蔽 skill 的文件。重名日志记录选中和被遮蔽的路径。
视图位于 `/mnt/skillfs/skills`，discovery 输出挂载后的路径。
继续使用目录名作为 skill 标识，不实现 OpenClaw 的 frontmatter 名称优先级规则。

文件只接受 `mountpoint` 和 `sources`。至少指定一个源，所有路径必须为绝对路径，
不展开 `~` 或环境变量。源必须存在且可读；规范化后相同的源去重，保留原顺序。
源与挂载点不能相互包含。无法解析的 skill 或合并后超过现有数量上限会导致启动失败。

多个不同源自动启用只读挂载；单源仍可写，除非指定 `--read-only`。
该入口固定使用 flat 输出布局，保留现有 flat/category 源目录扫描深度。
只使用第一个源的 `skillfs-views.toml`；只读配置挂载不更新此文件。
未分配到任何 view 的 skill 会在内存中加入有效默认视图；显式分配到 secondary
view 的 skill 保持原有分组。
不合并源配置，不自动发现源，也不热加载。修改配置或新增、删除 skill 后需重新挂载；
读取结果不是不可变的内容快照。

挂载配置不能与位置参数或 security、activation、installation、layout、managed
模式参数混用。允许的可选参数为 `--foreground`、`--allow-other`、`--read-only`、
`--verbose` 和 `--log-file`。不含 `sources` 或 `mountpoint` 的文件继续支持原有
`mount SOURCE MOUNTPOINT --config security.toml` 用法。

接入 OpenClaw 时，将所需 skill loader 指向挂载后的 `skills` 目录，并检查实际加载路径。
仅添加 extra directory 不会替代更高优先级的原生来源；直接访问源目录仍不受此挂载覆盖。
回退时执行 `fusermount3 -u /mnt/skillfs`，再使用原有单源命令。

## CLI 工具

### validate

```bash
skillfs validate /path/to/skills
skillfs validate /path/to/skills --format json
```

`validate` 会报告 successful、degraded 和 failed skill parse。Parse failure 会
进入状态汇总并返回非 0 exit code；仅 degraded 的 skill 会被报告，但 exit code 仍为
0。

JSON 输出中，error 和 warning entry 都包含 `path` 字段，方便调用方定位具体出错的
skill 文件。

### list 和 classify

```bash
skillfs list /path/to/skills
skillfs list /path/to/skills --enabled-only
skillfs classify /path/to/skills --primary-count 6
skillfs classify /path/to/skills --dry-run
```

`list` 输出发现的 skills 及其元数据。`classify` 生成或预览
`skillfs-views.toml`；前 N 个 skills 进入 default view，其余进入 secondary view。

## Views 与 Discovery

source 目录下的 `skillfs-views.toml` 控制可见性：

```toml
[[view]]
name = "major"
default = true
description = "Core skills shown directly in /skills"
skills = ["github", "notion", "slack"]

[[view]]
name = "other"
default = false
description = "Additional skills accessible through skill-discover"
skills = ["apple-notes", "blogwatcher"]
```

default view 会直接出现在挂载后的 skill 视图中。Secondary views 由虚拟
`skill-discover` skill 列出，其 `SKILL.md` 包含 skill 名称和 source path。

未分配到任何 view 的 skill 会在下次 mount 时加入 default view。

## 读写语义

| 操作 | 行为 |
| --- | --- |
| `readdir` | 由 views 和 runtime activation state 控制 |
| 读取 `SKILL.md` | 默认返回编译后内容；directive stage 关闭且无其他 stage 时返回选定目标的 raw 内容 |
| 读取普通文件 | 透传到底层物理 source 树 |
| 写入 `SKILL.md` | 写透传，并重新解析 store |
| 写入普通文件 | 写透传，不改变 skill 元数据 |
| rename skill 目录 | 目录名是权威 key |
| symlink 或 hardlink | 限制为安全的同 skill 相对目标 |
| `user.*` xattr | 普通路径上保守透传 |

In-place authoring 支持新建 skill 目录。刚创建但尚未写入 manifest 的目录不会暴露
phantom `SKILL.md`；写入 `SKILL.md` 后，SkillFS 会重新解析并暴露编译后的视图。
Pending 或 direct-final install 可以保留普通顶层 skill 目录的 mode、timestamp 和
ownership 等元数据。`.skill-meta/**` 仍只允许 trusted metadata path 访问。

无安全集成时，skill 默认读取 live source 树。启用 security activation 后，
可见性受 active mapping 限制：

- current：读取 live source 树，例如旧 decision-command resolve 路径产生的状态；
- fallback：读取 `.skill-meta` 下的可信 snapshot；
- hidden：对普通调用方隐藏 skill。

Activation file mode 下，Activation JSON 表达 fallback 和 hidden 两种状态，不写
current/live 状态。如果该模式下某个 skill 没有 activation JSON 或 activation
xattr，SkillFS 会按 fail-safe 默认值将其视为 hidden。

### 权限判定来源

可见性决定读**哪一份**内容，权限决定**能不能**读写。自 0.4.0 起两者的判定来源是分开的：

| 操作 | 权限来源 |
| --- | --- |
| Agent 可见的读取 | 当前激活目标的权限，即 fallback 时用 snapshot 自身的权限 |
| 写入 | live source 的权限 |

因此一个 skill 可能 snapshot 可读、live source 不可写。升级到 0.4.0 前若依赖读取跟随
live source 权限，需要复核 snapshot 的权限位。

## 读时转换

在解析出激活目标之后，`SKILL.md` 字节会先经过一条固定顺序的转换流水线，再交给
agent：

1. **directive** stage 执行条件编译（`@if` / `@else` / `@endif` 以及启发式命令
   归一化），**默认开启**；开启时始终第一个执行，输出与旧版本一致。可通过
   `[transforms.directive] enabled = false` 关闭。
2. 可选的 **OS 适配器** stage 第二个执行，且只作用于 `SKILL.md`，在 Ubuntu/Debian
   与 Alinux/Anolis 约定之间改写发行版相关字面量。

两个 stage 都是可选的：可以两者都开、仅 directive（默认）、仅适配器（关闭
directive），或两者都关闭——空流水线原样返回选中的字节。初始化诊断会报告实际
启用的 stage 列表。

| Directive | OS adapter | Agent 可见的 `SKILL.md` |
| --- | --- | --- |
| 开启（默认） | 关闭（默认） | 旧版 compiler 输出 |
| 开启 | 开启 | compiler 输出，再执行 OS 适配 |
| 关闭 | 开启 | 对选中的原始字节执行 OS 适配 |
| 关闭 | 关闭 | 选中的原始字节 |

流水线只影响 agent 读到的字节。源文件、可信 snapshot、激活元数据与规则文件都不会
被修改。Hidden 技能保持隐藏且不进入流水线；fallback 读取只转换可信 snapshot，绝不
回退到 live source。flat `<skill>/SKILL.md` 与 Hermes
`<category>/<skill>/SKILL.md` 布局使用相同的流水线与 activation 顺序。snapshot 读取只
解析、读取并转换选中的 snapshot；若 snapshot target 解析/定位失败，或其中的
`SKILL.md` 无法读取，操作会返回错误（虚拟读取边界通常为 `ENOENT`），绝不会重试 live
source。`getattr` 大小、部分读取与完整读取始终基于同一份转换结果。只有 `SKILL.md`
会被适配——其他 Markdown、shell、Python 与配置文件原样透传。

### 关闭 directive stage

directive/compiler stage 默认开启，除非显式关闭：

```toml
[transforms.directive]
enabled = false
```

`[transforms.directive]` 段不存在时保持开启，因此现有配置不受影响。关闭它只影响
compiler stage；OS 适配器仍是独立的 opt-in 项。

### 启用 OS 适配器

OS 适配器默认关闭，通过现有的 `--config <PATH>` TOML 文件配置（不新增 CLI flag）。
启用且未设置 `rules_path` 时使用内置规则目录：

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

SkillFS **内置一份 311 条规则的 Ubuntu/Alinux 规则目录**，通过仓库资产嵌入二进制，
因此源码构建、RPM 与容器中无需额外文件即可工作，且仍是 opt-in。目录中 257 条为
`auto_apply: always`，54 条为 `auto_apply: never` protection rule；编译后面向 Alinux
有 223 条 active substitution，面向 Ubuntu 有 192 条。高置信度规则会应用；中/低
置信度规则仅用于保护匹配 span。

- `target_os = "auto"` 在挂载启动时读取一次 `/etc/os-release` 的精确 `ID` 选择
  目标：`ubuntu`/`debian` 映射为 Ubuntu，`alinux`/`anolis` 映射为 Alinux。检测是
  fail-closed 的：不参考 `ID_LIKE`，因此 RHEL 系衍生版（Rocky、AlmaLinux、CentOS
  等）不会被静默判定为 Alinux，无法识别的宿主会拒绝挂载。其他发行版请显式设置
  `ubuntu` 或 `alinux`。
- `rules_path` 是可选的外部覆盖。省略即使用内置目录；设置非空路径则改为加载该外部
  只读规则文件。留空会被拒绝，而不会当作默认值。SkillFS 在启动时一次性加载并校验所选
  规则；读时路径只做内存内替换，不解析 YAML、不读取 `/etc/os-release`、不启动进程、
  不访问网络或调用 LLM。
- TOML 控制启用哪些 stage、目标 OS 与规则文件；YAML 规则文件控制单条映射及其资格。
  TOML 不支持逐条启用规则。

### 启用受保护规则和添加自定义规则

规则文件（内置或外部）是顶层 YAML 序列。每条规则声明两侧 OS 的字面量、`direction`
以及必填的 `auto_apply` 标记：

```yaml
- ubuntu: "apt-get install -y "
  alinux: "dnf install -y "
  direction: bidirectional          # bidirectional | ubuntu_to_alinux_only | alinux_to_ubuntu_only
  match: literal                    # literal | token —— 可选，默认 literal
  auto_apply: always                # always | never —— 必填
```

`rules_path` 是**完整替换**，不是 overlay。若要保留全部内置映射，只定制选中的条目，
请从源码 checkout 复制仓库资产：

```bash
cp src/skillfs/crates/skillfs-core/assets/ubuntu-alinux.yaml \
  /etc/skillfs/ubuntu-alinux.custom.yaml
```

然后在 TOML 配置中设置
`rules_path = "/etc/skillfs/ubuntu-alinux.custom.yaml"`。使用绝对路径可避免依赖挂载
进程的工作目录。

若要按本地策略启用一条受保护的中/低置信度规则，请在复制的规则文件中修改其
`auto_apply` 值。例如：

```yaml
- ubuntu: "ufw"
  alinux: "firewalld"
  direction: ubuntu_to_alinux_only
  auto_apply: always
  confidence: low
  notes: "enabled by local policy"
```

追加字段完整的条目即可定义本地映射：

```yaml
- ubuntu: "acme-agent-dev"
  alinux: "acme-agent-devel"
  direction: bidirectional
  auto_apply: always
  confidence: high
  notes: "local package mapping"
```

`ubuntu`、`alinux`、`direction` 和 `auto_apply` 是必填字段；`match` 是可选字段；
`confidence` 与 `notes` 是不影响行为的可选注解。外部文件还必须保留所有仍需使用的
内置规则：SkillFS 不会将其与嵌入目录合并。规则只在挂载启动时加载一次，修改后需要
重新挂载。当前没有 catalog overlay、hot reload、逐条规则 id 或 export 命令。

- `auto_apply` 在每条规则上都是必填的（外部覆盖文件同样如此）；只有
  `auto_apply: always` 的规则会被应用，且仅在目标允许的方向上生效。缺少
  `auto_apply` 的规则文件会被拒绝，并给出指明出错规则序号的错误信息。
- `confidence` 与 `notes` 作为注解被接受，但不影响行为——资格完全由 `auto_apply`
  决定。
- `match` 默认 `literal`，所以既有规则文件继续按子串匹配。`match: token` 会在两个
  方向上检查 source 字母数字边缘的 ASCII 字母数字边界：`cron` 可在 EOF、空白、
  换行或标点前匹配，但不会命中 `micron`、`crontab`、`cronutils` 或 `cron2` 的内部。
- 替换是单遍非级联扫描：每个位置优先匹配最长的模式，因此重叠模式不会连锁改写，
  且与文件顺序无关。
- 不生效的模式（`auto_apply: never`、identity、方向不允许）仍会参与匹配并原样输出，
  保护整个 span，使更短的可用规则无法改写其内部。protection 按 `(source, match)`
  去重：substitution 只移除相同 source 和 mode 的 protection。不同 mode 可以共存；
  只有 substitution 自身的 mode 命中当前输入时才优先，否则命中的 protection 仍会
  保留整个 span。
- 多对一的正向映射必须显式解决反向歧义：将一条标为 `bidirectional`（规范反向），
  其余标为 `ubuntu_to_alinux_only`。在反向目标上冲突的 `bidirectional` 会被拒绝。

启用后，缺失或不可读的外部 `rules_path`、留空的 `rules_path`、YAML 格式错误、缺失或
非法的 `direction`/`auto_apply` 值、非法的 `match` 值、重复或冲突的模式，或
`target_os = "auto"` 无法识别宿主，都会在挂载开始前以可执行的错误信息拒绝挂载。

## 安全集成

### Activation File Mode

当外部 daemon 接收 SkillFS mutation event、扫描 source 树并写入 activation 元数据
时，使用 activation file mode：

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --foreground \
  --security \
  --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --activation-events-log /var/log/skillfs/activation-events.jsonl \
  --activation-reload-mode poll
```

流程：

```text
Agent 或 installer 通过 SkillFS 写入
  -> SkillFS 发送 notify event
  -> Skill Ledger 扫描并写 activation state
  -> SkillFS reload activation state
  -> skill 变为 live、fallback 或 hidden
```

`--activation-reload-mode poll` 需要 `--notify-socket` 或
`--activation-events-log`，因为 SkillFS 需要触发源来启动 polling。

`--notify-socket` 指向的是**外部 daemon 监听的** socket，而不是 SkillFS 自己创建的
socket。与 Skill Ledger 联合部署时，它就是 agent-sec-core daemon 的 endpoint，默认为
`$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock`，可用 `AGENT_SEC_DAEMON_SOCKET` 覆盖。
注意 notify 投递失败只记 warning，不会中断 FUSE 服务，因此指向错误路径的表现是 skill
一直停留在 hidden 而没有明显报错。

对于 in-place activation 和 notify mount，需要设置 `--ledger-backing-root`，给
daemon 一个可见的 backing source path，并启用 authenticated resolver。Notify v2
只携带 canonical identity，因此缺少 authenticated control-peer 配置的 in-place
notify 配置会在启动阶段被拒绝。out-of-place notify mount 如果显式配置
`--ledger-backing-root`，同样必须启用 resolver：

```bash
skillfs mount /path/to/skills /path/to/skills \
  --security-mode \
  --security \
  --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --trusted-peer-exe /usr/bin/python3.11 \
  --ledger-backing-root /run/user/$UID/skillfs-ledger/source
```

当 daemon 使用 `PrivateTmp=true` 运行时，避免把集成路径放在 `/tmp` 或
`/var/tmp`；这些路径对 daemon 不可见，并会被启动校验拒绝。

### Control Socket

可信 control socket 是生产环境推荐的 activation 写入路径，也承载只读的
resolver 查询：

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --security \
  --activation-mode file \
  --control-socket /run/skillfs/control.sock \
  --trusted-peer-exe /usr/bin/python3.11
```

该 socket 要求 `--security --activation-mode file`，与 `--decision-command` 互斥，
并且必须恰好配置一种 peer authentication mode。宿主机模式使用 Linux peer
credential 和 executable identity。

packaged AgentSecCore daemon 使用 `sys.executable` 启动 Skill Ledger worker，该路径
解析为 `/usr/bin/python3.11`；worker 并不是 `/usr/bin/skill-ledger` executable。
使用自定义 virtual environment 时，请用启动 daemon 的同一个 interpreter 执行以下
命令，并配置它输出的真实路径：

```bash
/path/to/ledger/python -c 'import os, sys; print(os.path.realpath(sys.executable))'
```

这个 M1 executable gate 信任的是 Python interpreter，而不是某个特定 module。请让
SkillFS 与 Ledger worker 运行在相同 UID/security domain，并注意同一 UID 下使用相同
interpreter 的其他进程也会通过 executable identity 校验。

#### Endpoint 与优先级

control plane 是 opt-in 且需认证的。endpoint 按优先级解析：

1. CLI `--control-socket <PATH>`
2. 配置文件 `[control_socket].path`
3. 默认的每用户 endpoint `/run/user/<uid>/skillfs/control.sock`

executable peer 未给显式 path 时使用默认 endpoint；HMAC mode 要求显式 path。仅给
显式 path 而未配置 peer mode 为配置错误；两者都没有则 control plane 保持关闭。默认
endpoint 绝不 fallback 到 `/tmp` 或 `/var/tmp`——若 `/run/user/<uid>` 不可用，启动会返回明确且可操作的
错误，此时必须显式传入 `--control-socket`。第二个实例绝不会 unlink 处于活跃状态
的 endpoint；只有确认属于 SkillFS 且为 stale 的 socket 才会被回收。

无需 `register`、`mountId` 或 `generation` 握手——endpoint 按 UID 稳定，resolver
可直接查询。

#### 容器 HMAC profile

可信 peer 运行在独立 PID 或 mount namespace 时，SkillFS 侧可以使用 HMAC
profile。两个进程必须从只挂载给可信容器的私有文件读取同一个 secret：

```bash
skillfs mount /var/lib/skillfs/source /var/lib/skillfs/shared/mount \
  --foreground --allow-other \
  --security --activation-mode file \
  --notify-socket /run/anolisa/peer/notify.sock \
  --notify-auth-key-file /run/anolisa/auth/skillfs.key \
  --control-socket /run/anolisa/skillfs/control.sock \
  --trusted-peer-key-file /run/anolisa/auth/skillfs.key
```

Secret file 必须是绝对路径、nonblocking、no-follow 的普通文件，由 effective user
所有，不给 group 或 other 任何权限，并包含 32–4096 个 raw bytes。FIFO 等非普通
文件会立即导致启动失败，不会阻塞。HMAC mode 不会 fallback 到 executable 或
plain authentication。双向认证后，session-bound tag 会在双方解释 raw business
request/response 前验证其完整性。Authenticated notify 要求 daemon socket 由
SkillFS 的 effective UID 所有，不给 group 或 other 任何权限，并直接位于 owner
匹配且不给 group 或 other 任何权限的目录下，目录推荐使用 `0700`。兼容的
agent-sec-core listener 会以 `0600` 创建该 endpoint；SkillFS 每次连接前都会检查
这些属性。因此首版 profile 要求 SkillFS 与 agent-sec-core 使用相同的 effective
UID。Runtime socket volume 只能以 read-write 方式挂载到可信容器；负向测试中的
workload 可以只读挂载，但不能替换 socket entry。
resolver 仍返回 `transport: shared_path`，因此
SkillFS 和 resolver peer 必须在同一个绝对路径看到物理 source。不要把 source 或
Secret 挂载到 workload 容器。

本仓库改动只实现 SkillFS server/client surface，不配置或实现 agent-sec-core。
Peer-side 行为是
[容器 Peer 认证](../../../../src/skillfs/docs/design/container-peer-authentication_zh.md)
中的拟议合同，仍需 sec-core maintainer 确认。在其后续实现合入前，应使用独立
probe 和
[单方面验证计划](../../../../src/skillfs/docs/testing/container-peer-authentication-unilateral-validation_zh.md)，
不能把 sec-core 联动描述为已经可用。

支持的 JSONL 请求示例：

```json
{"schemaVersion":"1","method":"ping"}
{"schemaVersion":"1","method":"status"}
{"schemaVersion":"1","method":"meta.writeActivation","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
{"schemaVersion":"1","method":"meta.setActivationXattr","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
{"schemaVersion":"1","method":"skill.resolveLiveSource","canonicalSkillDir":"/path/to/skills/apple/apple-notes"}
```

#### `skill.resolveLiveSource`

只读查询，将 canonical Skill 目录映射到其物理 live/backing source。业务参数只有
`canonicalSkillDir`，结果分三态：

- **`managed=true`**——路径位于受管 canonical root 内且对应有效 live Skill 目录。
  响应包含推导出的 `skillId`、`relativeSkillDir`、物理 `liveSkillDir`、live 目录的
  `identity`（`device`、`inode`）以及 `transport`（`shared_path`）。查询是只读的：
  不触发 scan、manifest、policy decide 或 activation write。
- **`managed=false`**——请求格式合法且 `canonicalSkillDir` 是位于受管 root 之外的
  合法绝对路径（`reason: not_managed`）。这是正常成功响应，调用方可直接管理该目录。
- **structured error**——非绝对或非规范路径（包括重复或末尾 `/`）、非法 `..` 段、
  symlink/path 逃逸、管理/保留目录、Skill 目录不存在、布局无效/缺少 `SKILL.md`、
  live source 不可安全访问，或 peer 认证失败。这些绝不会伪装成 `managed=false`。

skillId 由 canonical 相对路径推导，因此 flat（`my-skill`）和 Hermes nested
（`apple/apple-notes`）布局都会解析为完整 id。S1 只实现单 source runtime；该 endpoint
在未来多 canonical root 场景下仍共享。

> 说明：`skill.resolveLiveSource`（SkillFS S1）是只读 resolver。notify v2 与删除态
> 语义不属于 S1。

#### Notify v2

`skill_ledger.skillfs_notify_change` 使用 schema version 2。业务 payload 只包含
`canonicalSkillDir`、完整 `skillId`、`eventKind` 和相对 `paths`。Flat id 保持完整
（`weather`），Hermes id 保留两个分量（`category/weather`）。SkillFS 会排序、去重
paths；空数组表示需要重新扫描整个 Skill，也用于超过路径数量上限的情况。

canonical 目录由不跟随 source-root symlink 的绝对、词法规范化 source identity
推导。物理 live/backing root 只供 activation 和 S1 resolver 使用，因此通知不会暴露
backing 路径。daemon 必须直接接受 v2，并返回 `schemaVersion=2` 与
`accepted=true`；不提供 v1 fallback 或协商。

### Trusted Mount-path Writer

`--trusted-writer-exe <PATH>` 是 mount path 可信写入兼容门禁。新生产集成优先使用
control socket。

`--trusted-writer <NAME>` 已废弃，仅匹配 Linux 进程 `comm` 名称。兼容条件允许时，
应使用 executable identity。

### Decision-command Mode

`--security --decision-command <COMMAND>` 是旧的兼容路径。SkillFS 调用外部命令执行
scan 和 resolve 决策。

Decision-command mode 与 activation file mode、`--notify-socket`、
`--activation-events-log`、`--ledger-backing-root` 和 `--control-socket` 互斥。

## Install 协议

SkillFS 支持面向 installer 的生命周期路径：

- staging roots 可从普通 listing 隐藏，但精确 staging 路径仍可写；
- direct-to-final install 可在 activation 出现前保持 hidden；
- `/.skillfs-inbox/<skill>/...` 是 hidden 或 new skill 的 install/repair 入口；
  写入会落到 source tree，并可触发外部安全流程；
- quiet-timeout notification 可在配置的 quiet window 后聚合 install mutation；
- post-publish grace 可允许发布后有界的 installer 元数据写入；
- fallback skill 的 post-publish grace 路径会路由到 live source，便于 installer
  在 publish 后完成元数据更新。

这些行为通过 SkillFS TOML config 配置，并需要 `--notify-socket` 或
`--activation-events-log` 等 notify source。

## 可观测性

### Audit 和 Activation Logs

`--audit-log <PATH>` 写 filesystem audit events JSONL。
`--activation-events-log <PATH>` 写 daemon-driven activation 流程中的 activation
protocol events JSONL。

启用 OS adapter 后，成功以只读方式打开 flat 或 Hermes 虚拟 `SKILL.md` 时，Open audit
event 的 `detail` 会包含 content-free adapter context：
`transform=os_adapter target_os=<target> rule_digest=<sha256>`。其中只记录启用的 stage、
目标 OS 与规则文件 digest，绝不记录 source content、转换后 content、diff 或规则字面量。
成功的逐 syscall Read event 仍不输出，避免高频 audit flooding。

### SLS Ops 和 Runtime Metrics

SkillFS 会 best-effort 写 SLS records 到：

```text
/var/log/anolisa/sls/ops/skillfs.jsonl
```

该文件由部署侧/SLS 组件拥有并预创建。SkillFS 只在文件存在时追加写入；不会创建该文件
或父目录，写入失败也不会改变 CLI 或 FUSE 行为。

以下 CLI 命令会追加 ops records：`mount`、`list`、`validate` 和 `classify`。mount
存活期间，runtime metric record 使用 `record_type = "runtime_metric"`，覆盖 mount
lifecycle、view pruning、skill hits 和 security policy outcomes。兼容旧消费者的
mount-session summary 仍共享同一个文件。

## 常用参数

| 参数 | 作用 |
| --- | --- |
| `--foreground` | 前台运行 |
| `--managed` | 启动 detached supervised mount |
| `--security-mode` | 要求 source 和 mountpoint 是同一路径 |
| `--skill-layout <MODE>` | `auto`（默认，按 source-root marker 探测 Hermes）、`flat` 或 `hermes`；`hermes` 与 `--decision-command` 互斥 |
| `--security` | 启用安全集成 |
| `--activation-mode file` | 消费 activation JSON/xattr 状态 |
| `--activation-reload-mode poll` | notify trigger 后 poll activation |
| `--notify-socket <PATH>` | 向外部 daemon 发送 mutation event |
| `--notify-auth-key-file <PATH>` | 使用 owner-only 共享 key file 双向认证 notify 连接 |
| `--activation-events-log <PATH>` | 写 activation protocol events JSONL |
| `--audit-log <PATH>` | 写 filesystem audit events JSONL |
| `--audit-queue-capacity <N>` | audit writer 线程队列长度；`0` 用内置默认值，仅配合 `--audit-log` 生效 |
| `--events-log <PATH>` | 写旧 decision 流程的 security decision events JSONL，仅配合 `--security --decision-command` 生效 |
| `--control-socket <PATH>` | 覆盖 control socket endpoint（executable mode 默认 `/run/user/<uid>/skillfs/control.sock`） |
| `--trusted-peer-exe <PATH>` | 固定可信 control socket peer（未给 path 时在默认 endpoint 启用 control plane） |
| `--trusted-peer-key-file <PATH>` | 启用双向认证的容器 peer mode；要求显式 control socket，并与 `--trusted-peer-exe` 互斥 |
| `--trusted-peer-uid <UID>` | 额外约束 control socket peer 的 UID（取自 `SO_PEERCRED`） |
| `--trusted-peer-gid <GID>` | 额外约束 control socket peer 的 GID（取自 `SO_PEERCRED`） |
| `--trusted-writer-exe <PATH>` | 固定可信 mount-path writer |
| `--ledger-backing-root <PATH>` | 提供 daemon 可见的 source view |
| `--decision-command <CMD>` | 使用旧 external decision 模式 |
| `--pid-file <PATH>` | 写进程 pid file |
| `--allow-other` | 允许其他用户访问 FUSE mount |
| `--read-only` | FUSE 挂载以 `EROFS` 拒绝修改；独立的物理源目录仍可由其所有者写入 |
| `--config <PATH>` | 加载 SkillFS TOML 配置 |
| `-v`, `--verbose` | 启用 debug logging |
| `--log-file <PATH>` | 将日志写入文件 |

## 排障

**新安装的 skill 不可见。**
启用 security activation 后，新 skill 可能保持 hidden，直到 ledger 写入 activation
state。检查 notify 投递和 activation reload events。

**Fallback 读到旧版本。**
这是预期行为。Fallback 读取 `.skill-meta` 下的可信 snapshot，而不是 live source。

**看不到 `.skill-meta`。**
普通调用方看不到该目录是预期行为。可信 peer 可以通过配置的 trusted path 访问元数据。

**日志里出现 notify socket 失败。**
Notify 失败只是 warning，不会停止 FUSE 服务，但外部 daemon 可能收不到 mutation
event，直到 socket 修复。

**In-place activation 启动失败。**
检查是否设置了 `--ledger-backing-root`，以及该路径对 daemon 可见。使用
`PrivateTmp=true` 的服务不要使用 `/tmp` 或 `/var/tmp`。

**Managed mount 在 launcher 重启后仍然存在。**
这是预期行为。使用 `skillfs stop <MOUNTPOINT>` 停止。

## 更多参考

- [SkillFS README](../../../../src/skillfs/README_zh.md)
- [External decision protocol](../../../../src/skillfs/docs/security/external-decision-protocol.md)
- [Runtime activation plan](../../../../src/skillfs/docs/security/runtime-activation-implementation-plan.md)
- [FUSE crate layout](../../../../src/skillfs/docs/architecture/fuse-crate-layout.md)
