# AW 架构

[English](../../en/aw/architecture.md)

AW 在原生 Agent 的工具调用前后执行策略。Adapter 接收原生回调，本地服务管理
绑定和事件，Host 调用 Provider，最后由 Adapter 将结果交还 Agent。
模型调用、工具调度和权限审批仍由 Agent 负责。

## 使用流程

![AW 用户流程与组件架构](../../../images/aw/architecture_zh.png)

图中按用户操作组织各包，箭头表示运行时协作；绿色区域是 Provider 协作目录。
底部说明目标分发职责：组件 owner 交付组件，OS 发行方验证并组合产物。
当前接入方式与后续发布范围见 [Provider 指南](providers.md)。

## Crate 关系

AW 框架包含 `crates/` 下的 10 个 crate。具体 Provider 在 `providers/` 中单独维护。

| Crate | 职责 | 直接依赖的 AW crate |
| --- | --- | --- |
| `aw-cli` | 统一 `aw` 入口、启动与命令编排 | adapter、config、exec、host、package、provider、service |
| `aw-service` | 本地服务、客户端与运行状态 | adapter、config、contracts、core、exec、host、provider |
| `aw-adapter` | 原生注册、回调转换、阶段校验与效果采用 | config、contracts、exec、provider |
| `aw-host` | 准备 Provider，执行已准入的 Hook 步骤 | config、exec、provider |
| `aw-provider` | 校验 Provider 消息和能力准入 | config |
| `aw-exec` | 管理进程期限、输出上限、安装文件读锁和退出清理 | 无 |
| `aw-config` | 解析配置，校验字段和静态引用 | 无 |
| `aw-core` | 能力计划执行器、Host/Journal 接口和 FileJournal | contracts |
| `aw-contracts` | Schema、规范编码和记录间一致性校验 | 无 |
| `aw-package` | 本地包数据库、计划、持久事务和配置 API，以及 `aw-build` | config、exec |

依赖列省略 `aw-` 前缀。`aw-package` 的测试还依赖 `aw-service`，CLI 集成测试还使用
`aw-contracts`。底层库不依赖 `aw-cli`；`aw` 在进程内调用安装 API。
独立 `aw-package` 可执行文件已删除，Rust crate 和 `aw-build` 保留。

公开 `aw` 命令树使用一份 typed Clap 声明生成解析与帮助；准入后的选项直接
调用现有处理逻辑，不重新解析 argv。内部 Hook 与生命周期回调保留协议解析器
和失败回复。软件安装、配置和卸载全部使用同一棵 `aw` 命令树；
移除另一个可执行文件不改变服务消息、Provider 效果或已有文件归属记录。

### 本地软件包操作

`aw-cli` 调用 `aw-package::plan` 检查本地 bundle 或所选卸载内容。计划保存包与版本
决定、精确文件变更和阻碍；生成计划不创建安装目录、锁或数据库。`plan::execute`
按数据库、安装目录的顺序重新获取写锁，复查记录并调用同一事务实现。
持久替换 `installed.json` 表示提交。中断的操作需要显式执行 `aw recover`，完成
已提交操作的清理，或恢复之前的文件，不覆盖发生冲突的字节。

新软件包信息保存在 `$XDG_STATE_HOME/aw/packages`（有 HOME 回退路径），或显式的
root 管理 raw scope `/var/lib/aw/packages`。安装目录中唯一的
`.aw-packages/aw-core.json` 标明数据库位置，查询和生成配置按此读取，无需猜测另一个
用户的 XDG 目录。首次成功写入软件包时存档旧组件 JSON，再替换成旧安装器会拒绝的
位置文件。移除最后一个包后保留空登记和位置文件，便于安全重装。RPM/DEB 后端和
跨安装目录选择依赖不属于本次实现。

`aw-exec::PackageLeases` 为已知可执行文件和参数中的字面绝对路径持有已有的安装目录
读锁。CLI 保留自身 core 与 Agent 的锁，服务保留自身可执行文件的锁，Host 在其生命
周期及所属进程清理期间持有 Provider 的锁。这个共用库不依赖 `aw-package`，不读取
集中清单，也不创建记录。它不能发现任意外部进程或 shell 动态生成的路径。包含无法
核验读锁支持的旧 AW 可执行文件的目录仍拒绝卸载。见
[本地包管理](../../../../src/aw/docs/design/zh/packaging/package-management.md)。

### 独立 Provider

| Cargo 包与二进制 | 目录 | 职责 | 依赖的框架 crate |
| --- | --- | --- | --- |
| `aw-provider-sec-core` | `providers/agent-sec-core/` | 桥接 sec-core CLI，提供安全检查 | exec、provider |
| `aw-provider-tokenless` | `providers/tokenless/` | 桥接 Tokenless CLI，提供输出压缩 | exec、provider |

这两个实现在仓库根目录下各自拥有 Cargo workspace 和锁文件，通过独立进程
接入，不计入 10 个框架 crate。Tokenless Provider 的测试还依赖 sec-core Provider。
其他团队可独立开发、分发 Provider，见[协作指南](providers.md)。

## 一次 Hook 请求

