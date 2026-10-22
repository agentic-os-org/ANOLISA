# AW 架构边界

[English](../en/architecture-boundaries.md)

AW 内部接入库随 AW 编译，具体 Provider 通过进程协议接入。
本次迁移统一源码归属和回归入口，保留现有命令、协议与 Preview 安装行为。

## 目录与责任

```text
ANOLISA/
├── src/aw/                         # 10 个框架 crate 的 virtual workspace
│   ├── Cargo.toml / Cargo.lock
│   ├── crates/
│   │   ├── aw-adapter/
│   │   │   ├── src/                # 方言、能力目录、原生校验与回复转换
│   │   │   └── native/             # Python / JavaScript 桥接资产
│   │   ├── aw-contracts/           # src/、schemas/、tests/
│   │   ├── aw-config/              # 配置解析与静态校验
│   │   ├── aw-provider/            # 通用 Provider 协议与能力准入
│   │   ├── aw-exec/                # 有界进程执行
│   │   ├── aw-host/                # Provider 准备与调用
│   │   ├── aw-core/                # 计划执行 API 与 Journal
│   │   ├── aw-cli/                 # 统一 aw 命令入口
│   │   ├── aw-service/             # 客户端、服务与运行状态
│   │   └── aw-package/             # 当前 Preview 构建、安装和配置编辑
│   ├── tests/                     # 回归矩阵、原生验收清单与共享检查
│   └── scripts/                   # 统一检查入口
├── providers/                     # 具体 Provider 的独立协作目录
│   ├── agent-sec-core/            # 独立 workspace、锁文件、源码与测试
│   └── tokenless/                 # 独立 workspace、锁文件、源码与测试
├── src/agent-sec-core/            # 安全引擎及公开产品接口
├── src/tokenless/                 # 压缩引擎及公开产品接口
└── distribution/                  # OS 组合发行
```

`providers/` 接受第一方和社区实现，也允许 Provider 在外部仓库维护、使用其他语言。
两个收录的 Rust 实现保留 `aw-provider-sec-core`、`aw-provider-tokenless` 包名和二进制名。
AW workspace 的普通依赖和测试依赖均不引用它们。

## Adapter 与服务之间传什么

`aw-cli` 提供统一 `aw` 命令；独立 `aw-package` 可执行文件已删除，Rust crate 保留。
CLI 直接调用 `aw-package`
库完成安装与离线编辑；`aw-build` 仍是 `aw-package` 中的维护者工具。
`aw-service` 以库形式提供客户端与服务；底层库不依赖 `aw-cli`。
启动器和服务共同使用 `aw-adapter`；Adapter 不依赖 service、Host 或 CLI 内部类型。

软件安装使用 `aw install --prefix ... --bundle ...`，原生 Hook 注册使用
`aw agent connect --config ... --agent ...`，旧的 `aw install --config ... --agent ...`
仍兼容。混用安装和原生注册选项会在写入前失败。Preview 仍仅接受现有本地包；
此次拆分不增加 URL 安装器、系统包后端、自动激活或安装记录迁移。

| 接口或状态 | 归属 |
| --- | --- |
| `NativeOptions`、`InstallOptions` | CLI 解析参数后交给 Adapter 的类型化选项 |
| `NativeContext` | 仅包含目标、实例 ID 与工作目录，用于原生事件转换 |
| `LaunchPlan`、`Readiness` | Adapter 返回注册计划；启动器执行并等待就绪 |
| `LaunchFiles`、取消标志 | 启动器提供文件访问与取消信号，保留目录和进程所有权 |
| `Client`、完整 `Binding`、wire 类型、`HookBinding` | 保留在服务与 CLI；Adapter 不持有连接或 RPC 状态 |
| 原生关联、完成阶段与回复 | Adapter 解释原生字段，返回比较数据或回复；CLI 输出并处理退出 |
| 实例、事件、claim、预算、取消、审计 | 服务持有并执行约束 |

