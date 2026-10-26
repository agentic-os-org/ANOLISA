# 参与 AW 开发

[English](CONTRIBUTING.md)

本文说明 AW 的开发检查。仓库通用贡献流程和提交规则见[仓库贡献指南](../../CONTRIBUTING_zh.md)。

## 运行检查

通过 rustup 准备 Rust，并安装 Python 3 和 Node.js。Rust、rustfmt 和 Clippy
由 [rust-toolchain.toml](rust-toolchain.toml) 固定。在仓库根目录运行：

```bash
python3 src/aw/scripts/check.py
```

入口依次运行 CI 行为测试、格式检查、Clippy、完整的 locked workspace 测试、
Python/JavaScript 摘要向量和 rustdoc。缺少工具，配置、Provider 协议/准入、Provider Host、
合同、计划、Core 执行、命令执行、本地服务或 Journal 测试目标为空或全部
ignored、向量错误及命令失败均返回非零。每条命令都有超时限制，失败或中断时
回收其子进程组。日志标明失败命令，可在 `src/aw` 单独运行对应命令定位问题。

数据库、安装清单、事务、运行锁和包管理 CLI 测试还分别以子进程 umask
`0022`、`0002` 运行，覆盖包规划、配置发布及 Host/服务租约的持有期。
确定性子进程屏障覆盖无关子进程处于 fork 与 exec 之间时的锁释放。
测试样本及复制的可执行文件应显式设置权限，
不要修改并行测试进程的全局 umask。

这些检查可由普通用户运行，无需启动 Agent 或登录服务。Cargo 会下载尚未缓存的
依赖，Schema 校验只读取随包资源。检查入口、本地服务、Provider Host 执行、命令执行及 FileJournal 要求 Linux；
本门禁不认证其他操作系统或最低支持版本。

PR 修改 AW、Provider、AW workflow 或声明的 Tokenless 消费合同路径时，[AW CI](../../.github/workflows/aw-ci.yml)
会对候选合并提交运行上述检查。提交 AW 改动前，请先在 Linux 上运行上面的命令。
CI 失败时，从任务日志中找到失败的命令，再到本地 `src/aw` 目录单独运行，定位问题。

## 回归矩阵与文件大小

[tests/regression-matrix.json](tests/regression-matrix.json) 把已注册 Agent、Hook
事件/效果和生命周期关联到具体可运行的 Rust 测试。编译后的 Adapter catalog
必须与矩阵、配置 schema 一致。共享 PreTool/PostTool 用例遍历矩阵中的所有
Agent，并使用明确的原生输入；新增 Agent 缺少 fixture、Hook 单元格漏填、
引用的测试被删除或标为 ignored，都会使现有 `AW / required` 门禁失败。

安全回归覆盖拒绝、扫描错误、取消和离线观察；Tokenless 覆盖新调用的重试归属、
禁止原地改参、最终文本替换、不支持/失败/无收益结果、托管 RTK 隔离和可选视图
隔离。这些 Provider fixture 与回调测试在每次相关推送执行，无需凭证。
真实 SDK/CLI 采纳、真实 Tokenless/RTK 包验收仍单独显式执行；不能把 ignored
的原生验收测试填入必需覆盖。

门禁通过 Cargo JSON 构建产物一次编译工作区测试，直接列举测试可执行文件，
再复用构建运行完整工作区回归。集成测试目标须包含可运行测试，新增目标
也会自动检查。显式声明的 `opt_in_targets` 将已安装包验收保留在独立流程；这些
目标必须存在且仅包含 ignored 测试。范围判断、候选 SHA 校验和必需状态的失败阻断保持原有行为；
不会根据修改了哪个 Adapter 而跳过共享安全或 Tokenless 回归。

Rust、Python 和 JavaScript（`.js`、`.mjs`、`.cjs`）源码、脚本和 fixture
统一限制为 699 行，达到 600 行提示。仅已有 `crates/aw-contracts/src/validation.rs` 保留固定的
711 行上限。按职责拆分并保留入口与断言，不给测试 fixture 豁免。
依赖/布局检查位于 [check_layout.py](scripts/check_layout.py)，编译测试清单位于
[check_inventory.py](scripts/check_inventory.py)，矩阵与 README 一致性检查位于
[check_matrix.py](scripts/check_matrix.py)。

更新支持能力后，在 `src/aw` 重新生成两份 README 的表格并运行门禁：