1. `aw run` 调用 `aw-adapter` 检查原生入口并准备注册，再由启动器启动 Agent。
2. Agent 触发回调，Adapter 把原生事件转成服务请求。
3. `aw-service` 管理绑定、事件期限和审计，交给 `aw-host` 执行步骤。
4. Host 用 `aw-provider` 校验协议，通过 `aw-exec` 启动配置中的 Provider 命令。
   内置桥接程序再调用 Tokenless 或 sec-core CLI；双方通过进程协议连接。
5. 调用结果沿原路返回，Adapter 按原生接口采用阻断或结果替换。Provider 返回成功
   只表示调用成功，是否实际采用效果须由 Adapter 确认。

原生 Hook 脚本也通过 `aw-exec` 执行，保留原始输出和退出码，不经过 Provider 握手。
AgentSight 是独立的可选观测来源；`aw inspect` 的只读视图不参与 Hook 准入。

### 显式策略更新

`aw run --reloadable` 显式预装稳定 Adapter 入口，`aw update` 定位现有实例，
由 `aw-service` 在请求准入门外准备新的不可变 Host，再在同一门内最终复查空闲和版本后发布。
服务保留启动身份，每个实例持有自己的激活策略。原生 Adapter 报告请求/工具生命周期，
保留调度与效果采纳；Provider 协议和 Core 计划执行不变。
固定版本、busy 拒绝、进程关联及验收范围见[策略热更新](../../../../src/aw/docs/design/zh/runtime/policy-reload.md)。

### `aw-core` 在哪里

当前 Hook 路径是 `aw-service → aw-host → aw-exec`。服务复用
`aw-core::journal::FileJournal` 保存审计，但不调用 `Core::prepare/execute`。

`aw-core::Core` 是单独的能力计划执行接口。使用它的嵌入方要提供
`ProviderHost`、`Clock` 和 `Journal`；它依赖 `aw-contracts` 校验计划和执行记录。
`aw-host::Host` 与这个 `ProviderHost` 接口各有自己的契约，不能当成同一个调用层。

## 目录与代码入口

`src/aw/Cargo.toml` 只定义 workspace。10 个通用框架 crate 位于 `crates/`，
2 个具体 Provider 位于仓库根 `providers/`，各自拥有 workspace、源码和测试。原来根目录 `src/`
中的 contracts 代码已迁入 `crates/aw-contracts/`。

`providers/` 是协作入口，允许收录第一方与社区实现；Provider 也可以独立发布，
通过进程协议接入。[Provider 开发与分发](providers.md)说明目录约定和打包范围。

| 路径，相对于 `src/aw/` | 内容 |
| --- | --- |
| `crates/aw-cli/src/bin/`、`crates/aw-cli/src/lib.rs`、`crates/aw-cli/src/cli/` | CLI 入口、通用启动与 RPC 协调 |
| `crates/aw-cli/src/command.rs`、`crates/aw-cli/src/command/` | 公开命令的 typed 声明、生成的帮助与处理逻辑分发 |
| `crates/aw-service/src/server.rs`、`crates/aw-service/src/runtime/` | 本地服务与事件执行 |
| `crates/aw-host/src/lib.rs`、`crates/aw-host/src/event.rs` | Provider 准备与调用 |
| `crates/aw-core/src/journal.rs` | 持久审计实现 |
| `crates/aw-package/src/{database,open_bundle,plan,transaction}.rs` | 包记录、本地包检查、预览和持久文件变更 |
| `crates/aw-exec/src/packages.rs` | 运行时对已知安装文件持有读锁 |
| `crates/aw-contracts/src/registry.rs` | 编译进程序的契约 Schema 清单 |
| `../../providers/agent-sec-core/`、`../../providers/tokenless/` | 具体 Provider 实现、配置示例和测试 |
| `crates/aw-adapter/src/` | 启动器与服务共用的原生方言、能力目录 |
| `crates/aw-adapter/native/` | 随包分发的 Python/JavaScript 原生桥接资源 |
| `scripts/`、`tests/` | workspace 检查、跨语言向量检查和原生验收 |
| `docs/design/` | 协议约定、边界与设计理由 |

详细设计从[设计索引](../../../../src/aw/docs/design/zh/README.md)按调用顺序阅读。
[架构边界](../../../../src/aw/docs/design/zh/architecture-boundaries.md)说明 Adapter 接口和后续发布工作。
开发环境和检查命令见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。

## Schema 归属

| 契约 | 归属 |
| --- | --- |
| 活跃能力、计划与证据 Schema | `aw-contracts/schemas/` |
| 配置 | `aw-config/schemas/` |
| Provider 消息与 View 协议 | `aw-provider/schemas/` |
| Provider 私有配置与后端数据 | 对应的具体 Provider 目录 |

`aw-contracts/schemas/` 只保存已注册、受测试覆盖的合同，目前共 23 份。
计划、证据等 v1 合同仍在使用；四类能力的输入、输出使用 v2。
此前八份未注册的能力 v1 草案没有仓库内调用方，已删除，历史内容可从 Git 查阅。

新增 Provider 可在现有事件和效果范围内声明操作，私有配置和后端数据由自身管理。
新增 Core 能力或合同版本时，须同时增加 Schema、Registry 注册项、跨记录校验和
有效及无效样例。受支持的版本可以并存；新增版本使用独立的名称和 `$id`，保留旧版
合同及其测试，直到明确结束兼容。迁移现有 Schema 时保持字节、`$id`、Registry 名称
与跨语言摘要向量不变。
