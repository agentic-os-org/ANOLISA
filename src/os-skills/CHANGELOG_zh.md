# 更新日志

[English](CHANGELOG.md)

本文档记录 OS Skills 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
项目遵循 [Semantic Versioning](https://semver.org/spec/v2.0.0.html)。

## [未发布]

### 新增

- 新增 `mysql-admin` Skill，支持 MySQL/MariaDB 实例管理：processlist 与
  锁等待诊断、死锁分析、连接水位检查、遵循只读优先的慢查询与索引审计、
  基于 binlog 位点的备份/恢复治理，以及复制与表空间健康检查。（#7980）

## [0.7.0] - 2026-11-12

### 移除

- 为聚焦系统运维，将八个通用技能移出默认技能包，并结束对内置副本的常规维护。
  仍需这些能力的用户可使用上游技能、工具或官方文档：
  - `github` — 通用 GitHub CLI 工作流，参见 GitHub CLI 技能
  - `humanizer` — 写作指导，按需加载上游 Humanizer 技能
  - `image-gen` — 通用图像生成，百炼接入交由使用它的产品维护
  - `pdf-reader` — 通用 PDF 读取，由宿主工具 / PyMuPDF 适配提供
  - `xlsx` — 通用表格，已由 MiniMax XLSX 技能覆盖
  - `regex-mastery` — 通用正则指南，按需加载上游指南
  - `shell-scripting` — 通用 Shell 脚本，由上游项目提供
  - `upgrade-alinux-kernel` — ALinux 专用内核升级，交由发行版维护方
- 移除 `pdf-reader` 与 `xlsx` 的配套回归测试，以及不再使用的 PyMuPDF 测试依赖。
- 从 `alinux-admin` 的依赖列表移除已退场的 `shell-scripting`；其自带的 Shell
  参考文档保持不变。

## [0.6.6] - 2026-11-12

### 变更

- 源码安装会把自身安装的 Skill 目录记录到安装清单中，`make uninstall` 只删除清单
  记录的目录，手动添加或由其他安装器放入的同名 Skill 不再被静默删除；
  `make install` 也不再覆盖未记录的同名目录，除非传入 `REPLACE_UNOWNED=1`。
  本次发布之前安装的集合没有安装清单，首次升级需执行
  `REPLACE_UNOWNED=1 make install`，否则安装与卸载都会报错停止 (#9886)
- 当安装路径仍由 `HOME` 推导时，`make install INSTALL_PROFILE=user` 在 `HOME`
  未设置、为空、为 `/` 或为相对路径时拒绝执行，不再静默写入
  `/.copilot-shell/skills`；显式传入 `PREFIX`、`DATADIR` 和 `SKILLS_DIR` 时仍可在
  没有家目录的环境中安装 (#9886)

### 修复

- `install-openclaw` Skill 以 `0600` 权限写入 `openclaw.json`（其中以明文保存 API
  Key）、其备份、`identity/device-auth.json` 与 `devices/paired.json`，本地其他用户
  不再能读取 operator token，重复执行也不会把加固过的配置权限降回 `0644` (#7846)
- `aliyun-ecs` Skill 中的 `DeleteKeyPairs --KeyPairNames` 示例改为 API 要求的数组
  形式，示例命令不再被参数校验拒绝 (#9547)
- `kernel-dev` Skill 的 `check-env.sh` 不再把 `kernel-build-dir` 伪包混入
  `yum install` 提示，避免 dnf 环境下整条安装事务失败；缺失的软件包与缺失的内核
  构建目录改为分行提示 (#7922)

## [0.6.5] - 2026-11-09

### 变更

- 从源码安装时改为按有效用户 ID 选择安装位置：root 安装到
  `/usr/local/share/anolisa/skills`，其他用户安装到原有的
  `~/.copilot-shell/skills` 目录，使 `build-all` 与升级继续管理同一份技能集合；
  `make uninstall` 使用相同的布局，`make install AWPRJ=` 选择 cosh 原生的系统资源目录 (#9460)
- `install-tokenless` Skill 说明 0.9.2 之后的 tokenless 已将 RTK 链接进二进制：
  npm 包不再单独安装 `rtk`，RTK 以 `tokenless rtk` 运行，源码构建还需要 `git` 与 `patch` (#9461)

### 修复

- `setup-mcp` Skill 中的 `validate_mcp` 会拒绝同时设置多个传输字段的服务，
  也不再接受根节点不是 `mcpServers` 对象的配置 (#4588)
- `xlsx` Skill 可识别包含转义单引号的工作表名，引用工作表中的公式会校验到正确的表，
  不再被误判为错误 (#4210)
- `install-openclaw` Skill 不再将已废弃的 `ownerDisplay` 字段写入生成的 OpenClaw 配置 (#8798)

## [0.6.4] - 2026-11-03

### 修复

- 修正本地兜底文档中的 ws-ckpt skill 和 adapter 脚本路径。

## [0.6.3] - 2026-08-21

### 修复

- RPM 现在提供 `anolisa-component(os-skills)`，当仓库侧组件索引不可用时，
  `anolisa upgrade` 可通过 RPM metadata 为已有 OS Skills 组件解析软件包
  ([#2576](https://github.com/alibaba/anolisa/pull/2576))。

## [0.6.2] - 2026-08-07

- 新增 `ktuner` Skill，支持确定性的内核诊断、调优和回滚。（#1278）
- 从源码和 RPM 安装内容中移除旧版 OpenClaw 与 Hermes adapter 脚本。（#1172）
- 更新 `anolisa-guide`，补充带认证的 Skill Ledger 恢复与篡改检测说明。（#2185）

## [0.6.1] - 2026-07-03

- 重写 `sysom-diagnosis` Skill，并移除旧版 CLI。（#1241）
- 修复 `install-openclaw` Skill 对 OpenClaw gateway 写入范围的验证。（#1205）

## [0.6.0] - 2026-06-29

- 新增 anolisa 组件契约，包括 component.toml、Makefile 和 RPM spec。（#1159）
- 为 `install-openclaw` Skill 新增 OpenClaw bootstrap 指引。（#1051）
- 在启动 gateway 前为 `install-openclaw` Skill 新增模型 endpoint 预检。（#1031）
- 为 `anolisa-guide` Skill 新增静态知识库更新脚本。（#1010）
- 新增 `anolisa-guide` Skill。（#849）
- 修复 uv 和 qwenpaw 安装过程中的阿里云镜像 fallback。（#968）
- 将 `install-claude-code` Skill 的 DashScope proxy URL 更新为新的 Anthropic
  endpoint。（#858）
- 在 OS Skills 中将 `copaw` 重命名为 `qwenpaw`。（#968）

## [0.5.0] - 2026-06-11

- 新增 `anolisa-register` Skill。（#829）

## [0.4.0] - 2026-06-08

- 新增 Agent 安装 Skill 自动安装 Tokenless plugin 的能力。（#731）
- 新增 OpenClaw 依赖项预检。（#719）
- 改进 OpenClaw 非交互式配置流程。（#687）
- 新增 Hermes adapter runner。（#617）
- 新增独立的 ANOLISA adapter 入口。（#549）
- 修复 OpenClaw state dir 路径规范化。（#641）
- 改进 Makefile 安装路径与组件契约。（#541）

## [0.3.0] - 2026-04-26

- 新增 `hermes-agent-install` Skill。（#353）
- 新增 `clawhub-skill-mng` Skill，支持 npm 安装和 YAML 描述匹配。（#315）
- 修复 AgentSight 自定义数据库路径问题，改用默认路径。（#366）
- 修复 AgentSight Token 节省量查询支持。（#355）
- 修复 AgentSight 中断 CLI，并统一使用 `conversation_id` 命名。（#334）

## [0.2.2] - 2026-04-15

- 支持通过 `agentsight` Skill 启用 AgentSight dashboard。（#222）

## [0.2.1] - 2026-04-14

- 使用 MiniMax 开源实现升级 `xlsx` Skill。（#218）
- 将 Skill 描述中的“适用于 alinux4”更新为“适用于 RPM-based Linux”。（#182）

## [0.2] - 2026-04-12

- 新增 `humanizer`、`image-gen`、`pdf-reader` 和 `xlsx` Skill。（#178）
- 新增 `cosh-guide` Skill。（#23）
- 为 `sysom-diagnosis` Skill 新增网络、I/O 和系统负载诊断能力。（#163）