```bash
python3 scripts/check_matrix.py --write-readmes
python3 scripts/check.py
```

只开发门禁时，可用 Python 3.11+ 运行以下测试，无需 Rust 或 Node：

```bash
python3 -B -m unittest discover -s tests -p 'test_ci_*.py'
```

## 日常 CI 与 nightly 集成

AW、Provider 和 AW workflow 变化运行完整门禁。日常门禁保留全部无需凭证的安全、
Tokenless、生命周期、Agent/Hook 矩阵契约，以及原生验收清单和文件大小检查。
其他组件的内部改动由各自 CI 负责。Tokenless CLI/protocol/runtime/statistics 源码、
其 AW Provider 与真实夹具、RTK setup/patch、依赖和工具链、共享 Provider 协议/执行器
源码及包构建输入会选中真实兼容测试，
确切路径在 `scripts/check.py`。它复用现有包构建器，显式运行四个具名的真实 CLI
Provider 测试，包括带归因的 RTK 重试和 PostTool 合同。测试缺失/被忽略、失败或
验证 SHA 不一致均使必需结果失败。其他后端公开合同仍需明确兼容验收入口。

范围判断后，AW 与 Tokenless 两个独立任务并行验证同一候选 SHA，各自使用 runner、
构建目录和缓存 key；Tokenless 任务无需安装 Node.js。现有 `AW / required` 始终汇总
两个任务的整体结果和验证 SHA。失败、取消、意外跳过和证据缺失均阻断，包括测试已
发布输出后的失败；只有明确未选中该范围时才允许跳过。AW 检查保持 35 分钟期限，
真实后端构建和合同另有 60 分钟期限。并行实际耗时取决于可用 runner 和缓存。
普通 Agent、service、CLI、安装/事务、文档，以及共享 crate 的测试/示例改动只运行
AW 快速夹具门禁，无需构建真实 Tokenless/RTK。混合改动中任何消费输入变化仍会触发
真实合同；手动运行和强制推送选中两组检查。Tokenless 自身组件、Adapter 和预构建
CI 的触发规则与完整覆盖独立保留。

在干净、已提交的 checkout 中执行：

```bash
python3 -B src/aw/scripts/check_tokenless.py --output /absolute/new/evidence-directory
```

输出目录须尚未存在，保留包清单、二进制摘要、测试日志和清理记录；无需托管模型凭据。
评审后删除该精确证据目录；常规组件构建缓存保留供复用。

[AW Integration](../../.github/workflows/aw-preview.yml) 每晚北京时间 03:23 运行，
也接受完整 40 位 `candidate_sha`，用于发布前手动验收。它一次构建全部四种
Preview 包，验证真实 sec-core health、Tokenless/RTK 契约、安装、服务生命周期、
可回滚的并存目录和卸载。并存验收使用同一候选构建，不认证跨版本数据迁移。
原生任务复用该次构建的二进制，并校验提交 SHA 和文件摘要。

无 Python 的包验收阶段使用 `scripts/check_packages.sh`，将测试并发限制为两个，
保留真实 AW payload 和 CLI 单条命令的 30 秒期限。嵌套 runner 容器可能看到超过
实际配额的 CPU 数量。每组 Cargo 测试另有 600 秒总期限；失败后停止后续组。
始终上传的 `test-evidence/` 记录命令、耗时、退出码，以及执行前后的 cgroup
CPU、内存和 I/O 数据。CLI 夹具失败时标明具体命令和工作目录，不输出环境变量。
该阶段结束后才安装 Python，用于比较包清单。安装目录和数据库统一放在私有 `/tmp`
目录，退出时清理，并将 demo 证据保留到产物目录。

[tests/native-matrix.json](tests/native-matrix.json) 为每个已注册 Agent 记录固定
版本、责任人、有界命令和明确的验收边界，并生成 nightly 任务矩阵。八种 Agent
均有自动验收。OpenClaw 覆盖官方 Hook runner，完整 Gateway 模型流程仍需单独验收。

Qoder CLI 1.1.64 和 QwenPaw 2.2.2b4 / AgentScope 2.0.9 各自必须完成四项真实
sec-core 用例：`allow`、`scan-deny`、`backend-error` 和 `timeout-block`。任务复用
候选提交的 `aw`、Provider、`agent-sec-cli` 和 `agent-sec-daemon`，并核对校验和。
每个用例在独立 Ubuntu 24.04 容器中启动真实 daemon，清单通过多平台 manifest digest
固定镜像。容器禁用网络，宿主仅挂载一个可写的 socket 目录，并记录实际平台镜像 ID。定时任务只选择一次 `github.sha`；
容器不会另行拉取不断变化的分支或独立构建的 nightly 镜像。

