# 变更日志

[English](CHANGELOG.md)

本文件记录 ANOLISA 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.3.13] - 2026-09-20

### 变更

- RPM 安装、更新、卸载和依赖预检现在支持 Yum 3 与 DNF 4。
  ANOLISA 优先使用 `yum`，仅在其不存在时使用 `dnf`；指定版本仍按精确版本处理，
  遇到版本锁等系统限制时会报错，不再静默选择其他版本
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318))。
- RPM 候选软件包查询现在直接读取并校验配置仓库的元数据，无需 `dnf repoquery`。
  仓库访问遵循 `/etc/yum.conf` 中的代理和私有 CA 配置，该文件不存在时使用
  `/etc/dnf/dnf.conf`。未配置 RPM 仓库时，本地软件包查询和 Raw 安装也可正常执行；
  已配置仓库的错误仍会报告
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318)、
  [#3353](https://github.com/agentic-os-org/ANOLISA/pull/3353))。
- Raw 安装发现组件没有适用于当前操作系统与架构的可安装产物时，现在会明确显示
  请求的平台并列出已发布的其他平台，`--json` 错误也包含这些信息，
  便于区分平台缺失与组件名称不存在
  ([#3326](https://github.com/agentic-os-org/ANOLISA/pull/3326))。

### 修复

- 运行依赖检查、自动安装依赖和 `anolisa doctor` 现在统一通过 `ID` 与 `ID_LIKE`
  识别 RPM 和 Debian 衍生发行版。无法识别软件包体系时，会提供手动安装指引，
  不再根据 `PATH` 选择无关的软件包管理器
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318))。
- 云元数据收集遇到单个字段返回 HTTP 错误或空响应时，现在会继续读取其他字段，
  保留遥测快照中仍可获取的实例信息。连接或传输失败时，仍会停止向不可达端点
  发起后续查询
  ([#3308](https://github.com/agentic-os-org/ANOLISA/pull/3308))。

## [0.3.12] - 2026-09-16

### 变更

- 以 root 身份运行 `anolisa upgrade --dry-run` 时，现在会通过系统依赖求解器
  检查合并后的新增 RPM 安装集合，冲突会显示为错误。非 root 用户仍可预览，
  但会明确提示尚未检查安装冲突；使用 `sudo` 重试并保留原有 `--target` 选项
  即可执行检查
  ([#3294](https://github.com/agentic-os-org/ANOLISA/pull/3294))。
- 内置 ws-ckpt 适配器元数据现在要求 OpenClaw `>=2026.2.13`，
  阻止在配置写入可能持久化展开后的环境变量值及运行时默认值的旧版宿主上启用适配器
  ([#3239](https://github.com/agentic-os-org/ANOLISA/pull/3239))。

### 修复

- `anolisa adapter enable <component> openclaw` 现在会在安装后显式激活插件，
  恢复被上次卸载保留为禁用状态的插件。激活操作独立探测 capability 授权支持，
  `--dry-run` 也会显示激活命令
  ([#3223](https://github.com/agentic-os-org/ANOLISA/pull/3223))。
- Sandbox 安装和卸载现在以 DNF 退出状态判断成功与否，即使输出包含
  “already installed” 或 “No match” 等文本也不会掩盖失败；
  安装失败后不再继续后续配置阶段或记录成功
  ([#3226](https://github.com/agentic-os-org/ANOLISA/pull/3226))。
- RPM 软件包及文件归属查询现在区分明确缺失与数据库、命令执行失败，
  避免错误地报告“未安装”
  ([#3230](https://github.com/agentic-os-org/ANOLISA/pull/3230))。
- Raw 组件服务管理现在会报告 `systemctl is-active` 查询失败，
  不再将其视为 unit 缺失；合法的 inactive、failed 等服务状态仍受支持
  ([#3240](https://github.com/agentic-os-org/ANOLISA/pull/3240))。
- RPM 查询失败，或系统软件包、语言运行时探测遇到权限、执行及信号终止错误时，
  运行依赖检查现在会阻止自动安装依赖。`anolisa doctor` 会保留诊断信息，
  引导检查探测失败，而不是建议安装被误判为缺失的依赖
  ([#3280](https://github.com/agentic-os-org/ANOLISA/pull/3280)、
  [#3285](https://github.com/agentic-os-org/ANOLISA/pull/3285))。
- Debian 运行依赖检查现在区分可用、缺失、安装未完成及查询失败状态。
  安装未完成时需先手动恢复软件包状态，才能继续自动安装依赖；`doctor` 会引导检查
  软件包状态。Multi-Arch 查询默认选择本机架构，除非依赖明确指定其他架构
  ([#3292](https://github.com/agentic-os-org/ANOLISA/pull/3292))。

## [0.3.11] - 2026-09-09

### 新增

- 在两版组件目录中注册 `ktuner`，配置的仓库提供该软件包时，可在
  Linux x86_64 上发现并通过 RPM 安装
  ([#2084](https://github.com/alibaba/anolisa/pull/2084))。

### 变更

- 执行 `anolisa adapter enable <component> openclaw` 时，若宿主声明支持
  `--accept-capabilities`，现在会接受插件声明的能力。
  绕过不安全安装检查仍需要单独明确授权
  ([#3126](https://github.com/alibaba/anolisa/pull/3126))。
- `status` 和 `doctor` 现在允许修改 Raw 安装的 `type = "config"` 文件内容，
  包括从目录展开的文件，同时继续检查文件存在性、类型、路径安全、权限及
  capabilities。旧状态会在已安装清单能够明确判定时恢复配置文件类型。
  更新和卸载行为保持不变，执行这些操作前应备份编辑过的配置。
  旧版 CLI 无法读取新的 `kind = "config"` 状态：降级前应备份状态，
  将这些受管文件条目改为 `kind = "file"`，恢复内容摘要检查
  ([#3142](https://github.com/alibaba/anolisa/pull/3142))。

### 修复

- OpenClaw 卸载报告没有受跟踪的软件包时，若 `plugins list --json`
  确认插件已不存在且诊断正常，适配器清理现在可以完成；
  无法确认时保留清理归属记录，供后续重试
  ([#3118](https://github.com/alibaba/anolisa/pull/3118))。
- 系统缓存不可写时，组件索引读取现在改用临时私有缓存，
  使普通用户能够解析组件并进行预览，同时保留索引校验。
  使用 DNF 4 的 RPM 安装预检仍需要 root 权限
  ([#3127](https://github.com/alibaba/anolisa/pull/3127))。
- 全新 Raw 安装现在会在 `post_install` 和 `post_enable` hook 执行后
  记录文件摘要，避免将 hook 写入的内容误报为损坏，并继续检测不可变文件的后续修改。
  修复和更新仍恢复软件包内容，不会重新执行安装 hook
  ([#3141](https://github.com/alibaba/anolisa/pull/3141))。
- RPM 安装现在会在创建恢复日志前检查 DNF 冲突，覆盖 `--dry-run` 及合并的
  `install --all` 事务，避免依赖求解被拒绝后留下虚假的待恢复操作。
  使用 DNF 4 时，RPM `--dry-run` 需要加 `sudo`，检查可能刷新仓库元数据
  ([#3158](https://github.com/alibaba/anolisa/pull/3158))。
- `anolisa list` 现在与安装使用相同的权威 RPM 软件包映射，
  避免历史别名或过期的 provider 声明导致错误地显示组件已安装
  ([#3158](https://github.com/alibaba/anolisa/pull/3158))。
- 对于仅留下恢复日志的待处理操作，`anolisa forget` 现在会引导执行
  `anolisa repair <component>`，而不是报告没有可遗忘的记录，并保留恢复证据
  ([#3158](https://github.com/alibaba/anolisa/pull/3158))。

## [0.3.10] - 2026-09-06

### 新增

- 现在可通过 `anolisa adapter` 发现、启用、查看和禁用 QwenPaw 插件适配器。
  安装包含 QwenPaw 适配器包的 Tokenless 后，可使用
  `anolisa adapter enable tokenless qwenpaw` 启用。安装和移除均调用 QwenPaw
  CLI，并校验操作后的插件文件；移除失败时保留插件目录和操作凭据以便重试，
  清理时使用启用时记录的工作目录
  ([#3075](https://github.com/alibaba/anolisa/pull/3075))。

### 修复

- `anolisa logs --limit N` 现在按追加顺序选取最近 N 条匹配记录，并在结果中
  将较早的记录排在前面，不再返回最早的匹配记录。因此，默认 50 条的限制在日志
  增长后仍会显示近期活动；扫描已有日志时也不再阻塞新记录的追加
  ([#2618](https://github.com/alibaba/anolisa/pull/2618))。

## [0.3.9] - 2026-09-02

### 新增

- 已应用和失败的 `anolisa update self` 操作现会连同版本与应用模式上下文写入
  中央操作日志。管理员可通过 `anolisa logs` 审计这些操作；生成的 bug report
  仅包含失败记录，并会从已处理的更新 URL 中移除 credential；preview 与已是
  最新版本的 no-op 仍不会写入日志
  ([#2994](https://github.com/alibaba/anolisa/pull/2994))。

### 变更

- 新提供的 Raw repository 配置不再声明未使用的 `cache_ttl_secs` 和
  `offline_fallback` setting。包含任一字段的现有配置仍保持兼容；Raw resolution
  会继续获取当前 distribution index，并在 index 不可用时失败
  ([#3002](https://github.com/alibaba/anolisa/pull/3002))。

### 修复

- 全局与命令级 sandbox uninstall dry-run 现都会显示计划中的 package 变更而不
  实际应用；system mode 的 Telemetry mutation preview 会在权限检查或产生副作用
  前返回
  ([#2922](https://github.com/alibaba/anolisa/pull/2922)、
  [#2926](https://github.com/alibaba/anolisa/pull/2926))。
- 没有 preview 实现的命令现会在 dispatch 前拒绝全局 `--dry-run`。Sandbox
  remove、kernel install 和 security install 会以 `INVALID_ARGUMENT` 拒绝两种
  dry-run 写法，并确认没有执行任何操作，不再进入 mutation、权限检查或
  not-implemented 路径
  ([#2952](https://github.com/alibaba/anolisa/pull/2952)、
  [#2957](https://github.com/alibaba/anolisa/pull/2957)、
  [#2961](https://github.com/alibaba/anolisa/pull/2961))。
- 独立安装器现会检测 PATH 是否仍将 `anolisa` 解析到另一份安装，报告新旧路径
  与版本，并提供可直接执行的 PATH 修复命令或对应的 npm/Homebrew 卸载命令，
  不再输出无条件的成功提示
  ([#2944](https://github.com/alibaba/anolisa/pull/2944))。

## [0.3.8] - 2026-08-26

### 新增

- 带 tag 的 ANOLISA Release 现会提供经过验证的 Linux x64、Linux arm64 和
  macOS arm64 CLI 预编译归档。用户可直接从 GitHub Release 下载各受支持 target
  的独立 binary 归档
  ([#2883](https://github.com/alibaba/anolisa/pull/2883))。

### 修复

- Raw 安装现会在渲染 file content 时保留供 shell 与 systemd 消费的 `${VAR}`
  reference，同时继续展开嵌套的 ANOLISA layout placeholder，并拒绝 destination
  path 中的 environment reference。`anolisa install cosh-ng --backend raw` 现可安装
  gateway service template，不再将其由 `EnvironmentFile=` 提供的 workspace
  reference 误判为 unknown placeholder
  ([#2903](https://github.com/alibaba/anolisa/pull/2903))。

## [0.3.7] - 2026-08-25

### 变更

- 源码构建与 RPM 构建现要求 Rust 1.93，rustup toolchain 固定为 1.93.1，
  Cargo dependency resolution 同时受声明的 MSRV 约束。构建者可使用
  Alibaba Cloud Linux 4 打包的最新 compiler，且 Cargo 不会选择超出受支持
  compiler 的 dependency；更早的 Rust toolchain 需要升级
  ([#2810](https://github.com/alibaba/anolisa/pull/2810))。

### 修复

- `anolisa --dry-run restart <component>` 现会列出将要重启的 unit，但不会调用
  `systemctl daemon-reload` 或 `systemctl restart`。System mode 预览会读取已记录
  状态而不获取 exclusive install lock，因此不再要求对 state root 拥有写权限
  ([#2774](https://github.com/alibaba/anolisa/pull/2774))。

## [0.3.6] - 2026-08-22

### 修复

- `anolisa --quiet adapter scan` 和 `anolisa --quiet adapter status` 现会
  抑制所有非错误的人类可读输出，包括空状态提示与结果表格；`--json` 仍会输出
  标准响应封装。Agent 可依赖 quiet 模式的 adapter 检查不产生人类可读输出
  ([#2752](https://github.com/alibaba/anolisa/pull/2752))。
- `anolisa --dry-run forget <component>` 现会与真实执行一样，以
  `INVALID_ARGUMENT`、退出码 2 和 `adapter disable` 指引，拒绝仍有已启用
  adapter 的 component。预览不再误报无法执行的 forget 会成功，同时仍会忽略
  无关 component 的 adapter receipt
  ([#2762](https://github.com/alibaba/anolisa/pull/2762))。

## [0.3.5] - 2026-08-20

### 修复

- 卸载声明 bare systemd service template 的组件时，ANOLISA 现会通过
  `name@*.service` 停止所有已加载实例，再禁用声明的 `name@.service` template。
  基于 template 的 service 不会在 `anolisa uninstall` 后继续运行；单个实例停止
  失败仍会以 warning 呈现，而不会阻止后续清理
  ([#2603](https://github.com/alibaba/anolisa/pull/2603))。

## [0.3.4] - 2026-08-19

### 变更

- 面向组件的命令现将 repository component index 作为 local state 中不存在的
  名称的唯一身份权威。已安装和 recovery identity 在离线时仍可使用；不支持的
  名称返回 `INVALID_ARGUMENT`，index 不可用返回 `EXECUTION_FAILED`，而
  `NOT_INSTALLED` 现可明确表示受支持的组件尚未安装。`--repo` override 同时决定
  整个 invocation 的 identity 与 package selection，因此 site-local package
  mapping 和 RPM `Provides` metadata 不再能创建 index 未识别的组件名称
  ([#2637](https://github.com/alibaba/anolisa/pull/2637))。

### 修复

- 本地 Raw repository index 缺失时，错误信息现会指出 active repository 来自
  具体的 `repo.toml` path 还是一次性的 `--repo` override，并提供对应的 recovery
  guidance。用户无需再猜测缺失 repository 由哪个配置来源指定
  ([#2650](https://github.com/alibaba/anolisa/pull/2650))。

## [0.3.3] - 2026-08-18

### 新增

- Telemetry instance snapshot 现会将检测到的 Docker、Podman、containerd、
  Kubernetes cgroup 或 LXC runtime 写入 `instance.container`，bare-metal host
  则省略该字段。这为下游 deployment statistics 与 troubleshooting 提供
  container-aware signal，同时不会采集 container 或 pod identity
  ([#2642](https://github.com/alibaba/anolisa/pull/2642))。

### 变更

- `anolisa status <component>` 现会依据 component index 验证新 target、解析
  package alias，并为不支持的名称提示 `anolisa list`，同时将 telemetry service
  target 引导至 `anolisa telemetry status`。Repository metadata 不可用时，仍可
  检查已精确记录的 installed identity
  ([#2626](https://github.com/alibaba/anolisa/pull/2626))。

### 修复

- Raw adapter bundle 安装现会保留 archive 中每个 file 的 mode，并记录 effective
  mode 供 integrity check 使用。Framework hook 与 script 不再统一按 data file
  安装，可继续保留 executable bit
  ([#2619](https://github.com/alibaba/anolisa/pull/2619))。

## [0.3.2] - 2026-08-17

### 新增

- ANOLISA 现为 plugin bundle 提供原生 DSH adapter driver。
  `anolisa adapter enable <component> dsh --profile <name>` 支持重复指定
  profile、验证 bundle identity、将 profile 变更委托给 DSH，并记录 enable
  时的 DSH home，使 status、disable 和 re-enable 在 `DSH_HOME` 或 working
  directory 变化后仍针对相同 profile。降级到更早的 ANOLISA release 前需先
  disable DSH adapter
  ([#2580](https://github.com/alibaba/anolisa/pull/2580))。
- `anolisa logs --level <LEVEL>` 现为已有 `--severity` option 的可见别名，
  使用相同的 validation 和 filtering behavior，同时 `severity` 仍为 canonical
  JSON field
  ([#2558](https://github.com/alibaba/anolisa/pull/2558))。

### 变更

- `anolisa list` 和 `anolisa install --all` 现依据 schema v2
  `components-v2.toml` 中精确的 OS 与 architecture target 判断 component
  availability。JSON output 以 `targets` 和 `target_available` 取代
  `platforms` 和 `platform_available`；repository publisher 必须在保持 v1
  index 不变的同时部署 v2 index
  ([#2533](https://github.com/alibaba/anolisa/pull/2533))。

### 修复

- `anolisa --dry-run install` 现优先读取 resolved Raw artifact 旁的
  `meta.toml`，仅在该 sibling file 不存在时回退到 version-level metadata。
  Preview 现会验证所选 target 的 contract，并拒绝损坏的已发布 metadata，
  不再掩盖错误，同时仍不会下载 artifact
  ([#2551](https://github.com/alibaba/anolisa/pull/2551))。
- System-helper status 现会在 `systemctl` 无法启动时报告 `unknown`，仅在 unit
  的实际状态为 failed 时报告 `failed`
  ([#2604](https://github.com/alibaba/anolisa/pull/2604))。

## [0.3.1] - 2026-08-13

### 修复

- Adapter discovery 现会忽略未声明的共享 resource directory，除非 contract、
  receipt 或内置 framework driver 将其识别为实际 adapter。Tokenless common hook
  等共享 asset 不再于 adapter scan 和 status output 中显示为不支持的 framework
  ([#2502](https://github.com/alibaba/anolisa/pull/2502))。

## [0.3.0] - 2026-08-12

### 修复

- Adapter enable、status 和 update 现基于 ANOLISA-owned Raw file 或 native
  package metadata 派生 adapter revision，不再对整个 resource tree 计算 hash。
  Runtime cache 与其他 unowned file 不再造成错误 drift，也不会被复制到 framework；
  已变化的 package-owned input 会在 framework mutation 前阻止 enable，metadata
  不可用时则报告为 `unknown` 状态
  ([#2419](https://github.com/alibaba/anolisa/pull/2419))。
- Re-enable adapter 现仅删除旧 receipt 记录的 stale materialized file，保留
  runtime-created file，通过 `--dry-run` 预览 cleanup，并在 directory-to-file
  replacement 会丢弃 runtime data 时保留旧 receipt
  ([#2438](https://github.com/alibaba/anolisa/pull/2438))。

## [0.2.20] - 2026-08-11

### 变更

- `anolisa list` 现显示检测到的 host platform，并在 human-readable output 中以
  component availability 取代 backend 和 ownership column。不支持当前 host 的
  component 仍会显示其支持的 platform，且不提供 install action；JSON 新增
  `platforms` 和 `platform_available`，同时保留 backend 与 ownership metadata
  ([#2367](https://github.com/alibaba/anolisa/pull/2367))。

### 修复

- npm 安装现由 `@anolisa/cli` 独占公开的 `anolisa` executable。在 npm 10 下，
  本地安装可稳定创建 `node_modules/.bin/anolisa`，不再因 platform package 参与
  链接而丢失该 command
  ([#2345](https://github.com/alibaba/anolisa/pull/2345))。

## [0.2.19] - 2026-08-10

### 修复

- 在 Raw 安装准备系统依赖时，现会将 resolver 提供的 `rpm` 和 `deb`
  package-family hint 直接映射到相应的 package manager backend。在缺少可选
  `which` command 的最小化受支持主机上，不再仅因此报告不支持 package base，
  同时保持对 distro-specific hint 的兼容
  ([#2314](https://github.com/alibaba/anolisa/pull/2314))。
- `anolisa --json osbase sandbox list` 和
  `anolisa --json register status` 现使用标准 success envelope，包含 `ok`、
  `schema_version` 和 `command` metadata，并将业务字段嵌套在 `data` 下。脚本现可
  与其他 JSON surface 一致地解析这些 legacy command
  ([#2319](https://github.com/alibaba/anolisa/pull/2319))。
- OpenClaw adapter 现以 OpenClaw 兼容的 whitespace、tilde 和 absolute-path
  处理方式遵循 `OPENCLAW_STATE_DIR`，使 plugin、skill、receipt、status 和
  disable 操作都使用配置的 state。Re-enable 会安全迁移 legacy fallback 下记录的
  resource，在 cleanup 需重试时保留旧 receipt，并在 `--dry-run` 中预览迁移。若旧
  receipt 使用的 `OPENCLAW_HOME` 当前已不在 environment 中，迁移或 cleanup 前需
  临时恢复该变量
  ([#2337](https://github.com/alibaba/anolisa/pull/2337))。

## [0.2.18] - 2026-08-06

### 变更

- Telemetry 上传现将 `SLS_PROJECT_PREFIX` 视为 SLS project prefix，并附加
  检测到的 region，例如 `anolisa-cn-hangzhou`。设置旧 `SLS_PROJECT` 的部署
  必须迁移至 `SLS_PROJECT_PREFIX`，以便将数据上传到相应 region 的 project
  ([#2260](https://github.com/alibaba/anolisa/pull/2260))。

### 修复

- Raw 安装现仅将选中的 archive payload 通过私有、disk-backed staging 流式
  处理，不再将解压后的内容保留在内存中。大型 package 可以使用有界的 payload
  内存完成安装，同时保留 atomic placement、rollback、cleanup 和 digest
  verification
  ([#2250](https://github.com/alibaba/anolisa/pull/2250))。
- `anolisa status` 和 `anolisa doctor` 现会 hash 最大 2 GiB 的 ANOLISA-owned
  file，并将更大的 file 视为未检查和 degraded，而不是 failed。包含大型 artifact
  的完整组件不再显示为损坏或触发多余的 repair，同时在必须完成 verification 的
  recovery 场景中仍会 fail closed
  ([#2271](https://github.com/alibaba/anolisa/pull/2271))。
- Enable 声明了 hook 的 Codex adapter 时，现会发现已安装 plugin 的 hook
  identity，并以 atomic 方式持久化其 trusted hash，使 non-interactive
  `codex exec` session 可以运行这些 hook。缺失 hook 或被覆盖的 trust setting
  会以可操作的诊断信息中止 enable
  ([#2281](https://github.com/alibaba/anolisa/pull/2281))。

## [0.2.17] - 2026-08-05

### 新增

- Raw 安装现可在放置声明的文本文件前渲染其中的 `{bindir}`、`{datadir}` 等
  layout placeholder，使共享的软件包模板遵循所选安装 scope 与 prefix。
  完整性检查和 repair 使用渲染后的字节
  ([#2222](https://github.com/alibaba/anolisa/pull/2222))。

### 变更

- Raw repository 解析现会在已发布时优先使用第二代 index，并强制检查各组件的
  CLI 最低版本。不兼容的条目会给出 `anolisa update self` 提示并失败，而不会
  静默安装较旧或格式错误的结果，同时保持对第一代 repository 的兼容
  ([#2222](https://github.com/alibaba/anolisa/pull/2222))。
- RPM-backed adapter 的 scan、status 和 enable 操作现使用声明的软件包自有
  resource root；root 缺失或无效时会明确报告，而不会回退到过期的 raw 文件。
  目标位于外部 RPM root 的 Codex adapter 会记录 trust anchor；降级到
  `0.2.16` 前需先 disable 这些 adapter
  ([#2222](https://github.com/alibaba/anolisa/pull/2222))。

### 修复

- Qoder native plugin bundle 现使用 Qoder 自身的 plugin lifecycle，不再按
  legacy hook bundle 复制或改写。User scope 和 project scope 中已有的同 ID
  plugin 会受到保护；无法确认的安装或移除会保留可重试的 receipt，而不会认领或
  删除用户状态
  ([#2221](https://github.com/alibaba/anolisa/pull/2221))。

## [0.2.16] - 2026-08-03

### 新增

- 成功执行的 `anolisa update <component>` 和 `anolisa update all` 现会报告
  resource bundle 已变化的 adapter，并给出准确的 `anolisa adapter enable ...`
  或 `anolisa adapter status ...` 后续命令。JSON 响应通过稳定的
  `adapter_actions` 数组提供同样的信息
  ([#2018](https://github.com/alibaba/anolisa/pull/2018))。

### 修复

- 在同时缺少 RPM 工具和 RPM database 的 Debian 系发行版上，system scope
  raw 安装不再因 `rpm not found on PATH` 而失败。如果已有 RPM database，
  或安装期间新出现 RPM database，仍会在任何文件变更前停止 raw 安装
  ([#2061](https://github.com/alibaba/anolisa/pull/2061))。

## [0.2.15] - 2026-07-30

### 新增

- 交互式 `anolisa install`、`anolisa install --all` 和 `anolisa uninstall`
  现会在耗时的规划与执行阶段显示分阶段进度。支持 ANSI 的终端会动态显示当前阶段，
  能力受限的交互式终端则输出静态阶段提示
  ([#2036](https://github.com/alibaba/anolisa/pull/2036))。

### 变更

- 面向用户的失败信息现采用常规的 `error:` 和 `hint:` 标签，不再显示机器错误码；
  `--json` 仍保留结构化错误码，退出状态保持不变。
- 更新通知现会为建议运行的 `sudo anolisa upgrade` 和 `anolisa update --check`
  命令加上引号，使命令边界更加清晰。

## [0.2.14] - 2026-07-29

### 修复

- `anolisa status` 和 `anolisa doctor` 现可检测 raw 托管文件的 Unix mode 与
  Linux file capability 漂移（包括由旧版本记录的安装），并建议运行
  `anolisa repair` 进行恢复。
- `anolisa repair` 现会在仅文件元数据漂移时重新部署 raw 托管组件，恢复声明的
  mode 和已确认的 capability。更新失败后的回滚仅恢复操作前已确认生效的
  capability，避免授予原安装中未成功应用的可选 capability
  ([#1987](https://github.com/alibaba/anolisa/pull/1987))。

## [0.2.13] - 2026-07-28

### 新增

- `@anolisa/cli` npm 软件包现支持 macOS arm64，并在安装时选择匹配的原生二进制。
- Tokenless adapter 现支持 Qwencode，同时保持 Cosh extension 与共享 hook 资源相互独立。

### 修复

- raw 安装现拒绝 provision 被另一个 pending RPM 安装占用的系统软件包，并引导用户运行
  `anolisa repair`，避免组件相互占用或在后续移除对方的依赖。
- `cosh-ng` RPM 安装现保留 `cosh-ng` 组件身份。以 `cosh` 保存且可明确识别的旧记录和
  recovery journal 会被修复，使 lifecycle 命令操作正确的组件。
- raw 更新和修复失败后的回滚现会恢复文件权限和 capability，确保恢复的二进制仍可执行。
- 启用 Tokenless Qoder adapter 时，现会解析缓存 plugin 中的共享 hook 路径，避免匹配的 tool call 因 hook 命令路径错误而失败。

## [0.2.12] - 2026-07-27

### 变更

- 对已安装组件执行操作的命令在目标缺失时现报告 `NOT_INSTALLED`，不再报告
  `INVALID_ARGUMENT`，调用方无需解析错误消息即可区分“没有可操作的目标”和“调用方式错误”。
  该错误码仅表示状态缺失，不表示组件名称是否有效；影响 `uninstall`、`update`、`repair`、
  `forget`、`restart` 和 `adapter`，退出码仍为 2
  ([#1915](https://github.com/alibaba/anolisa/pull/1915))。

### 修复

- adapter 状态检查现忽略空的或不完整的过期源目录，并将缺失 bundle 报告为 degraded；
  raw 卸载会清理空目录，避免遮蔽其他安装 scope
  ([#1850](https://github.com/alibaba/anolisa/pull/1850))。
- raw 安装 dry-run 现会在执行前校验组件冲突，使预览结果与实际安装保持一致。仓库缺少
  轻量 sidecar 元数据时会提示已跳过冲突校验
  ([#1898](https://github.com/alibaba/anolisa/pull/1898))。

## [0.2.11] - 2026-07-24

### 新增

- raw `anolisa install --version` 现可安装指定的已发布组件版本。
- raw `anolisa install --version` 输出现显示请求版本、解析版本、制品地址和来源。

### 变更

- 请求的 raw 版本不可用时，`anolisa install --version` 现列出已发布版本。

### 修复

- 请求的 raw 版本不可用时，`anolisa install --version` 不再安装其他版本。
- 包含大量文件的 raw 组件卸载速度更快，写入量显著降低。
- 操作恢复数据缺失或损坏时，恢复流程现会保留已安装状态。

## [0.2.10] - 2026-07-23

### 新增

- `anolisa telemetry` 现可启用或停用数据收集。
- `anolisa telemetry` 现可关联或取消具名上报。
- `anolisa telemetry status` 现以文本或 JSON 显示收集和具名上报状态。
- adapter 启用和停用命令现显示组件提供的后续提示。
- adapter JSON 输出现包含结构化组件提示。
- `anolisa install --version` JSON 输出现包含请求版本、解析版本、来源及精确 RPM。

### 变更

- 全新 ANOLISA RPM 安装现默认启用匿名遥测。
- RPM 安装输出现说明如何停用遥测。
- 已启用的遥测现会在支持的主机重启后自动恢复。
- `anolisa register` 现提示该命令已弃用。
- `anolisa register` 现无需确认即可启用遥测。
- `anolisa register status` 现引导使用 `anolisa telemetry status`。
- `anolisa unregister` 现停用遥测并保留本地日志。
- `anolisa install --version` 现精确选择与请求版本匹配且兼容本机的 RPM。
- `anolisa install --dry-run --version` 现验证可用性并显示解析后的 RPM 详情。
- adapter dry-run 现预览组件提示且不更改主机。
- adapter quiet 输出现隐藏组件提示。

### 修复

- `anolisa register` 现避免旧遥测配置重复上传。
- `anolisa unregister` 不再让旧遥测配置继续上报。
- `anolisa install --version` 在请求版本不可用或不兼容本机时不再更改主机。
- `anolisa install --version` 安装到其他 RPM 版本时不再记录成功。
- `anolisa repair` 现拒绝已装版本偏离原请求的中断 RPM 安装。
- `anolisa repair` 现报告中断 RPM 安装的架构无法验证。
- `anolisa adapter disable` 现可在组件文件不可用时显示已保存提示。
- adapter 提示不再能向可读输出注入终端格式。

## [0.2.9] - 2026-07-22

### 新增

- `anolisa update all` 现更新全部已跟踪的 raw 与 RPM 组件，不更新 CLI。
- `anolisa repair` 现可按记录版本恢复损坏的 raw 安装。
- `anolisa repair` 现支持无 root 权限修复 user scope 安装。
- `anolisa repair` 现可恢复中断的安装、更新、采纳、卸载和批量操作。
- `anolisa repair` 现可重装缺失的托管 RPM 软件包。
- `anolisa status` 现将不明旧记录标为需处理，并给出按 scope 修复或遗忘的指引。
- `anolisa status` 与 `anolisa doctor` 现按安装时保存的组件清单执行健康检查。
- user mode 的 adapter 命令现可作用于可见的 system 安装。
- `anolisa repair` 现可根据已安装软件包或完好文件恢复不明旧记录。
- `anolisa forget` 现可仅删除不明旧记录，不改动已安装内容。

### 变更

- `anolisa install` 遇到未托管的 system RPM 时现会拒绝，并提示使用 `anolisa adopt`。
- 已跟踪组件再次执行 `anolisa install` 时现直接成功，不再重复安装。
- `anolisa adopt` 现允许更新既有 RPM，卸载软件包仍需显式授权。
- 已采纳软件包再次执行 `anolisa adopt` 时现直接成功。
- 仅观察的 RPM 组件现须先执行 `anolisa adopt` 才能更新。
- `anolisa install --all` 现用一次软件包事务安装全部新 RPM。
- `anolisa upgrade` 现用一次软件包事务完成全部 RPM 更新。
- `anolisa upgrade` 现用一次软件包事务安装全部计划内 RPM。
- `anolisa list` 与 `anolisa status` 现分别显示同名组件的 user 和 system 记录。
- `anolisa list` 现将记录标为 owned、managed、adopted 或 observed。
- `anolisa --install-mode user install` 现可在 system 安装旁创建独立 user 安装。
- 生命周期修改现严格限制在所选 scope，软件包别名也不例外。
- 首次修改旧状态时现自动升级格式，并保留 `installed.toml.v4.bak`。
- 遇到新版状态格式时现会报错，不再显示为空状态。
- `install-anolisa.sh` 现由 CLI 获取分发索引，以使用镜像最新数据。
- `install-anolisa.sh` 现仅部署 OS-base 清单，组件清单改为按需获取。
- `install-anolisa.sh --strict` 现仅校验二进制和清单包。
- `ANOLISA_INDEX_URL` 与 `ANOLISA_INDEX_SHA256` 现不再影响安装脚本。
- 安装、采纳、更新、修复和卸载的 JSON 输出现包含明确执行计划。
- raw 与 RPM 组件的卸载 JSON 现统一格式，并包含删除方式和计划。
- `anolisa uninstall --dry-run` 遇到缺失组件时现会报错，不再返回空成功计划。
- 组件存在待恢复操作时，`anolisa forget` 与 `anolisa restart` 现会停止。
- `anolisa doctor` 现会报告没有活动组件记录的未完成操作。

### 修复

- RPM 托管组件不再因 raw 安装健康检查被 `status` 或 `doctor` 误报失败。
- RPM 组件更新现会先刷新已保存组件清单，再报告成功。
- RPM 清单刷新未完成时，`anolisa logs --severity warn` 现可检索该操作。
- RPM 更新中断后现保持可修复，不再以过期设置显示成功。
- `anolisa doctor` 遇到损坏或不明确的恢复数据时不再建议生命周期命令。
- 多个组件共用状态目录时，`anolisa doctor` 不再重复报告恢复问题。
- `anolisa doctor --help` 现明确说明 `--fix` 尚不可用。
- 批量 RPM 操作失败后，已变更软件包现会保留可修复状态。
- 组件别名不再将生命周期修改导向其他 scope 的安装。
- 跨 scope 健康检查现使用正确的 user 服务管理器。
- 批量 RPM 操作失败后，未受影响组件现会单独重试。

## [0.2.8] - 2026-07-21

### 新增

- `anolisa adapter enable` 现支持以 `--allow-unsafe-plugin-install` 显式授权 OpenClaw 不安全插件安装。
- OpenClaw 适配器设置现可限定适用的 OpenClaw 版本。

### 变更

- `anolisa adapter enable` 现会在执行任何更改前检查 OpenClaw 兼容性。
- `anolisa adapter enable` 现会确认 OpenClaw 插件已加载后再报告成功。
- OpenClaw 阻止不安全插件时，ANOLISA 现会显示检查结果。
- OpenClaw 支持显式授权时，安全错误现会提示授权重试。

### 修复

- OpenClaw 设置更新失败后，现可保留受影响设置供重试。
- 重新启用 OpenClaw 适配器时，不再丢失此前已应用的设置记录。
- `anolisa adapter disable` 现会提示更新结果不确定且可能残留的 OpenClaw 设置。

## [0.2.7] - 2026-07-18

### 新增

- `anolisa adapter` 现可通过 `qwen` CLI 管理 Qwen Code 0.17 及更高版本的扩展。

### 变更

- `anolisa upgrade` 和 `anolisa repair` 现会在人类可读和 JSON 输出中说明组件清单同步。

### 修复

- `anolisa upgrade` 现会在 RPM 软件包升级后刷新组件清单。
- `anolisa upgrade` 现可同步版本号未变的 RPM 组件清单变更。
- `anolisa repair` 现会从已安装的 RPM 刷新过期组件清单。
- 组件清单刷新失败时，现会保持 RPM 组件可修复并报告受影响组件。

## [0.2.6] - 2026-07-16

### 修复

- `anolisa status` 不再误报正常 RPM 组件失败。

## [0.2.5] - 2026-07-14

### 新增

- `anolisa repair` 现可恢复软件包安装后中断的首次 RPM 安装。
- `anolisa update --check` 现会报告已保存状态需要同步的 RPM 组件。

### 变更

- 安装、接管和升级命令现会先要求修复中断的 RPM 安装。

### 修复

- 并发 RPM 安装现会安全失败，不再覆盖其他操作的组件状态。
- 重装缺失的 ANOLISA 托管 RPM 时，现会保留组件设置和历史记录。
- `anolisa uninstall --dry-run --json` 现包含 `dry_run: true`，且未安装组件不再显示删除阶段。
- `anolisa upgrade` 现会在升级后刷新已保存的 RPM 版本和软件包信息。
- `anolisa upgrade` 现可同步缺少软件包信息的旧版 RPM 记录。

## [0.2.4] - 2026-07-13

### 新增

- 交互终端中，`anolisa update --check` 现会在检查更新时显示进度。
- 交互终端中，`anolisa upgrade` 现会在规划和执行升级时显示进度。

### 修复

- Raw 组件安装和更新现可在同时存在二进制发布包时选中可安装归档包。

## [0.2.3] - 2026-07-12

### 变更

- 软件包安装和卸载进度现输出至标准错误，避免干扰重定向结果。

### 修复

- 下游管道提前关闭标准输出时，ANOLISA 命令现可正常退出。
- 标准输出写入失败时，ANOLISA 命令现会报错而非静默成功。

## [0.2.2] - 2026-07-09

### 新增

- `anolisa update --check` 现可只读报告 RPM 升级机会。
- `anolisa update --check --motd` 现可输出简短登录升级提示。
- `anolisa upgrade` 现可应用 RPM 工具链升级。
- `anolisa upgrade` 现可安装目标配置缺失默认组件。
- `anolisa adapter scan` 现将缺失来源的启用记录标为 orphaned。
- `anolisa adapter status` 现将缺失适配器来源报告为降级。

### 变更

- `anolisa list` 现显示可见用户和系统记录的 scope。
- `anolisa status` 现显示 scope、可变性、遮蔽和状态路径。
- `anolisa doctor` 现在用户模式诊断可读系统组件。
- `anolisa doctor` 现为只读系统记录建议系统模式命令。
- `anolisa update --check` 未指定 `--target` 时使用最新目标配置。
- `anolisa update --check --motd` 现提示用 `sudo anolisa upgrade`。

### 修复

- `anolisa uninstall`、`forget`、`update` 现拒绝只读系统目标。
- `anolisa upgrade` 现将未解析默认组件报告为检查错误。
- `anolisa upgrade` 刷新 RPM 详情失败时现会提示。

## [0.2.1] - 2026-07-08

### 新增

- `anolisa adapter enable` 现支持 cosh、Codex、Claude Code 的 Tokenless 适配器。
- `anolisa adapter enable` 现支持 Qoder Tokenless 适配器。

### 变更

- Claude Code 适配器现使用组件专属 marketplace。
- `anolisa adapter enable` 现先拒绝无效适配器类型。

### 修复

- Codex 适配器现可使用已打包数据目录资源。
- Qoder 启用遇到损坏 `settings.json` 时不再覆盖。
- Qoder 禁用现只移除 ANOLISA 添加的 hook。
- Qoder 适配器现优先使用稳定版 qodercli。

## [0.2.0] - 2026-07-07

### 新增

- Raw 组件现可声明 `conflicts` 阻止不兼容安装。

### 修复

- `anolisa install` 现会在变更主机前拒绝 Raw 组件冲突。
- `anolisa install --dry-run` 现报告 Raw 组件冲突，不再显示无效计划。

## [0.1.20] - 2026-07-03

### 新增

- ANOLISA 现可通过 `@anolisa/cli` 发布 Linux x64 和 arm64 二进制。
- `repo.toml` 现启用 npm 后端用于组件分发。

### 变更

- `anolisa list` 现显示本地状态、归属和下一步操作。
- `anolisa list --json` 现包含 RPM 包名、版本、架构和来源。

### 修复

- RPM 安装和更新现可继续使用系统软件源解析依赖。
- Adapter 命令现区分缺失清单和无效清单。

## [0.1.19] - 2026-07-02

### 修复

- `anolisa adapter disable --dry-run` 现只预览清理。
- 只读命令保存 `repo.toml` 失败时仍可使用已下载配置。
- 组件命令现可一致接受软件包别名。
- 模糊软件包别名不再误选已安装组件。
- 未知组件名不再先查询软件包。

## [0.1.18] - 2026-07-01

### 新增

- `anolisa install` 系统模式现自动安装缺失系统包。
- `anolisa install --dry-run` 现标出依赖处理方式。
- `anolisa install` 现显示自动安装的系统包。
- `anolisa status --verbose` 现显示组件自动安装包。

### 变更

- 需要仓库的命令现首次使用会下载 `repo.toml`。
- 仓库配置 dry-run 现只校验不写入。
- RPM 安装和更新现只使用 `repo.toml` 源。
- 用户模式 raw 安装现先提示缺失依赖。
- raw 安装失败现提示已保留的自动安装包。
- `anolisa update self` 不再预先获取仓库配置。

### 修复

- `anolisa list --installed` 现包含已收编 RPM。
- `anolisa list` 现显示 adopted、failed、disabled 状态。
- Adapter 命令现优先使用契约所在数据目录资源。
- 缺少 `[backends.rpm]` 时 RPM 安装不再调用 `dnf`。
- RPM 更新缺少 `[backends.rpm]` 时不再使用主机源。

## [0.1.17] - 2026-06-30

### 新增

- 仓库 `components.toml` 现可声明组件与包名映射。
- `anolisa list --installed` 现过滤已安装组件。

### 变更

- `anolisa list` 和 `install --all` 现读取 `components.toml`。
- `anolisa list` 现显示 NAME、SUMMARY、BACKENDS、STATUS。
- `anolisa list --enabled` 现作为隐藏别名保留。
- `ANOLISA_CATALOG_URL` 不再控制列表来源。
- `install`、`status`、`adopt`、`repair` 现解析 RPM 包别名。
- `anolisa status` 现提示用 `sudo anolisa adopt` 收编 RPM。

### 修复

- `status <RPM 包>` 现显示规范组件行。
- `repair <RPM 包>` 现刷新规范组件行。
- 非 root `osbase` 变更命令现可进入系统 helper。
- root 用户模式现会在写入前被拒绝。
- 缺少 sudo 的系统模式写入现提前失败。
- 旧符号链接安装不再误报完整性失败。
- `status` 现报告符号链接目标不匹配。

## [0.1.16] - 2026-06-29

### 新增

- `anolisa osbase sandbox install runc` 现安装 runc、containerd、Docker 和客户端。
- `anolisa osbase sandbox install` 现启用场景声明的服务。
- `anolisa osbase sandbox install` 现执行场景安装校验。
- `anolisa osbase sandbox install` 现记录沙箱安装状态。
- `anolisa osbase sandbox install` 现提示可选场景包。
- `rund`、`firecracker`、`gvisor` 场景现声明安装校验。
- `anolisa adapter enable` 现支持 `adapter_type = "skill_bundle"`。
- RPM 包现安装默认 `/etc/anolisa/repo.toml`。
- ANOLISA 遥测现为 `.jsonl` 运维日志配置轮转。

### 变更

- `anolisa osbase sandbox install --dry-run` 现显示五个安装阶段。
- `anolisa osbase sandbox install runc` 现要求 Linux 4.18 及以上。
- `anolisa osbase sandbox install` 校验失败现作为警告报告。
- `repo.toml` 默认 RPM 源现指向 agentic-os 路径。
- `anolisa update self --json` 现包含 RPM 包和版本信息。
- `anolisa adapter status` 现不要求技能包注册插件。
- `anolisa adapter enable` 现拒绝带配置的技能包。

### 修复

- RPM 相关命令现默认用组件名作为包名。
- `anolisa update self` 现通过 `dnf` 更新 RPM 安装。
- 非 root 沙箱安装现显示完整阶段结果。
- ilogtail 安装脚本需 bash 时现能正常运行。
- `anolisa adapter disable` 清理技能包时不再报插件卸载错误。

## [0.1.15] - 2026-06-25

### 新增

- `anolisa doctor` 现输出组件健康、依赖和修复建议。
- raw 组件现可声明运行依赖供安装和更新检查。
- `anolisa install --dry-run` 现预览 raw 组件依赖状态。

### 变更

- `anolisa install` 和 `update <component>` 缺依赖时先停止。
- `anolisa restart <component>` 现重启 RPM 组件服务。
- `anolisa restart <component>` 遇到 RPM 模板服务时给出指引。

### 修复

- `anolisa adapter enable` 现按包内元数据展开 `{datadir}`。
- `anolisa uninstall` 和 `forget` 后 adapter 不再见旧元数据。

## [0.1.14] - 2026-06-24

### 新增

- raw 组件现可用 `{unitdir}` 放置系统单元。
- raw 组件现可用 `{userunitdir}` 放置用户单元。
- 用户模式 `anolisa install` 现会激活用户服务。

### 变更

- 用户模式 `anolisa install` 现将 `%u` 展开为当前用户。
- 系统模式 `anolisa install` 现保留 `%u` 用户模板。
- `anolisa uninstall` 删除单元文件后现会重载 systemd。
- `anolisa restart <component>` 现会重启用户服务。

### 修复

- `anolisa install` 现无需手动重载即可启动新单元。
- `anolisa uninstall` 现会停用用户模式安装的服务。
- `anolisa adapter enable` 现从包目录查找 `{datadir}` 技能。

## [0.1.13] - 2026-06-23

### 新增

- `anolisa adapter enable` 现支持 Hermes 插件。
- `anolisa adapter enable` 现安装声明的 OpenClaw 技能。
- `anolisa adapter enable` 现写入声明的 OpenClaw 配置。
- `anolisa install` 现启动 raw 组件声明的服务。
- `anolisa install` 现设置 raw 组件声明的文件能力。
- `anolisa install` 现执行 raw 组件声明的钩子。
- `anolisa update <component>` 现重启 raw 组件声明的服务。
- `anolisa update <component>` 现重设 raw 组件声明的文件能力。
- `anolisa uninstall` 现执行 raw 组件卸载钩子。
- `anolisa uninstall` 现停用已停止的声明服务。

### 变更

- `anolisa adapter scan` 现按声明位置查找资源。
- `anolisa adapter enable` 现读取包内适配器资源。
- `anolisa install --dry-run` 现预览 raw 文件能力。
- `anolisa register status` 现显示最新注册记录。
- 取消 `anolisa register` 或 `unregister` 不再报错。

### 修复

- `anolisa adapter status` 现能识别换行的 OpenClaw 表格。
- `anolisa adapter status` 检查 Hermes 时忽略内置插件。
- `anolisa adapter` 现能找到 RPM 组件附带的元数据。
- `anolisa register status` 现显示 sysom 控制台注册。

## [0.1.12] - 2026-06-22

### 新增

- `anolisa update <component>` 可更新 raw 组件。
- `anolisa osbase sandbox list` 可显示 `sandbox.toml` 场景。
- `anolisa osbase sandbox uninstall <scenario>` 可移除场景软件包。
- `anolisa system setup` 可为非 root osbase 命令安装助手服务。
- `anolisa system status` 可显示助手健康状态。
- `anolisa system teardown` 可移除助手服务和沙箱配置。
- `anolisa env --json` 现包含发行版身份字段。

### 变更

- `anolisa osbase sandbox install <scenario>` 现按 `sandbox.toml` 安装场景。
- 未指定 `--install-mode` 时，root 用 `system`，普通用户用 `user`。
- `anolisa update <component> --dry-run` 现显示 raw 候选版本。

### 修复

- 旧 `yum` 后端名现会作为 `rpm` 处理。
- 用 `--package` 安装的 raw 组件更新时复用包名。
- `anolisa update <component>` 不再允许 raw 降级。
- `anolisa update <component>` 无法比较版本时不再替换文件。

## [0.1.11] - 2026-06-18

### 新增

- `anolisa adopt <component>` 可接管预装 RPM。
- `anolisa repair <component>` 可刷新漂移的 RPM 状态。
- `anolisa forget <component>` 可停止跟踪组件。

### 变更

- `anolisa status <component>` 现报告 RPM 状态漂移。
- `anolisa uninstall` 默认保留观察到的系统 RPM。
- `anolisa install` 接管 RPM 时保留适配器资源。

## [0.1.10] - 2026-06-17

### 新增

- `anolisa install --backend rpm` 可通过 `dnf` 安装缺失 RPM 组件。
- `anolisa install` 可接管匹配的预装系统 RPM。
- `anolisa update <component>` 可通过 `dnf` 更新 RPM 组件。
- `anolisa status` 现显示 RPM 组件的软件包来源。
- `anolisa status <component>` 现显示匹配的未跟踪系统 RPM。

### 变更

- `anolisa update runtime <component>` 改为 `anolisa update <component>`。
- `repo.toml` 现使用 `[backends.rpm]` 替代 `[backends.yum]`。
- `anolisa install --all` 现在批量摘要列出接管的 RPM。

### 修复

- `anolisa install --all` 现在普通输出显示各组件失败原因。
- `anolisa install` 在缺少 `rpm` 或 `dnf` 时提示 `--backend raw`。
- `anolisa install` 不再覆盖先完成的 raw 安装。

## [0.1.9] - 2026-06-16

### 新增

- `anolisa install --all` 可安装目录中的所有可用组件。
- `anolisa install --all --fail-fast` 可在首个失败组件后停止。
- `anolisa install --all --json` 现返回按组件汇总的批量结果。
- `anolisa status` 现显示已安装组件的适配器摘要。

### 变更

- `installed.toml` 现区分 ANOLISA 管理包和只观察的系统 RPM。

## [0.1.8] - 2026-06-15

### 新增

- `anolisa adapter enable` 现可将已安装适配器注册到 OpenClaw。
- `anolisa adapter disable` 现可移除 OpenClaw 适配器注册。
- `anolisa adapter status` 现可报告 OpenClaw 适配器健康状态。
- `anolisa adapter scan` 现可显示已安装适配器资源。

### 变更

- `anolisa install` 现会放置后续启用所需的适配器资源。
- `anolisa uninstall` 现会阻止移除仍有启用适配器的组件。

## [0.1.7] - 2026-06-13

### 变更

- 用户态库路径调整为 `~/.local/lib/anolisa`；其余目录继续遵循 `XDG_*` 环境变量覆盖。

### 修复

- `anolisa install` 不再要求本地已有组件目录条目即可从远程仓库下载安装。
- `anolisa install --dry-run` 无需下载完整安装包即可预览文件和服务列表。

## [0.1.6] - 2026-06-12

### 新增

- `anolisa osbase sandbox install gvisor` 支持 standalone、containerd 和 substrate 三种部署形态。(#851)
- `anolisa list` 可从 `repo.toml` 配置自动发现组件目录。(#854)

### 变更

- 废弃旧版"能力"模型，统一为组件生命周期；旧状态在下次写入时自动迁移。(#876)

### 修复

- `anolisa list --enabled` 现在正确显示已安装组件，而非空列表。(#872)
- `anolisa list` 在已配置 `repo.toml` 时不再要求额外的本地目录文件。(#854)

## [0.1.5] - 2026-06-11

### 新增

- `anolisa list` 从远程或本地组件目录读取并返回结构化 JSON。(#850)
- `anolisa install <组件>` 从远程仓库下载、校验并安装组件。(#852)
- `anolisa uninstall` 支持新组件模型，同时保留旧版回退。(#852)

### 变更

- 简化 CLI 帮助输出，围绕 `list`、`install`、`uninstall`、`status`、`doctor`、`logs`、`restart`、`update` 重新分组。(#850)

### 修复

- 未配置组件目录时，`anolisa list` 返回空列表并提示配置方法。(#850)
- 安装中途失败时自动回滚已写入的文件。(#852)

## [0.1.4] - 2026-06-10

### 新增

- `anolisa adapter scan` 探测已安装的 Agent 框架集成。(#808)
- `anolisa adapter install` 下载校验后的安装包并注册到目标框架。
- `anolisa adapter remove` 安全移除 ANOLISA 管理的文件，支持预览和 dry-run。
- `anolisa adapter install tokenless openclaw` 通过 OpenClaw CLI 注册 tokenless 适配器。
- `anolisa enable` 从远程仓库获取组件元数据，离线时降级到本地缓存。
- `anolisa status` 输出中新增组件健康检查结果。

### 变更

- 订阅管理命令提升为顶层 `anolisa register` / `unregister`。

### 修复

- adapter 安装或移除失败时自动回滚或保留状态以便重试。

## [0.1.3] - 2026-06-09

### 新增

- `anolisa --help` 按类别分组展示命令（日常操作 vs. 管理命令）。
- `list` 命令在帮助中展示 `ls` 别名。
- `anolisa update self` 成功后输出 changelog 链接。

### 变更

- 修正包 license 元数据为 Apache-2.0。

## [0.1.2] - 2026-06-08

### 新增

- `anolisa bug` 生成本地诊断报告，包含环境信息和近期错误日志。
- `anolisa self update` 作为 `anolisa update self` 的别名。

### 修复

- 恢复 bug report issue 模板。

## [0.1.1] - 2026-06-07

### 新增

- `anolisa osbase sandbox install` 一键部署沙箱环境（支持 firecracker 和 e2b 后端）。
- `anolisa register` / `unregister` 管理数据上传授权，支持 30 天延后。
- `anolisa enable` 可配置日志上传（ilogtail），自动探测地域。
- `anolisa update self` 下载并应用 CLI 更新，含完整性校验和失败回滚。
- 真实的 dnf/apt 包管理器后端，替换占位实现。
- anolisa 工作区 GitHub Actions CI。

### 修复

- 安装脚本改用 bash 参数展开替代 `sed`，提升可移植性。

## [0.1.0] - 2026-06-04

ANOLISA CLI 首个 alpha 版本。

### 新增

- CLI 命令：`env`、`list`、`status`、`logs`、`enable`、`disable`、`uninstall`、`restart`、`update`、`info`、`doctor`。
- 环境探测：OS、架构、内核、发行版、容器运行时、用户身份（探测失败时优雅降级）。
- 组件生命周期引擎：先预览再执行，含完整性校验和操作日志。
- 配置驱动的上线门控，新能力无需改代码即可发布。
- 声明式 TOML 组件清单，支持多架构。
- `install-anolisa.sh` 安装器：三种模式（本地、checkout、URL），支持校验和 `--dry-run`。
- agent-observability 和 token-optimization 端到端冒烟测试。

### 已交付能力

| 能力 | 状态 |
|-----|------|
| agent-observability | `enable` 完整链路（dry-run + 真实执行） |
| 其余 9 个 | 仅清单；`enable` 返回 NOT_IMPLEMENTED |

### 已知限制

- 真实执行路径仅限 Linux（darwin 宿主只能 `--dry-run`）。
- 尚无签名校验和 rpm/deb 后端。
- `update` 命令返回 NOT_IMPLEMENTED。