Qoder 的步骤排序要求、Hermes 的观察/转换阶段、OpenClaw 的替换后观察，以及
观测来源的 framework 别名归 Adapter。服务继续固定初始快照、检查关联摘要和
单次调用。原始 native bytes 使用服务传入的严格 JSON 解码器核对，不接受回调
自报的 `verified: true`。

运行路径仍为 `aw-service → aw-host → aw-exec → Provider`。结果由 Adapter 转成
原生回复，Agent 决定是否采用。服务复用 `aw-core::FileJournal`，不经过 Core 计划 API。
原生权限、安全拒绝、PreTool 新调用要求和 PostTool 授权文本槽规则保持不变。

## Schema 与演进

能力、计划和证据合同归 `aw-contracts`，配置归 `aw-config`，Provider 公共消息和
View 归 `aw-provider`。原生桥接数据归 Adapter，Provider 私有数据归各实现。

`aw-contracts` 保留 23 份已注册合同；此前八份未注册、无仓库内调用方的能力 v1
草案已删除，历史可从 Git 查阅，不另设 `references/`。计划和证据等活跃 v1 合同仍保留。
迁移未改变活跃 Schema 的字节、`$id`、Registry 名称或摘要向量。

新版本使用独立名称和 `$id`，与受支持的旧版并存，并增加语义校验和正反样例。
目录清单测试要求每份合同都在 Registry 中，防止新增文件未接入运行时。

## 组件 owner 与 OS 发行方

`providers/` 是源码协作空间，构建和发布责任属于各组件。目标是让每个一方 Provider
只依赖固定版本的公共 AW 库，就能在 ANOLISA 仓库外构建、测试和发布；自身的第三方库
和后端依赖由 owner 管理。Provider 不应依赖 AW 服务内部类型、其他 Provider 的源码
或整个 OS 源码树。

| 角色 | 责任与交付物 |
| --- | --- |
| AW 维护方 | 发布公共 SDK、进程协议和兼容性测试，定义版本兼容边界 |
| Provider owner | 实现与契约测试、固定依赖、版本化产物、后端要求及安装说明 |
| OS 发行方 | 选择组件版本、核对来源与校验和、验证组合兼容性，完成安装与升级交付 |
| 第三方实现者 | 按公开进程协议交付程序，通过 YAML 接入；语言与源码仓库自定 |

OS 发行方消费组件产物，不接管各组件的内部实现。随产品交付的后端依赖纳入分发验证，
外部安装的 Provider 则按其公开契约接入。公共 SDK 是源码接口；运行时边界是
JSON/stdio 协议，不承诺 Rust 二进制 ABI 稳定。

### 本次迁移与后续发布

本次已拆出独立 workspace、锁文件和测试入口。两个 Provider 仍通过仓库内路径使用
AW 库，Tokenless 组合测试仍引用 sec-core Provider，尚未达到仓库外独立发布的目标。

当前 Preview 将 core 和 Provider 作为同一批构建产物交付，因此安装校验要求版本和
源码提交一致，以确认组合来源。后续以已发布 SDK 和 Provider 产物建立兼容矩阵，
再调整安装规则，并补齐旧安装回执迁移测试。本次不要求用户迁移配置或回执。

## 回归边界

从仓库根运行 `python3 src/aw/scripts/check.py`。入口分别检查三个 workspace 的
格式、Clippy、locked 测试和 rustdoc，再核对合并后的可运行用例、能力矩阵和
Python/JavaScript 摘要向量。缺失 Provider workspace、反向依赖、漏掉矩阵用例、
被忽略的必跑测试或越界源码都必须失败。

Adapter 单元测试随实现迁移；服务、CLI 和 Provider 的组合测试继续覆盖预算、取消、
安全与压缩。原生桥接资产不改变内容，仅更新引用路径。真实 SDK/CLI 与真实
Tokenless/RTK 的验收保留在独立集成流程，日常门禁通过不代表这些验收已经完成。

依赖表与代码入口见[架构总览](../../../../../docs/developer-guide/zh/aw/architecture.md)，
原生验收边界见[贡献指南](../../../CONTRIBUTING_zh.md)。