另一个必需的 Qoder 1.1.73 TUI 用例验证原生 SessionStart 就绪，以及请求派发前的
空策略 → 增加 Hook → 移除 Hook 更新，保留相同 PID/启动 ticks。它下载独立校验和固定
的版本，并保留 1.1.64 print 用例。原生清单要求其具名元数据报告及清理结果。
该用例不证明托管 TUI 工具轮次或结果替换已经通过。
TUI 夹具仅从子进程环境移除 `CI` 和 `GITHUB_ACTIONS`，并通过原生
`permissions.trustDirectories` 仅信任本次临时工作目录。Runner 与 print
验收通道保留各自的环境和权限检查。

正常用例必须写出标记文件；风险用例必须命中内置 `shell-recursive-delete` 规则
（`warn` 转为阻断），保留待删除的临时夹具，且不生成标记。停止真实容器模拟后端
不可用，暂停容器验证 Provider 的 2 秒截止时间和 `on_error: block`。二者均须
阻止标记生成，恢复后端后也不能补执行。检查同时核对原生回调、sec-core 事件、
AW 审计和零存活绑定。QwenPaw 通过原生审批 API 仅批准那条确切的夹具命令，再由
AW 拒绝执行。每项用例停止所属进程和容器，检查 socket、插件和端口清理；profile
及临时状态在退出时删除，不上传。
若夹具超过清理宽限期，外层 Linux subreaper 会终止并回收跨 session 的后代进程，
并按本次命令的唯一归属标签删除容器，保留清理报告；被中断的运行仍判定为失败。

QwenPaw 的真实 App/API 使用本地确定性 OpenAI 兼容模型，无需云端密钥。Qoder
调用真实的 Performance 托管模型，使用仓库的 `QODER_PERSONAL_ACCESS_TOKEN`
Secret，认证方式见 [Qoder 官方说明](https://docs.qoder.com/cli/authentication)。
依赖安装不会接收 Secret。Qoder 启动器及其原生回调在执行时继承它；后端探测、
Docker 命令和版本检查会从环境中明确移除它。凭据缺失或过期会使验收失败，不会跳过；fork 运行该 nightly 分支时需自行配置 Secret。Qoder 下载使用
固定官方 URL、归档校验和及运行时版本检查，升级须明确修改清单。PR 合同门禁
保持无凭据运行。

每个原生命令都必须声明报告契约：`unittest`/`tap` 要求确切的测试数量；`json`
要求结果文件及确切的用例名称或实际完成数量。执行端与最终汇总都会校验这些证据；
仅退出码为零或外层标记 `passed` 不能算作套件通过。报告计数须来自实际完成的检查，
不能写死为常量；覆盖范围变化时须同步更新清单。
缺少套件、SHA 过期或报告不完整都会使集成汇总失败。证据 artifact 保存候选 SHA、
组件包清单、已安装 SDK 版本/npm lock、命令/PID/截止时间和逐套件结果。
@kongche-jbw 负责 AW 集成失败，后端契约失败转对应组件 CODEOWNERS。发布前须
对确切发布提交重新运行，并验收发布所需的其他原生边界。

日常 Rust 缓存由 PR 读取，仅 main 推送写入；重型构建缓存由 nightly 写入。
原生 Python/npm 依赖分别缓存，原生任务最多三个并行。完整模式须从 `main` 触发，
候选提交须属于拉取到的 `origin/main` 历史，才能执行候选脚本或接收 Qoder Secret。
fork 的手动运行可选择 `credential_free=true`，验收该 workflow 的确切 SHA。
该模式执行包验收、六种无凭据 Agent 和常驻容器；汇总明确列出待完整主线 nightly
验收的 Qoder CLI/SDK 托管模型任务。调用方与原生任务均在该模式下禁止传递 Qoder Secret。

```bash
gh workflow run aw-ci.yml --ref main -f integration=true
gh workflow run aw-preview.yml --ref main -f candidate_sha=<40-character-SHA>
```

### 最小常驻测试容器

新增必需的容器任务复用原生清单中全部 `tests/sdk_resident.py` 命令及其确切 JSON
报告：当前为 24 个（Claude 16、Pi 8），来自全部 49 个原生套件，无凭据档为 42 个。
覆盖 1,280 次长会话、状态回收、资源增长断言、回调排空、session-store 和原生拒绝，
保留既有生命周期/MCP 场景。model-relay 和 dispatch 合同入口继续由宿主原生任务验证。

镜像只包含明确列出的公开夹具、固定 SDK 依赖和四个候选二进制：`aw`、
`aw-provider-tokenless`、`tokenless`、`rtk`。混合 Bash/MCP 长会话的 PreTool 需要 RTK。
Ubuntu、Node 和 uv 镜像通过 manifest digest
固定；Python/SDK 版本来自原生清单，Pi 使用已提交的 npm lock。构建上下文排除完整
仓库、部署文件和用户配置。运行使用普通测试用户、合成数据和 loopback 模型夹具，
关闭外网、禁止宿主挂载，且不传递凭据。一次构建的镜像归档通过保留一天的 Actions
artifact 供四个 Agent/常规或长会话分片使用；加载后校验归档 SHA-256、image ID 和源码版本。
镜像不推送到 registry。
保留产物限于声明的结果 JSON、运行时版本、输入摘要及命令/清理记录；私有 profile 和任务数据
随容器删除。汇总重新核对逐项报告、候选 SHA、运行时版本及容器/镜像清理。

显式运行 fork 候选模式：

```bash
gh workflow run aw-ci.yml --repo <owner>/anolisa --ref <branch> \
  -f integration=true -f credential_free=true
```

在 `src/aw` 中使用匹配的 nightly bundle，仅运行本地容器验收：

```bash
python3 -B scripts/resident_container.py --candidate-sha "$(git rev-parse HEAD)" \
  --bin target/native-binaries --output target/resident-evidence
```

输出目录须尚未存在，镜像输入须已提交。本地命令构建一次再顺序执行四个分片；
CI 构建一次，最多两个分片并行。分片期限为用例期限之和加清理余量，job 再预留
15 分钟用于加载镜像、证据上传和清理。宿主原生 job 同样按清单加安装余量计算预算。
汇总要求全部分片与同一已清理的镜像构建身份。执行器在成功或失败后删除所属构建上下文、
容器及唯一镜像标签。构建或载入镜像时收到中断，会先等待有界 Docker 操作结束再删除标签。
镜像操作超时会记录清理不完整，因为 daemon 仍可能稍后创建标签；按记录命令核对并删除。
保留 `target/resident-evidence` 供检查，评审后运行
`rm -rf -- target/resident-evidence` 清理。Docker 不可用时，可按容器记录中的确切
停止/删除命令完成清理。

常驻 SDK 与真实 sec-core 组合、SDK 饱和/恢复、请求接流/排空和监督器重启继续独立验收。
常驻套件保留 fail-close、原生权限与仅元数据证据；生产 endpoint、流量和部署退出链路
由对应部署验收覆盖。

本地清单检查与原生执行使用和 CI 相同的入口：

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

仅运行固定 OpenClaw 的 loader/runner 套件时，从 `src/aw` 复用清单中的隔离 npm
安装方式：

```bash
npm install --prefix target/openclaw-native --ignore-scripts --no-audit --no-fund openclaw@2026.9.6
OPENCLAW_PACKAGE_DIR="$PWD/target/openclaw-native/node_modules/openclaw" \
  node --test --test-reporter=tap crates/aw-cli/tests/openclaw-native.mjs
```

八项 TAP 测试必须全部通过且不得跳过。套件通过 OpenClaw 官方 loader 加载实际 AW
入口与 manifest，再执行注册的 Hook 和中间件；策略别名来自固定版本的运行时。
SDK 依赖缺失或 loader 失败都会使套件失败。该定向命令不能替代原生证据汇总，
也不代表 Gateway 或模型轮次验收通过。

二进制目录须包含 nightly 的 bundle 和校验文件。先安装清单指定的 Python 版本、
Node 24.16.0 和 uv 0.11.13。Qoder 和 QwenPaw 还需要可用的本地 Docker daemon，
Qoder 需在环境中配置 PAT。在上述统一原生入口使用 `--agent qoder` 或
`--agent qwenpaw` 选择对应套件。原生执行使用一个所属短临时目录容纳 Linux Unix
socket，并在退出时删除；保留证据放在 `target/`。原生任务使用独立 Python 和
Hermes 私有工具/环境，避免为 runner 的 loader 覆盖或共享可写缓存放宽原生
准入规则。不能通过重试或省略框架将
全量失败改报为成功。

## Crate 边界

本地软件包变更从 `aw-package::plan` 进入，先生成只读计划，再由 `plan::execute`
调用同一持久事务实现。查询和预览不能创建记录或锁。在 Linux 上检查这些路径：

```bash
cargo test --locked -p aw-package --test management --test database
cargo test --locked -p aw-package transaction::tests
cargo test --locked -p aw-exec --test packages
```

包测试覆盖独立工具、同目录精确依赖、旧记录存档、冲突和恢复。CLI、服务与 Host
共用 `aw-exec` 的包读锁，不让运行时依赖包数据库。修改包和发布配置时都保持先锁
数据库、再锁安装目录的顺序。system scope 是显式 raw 管理，不是原生包后端。
提交、恢复规则及运行引用发现范围见[本地包管理](docs/design/zh/packaging/package-management.md)。
这些局部命令不能代替 AW 完整检查或原生验收。

通用框架位于 `crates/<name>/`，具体 Provider 位于仓库根 `providers/<provider>/`，各自构建；
根清单只组织 workspace。[Provider 协作约定](../../providers/README_zh.md)说明如何收录和分发。
包的职责和依赖见[架构总览](docs/design/zh/architecture.md)，
协议阅读顺序见[设计索引](docs/design/zh/README.md)。

原生方言和桥接资产归 `aw-adapter`；`aw-cli` 负责参数、RPC、信号和资源，服务负责
状态、预算和审计。两者共用 Adapter，不向它传递服务内部类型。
`aw-provider` 负责离线协议校验，进程执行统一使用 `aw-exec`。
修改 crate 边界时，同时更新 [check_layout.py](scripts/check_layout.py)
和[回归检查](tests/test_ci_checks.py)，并同步[架构表格](docs/design/zh/architecture.md)。

## 构建服务

在 Linux 的 `src/aw` 中构建 CLI 和样例 Provider：

```bash
cargo build --locked -p aw-cli --bin aw
cargo build --locked -p aw-provider --example policy
```

[使用指南](../../docs/developer-guide/zh/aw/native-agents.md#运行本地服务演示)提供前台
服务与独立客户端的完整演示，以及状态目录和审计历史的清理说明。服务使用
`aw-service/v1alpha7` 本地协议，Provider 继续使用 `aw-provider/v1alpha1`；二者
分别版本化。

## 运行时验收

本地服务定向检查：

```bash
cargo test --locked -p aw-service -p aw-cli
```

服务单元测试和 CLI 集成测试使用真实 Unix socket、样例 Provider 子进程和私有临时状态目录，不需要
模型、Agent 安装或云端密钥。检查共享事件预算、调用关联与单次执行、取消和关闭、
持久审计、陈旧句柄及端点所有权。fixture 负责等待所属进程并删除临时目录；构建
产物保留在 `target/`。验收边界见[本地服务合同](docs/design/zh/runtime/local-service.md)。

在 Linux 的 `src/aw` 目录中运行 Provider Host 定向检查：

```bash
cargo test --locked -p aw-host
cargo test --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
```

这些测试使用本地 fixture 进程检查准备、请求绑定、失败报告、共享截止时间、取消和
事件步骤的单次调用。[本地示例](docs/design/zh/runtime/provider-host.md#本地示例)使用合成
Adapter 证据和工具事件运行样例 Provider，不代表原生 Agent 验收或效果已被采用。

协议与 Core 测试使用合成输入和 Host。原生接入需要单独取证，确认回调已安装、
工具按预期执行或被阻断，以及返回效果已被 Agent 采用。验证命令和结果写入 PR，
实验日志不放入 README。

Qoder SDK 1.0.56 原生验收使用随包的 Worker 1.1.78 和托管 `auto` 模型，
复用 `QODER_PERSONAL_ACCESS_TOKEN` Secret。依赖安装不接收凭据，SDK 运行阶段
验证首请求拒绝、session Hook 隔离、成功/失败观察、原生权限拒绝和 daemon 不重启。

Qoder SDK 原生清单分别运行普通 `aw run` 和 `--reloadable` 两条路径。后者增加已有
Query 的策略新增/移除、另一 session 活跃时 busy，以及保留原生回调直到清理结束的真实
`aw update` RPC 检查。两条路径都由调用方管理 `startDaemon()`，仅运行阶段注入 PAT，
并断言 daemon 身份不变、清理后 bindings/events 为零。
