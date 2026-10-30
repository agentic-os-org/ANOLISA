# 本地 Provider 管理

[English](../../en/packaging/package-management.md)

`aw-package` 准入本地 Provider 扩展，并生成或编辑 AW 配置。文件校验、清单和
持久事务位于其私有 `store` 模块。框架与后端通过 Makefile 和 distribution 脚本构建。AW 的 Provider
API 不构建源码，也不安装 core/后端软件。

## 输入与准入

每个 AW bundle 包含一份大小受限的 `package.json` 和 `payload/` 文件树。
描述保存独立 ID、SemVer 版本、Linux 架构、精确依赖、文件 SHA-256 与模式。
每个包必须声明 `aw-provider/v1alpha1`；拒绝 `manifest.json`、`aw-core` 和普通工具。
Provider ID 和版本无需匹配 AW core。后端与其服务始终是单独安装的组件。

公开 `aw_package::plan::install` 在返回不透明的
`PackagePlan` 前执行准入。该类型序列化计划的只读决定与阻碍，
`aw_package::plan::execute` 只接受这个准入类型。通用 bundle 检查、描述与安装
入口保留为 `aw-package` 的私有实现。

生成计划不创建目标 prefix、数据库、锁或日志。它以拒绝符号链接的方式核验普通
文件、同一 prefix 内的精确依赖、目的地冲突与所有权，不下载 URL、不解析产品名，
也不原地替换。删除计划检查反向依赖与所选文件。

## 数据库归属

用户扩展记录位于 `$XDG_STATE_HOME/aw/packages`，回退到
`$HOME/.local/state/aw/packages`。显式 system scope 使用 `/var/lib/aw/packages`，
写入需要 root；有效 UID 不隐式选择 scope。普通用户可以读取 system 清单及其读锁，
事务日志仍仅写入者可访问。

| 数据库路径 | 用途 |
| --- | --- |
| `installed.json` | 带版本的清单、提交编号、安装 ID、Provider 包、文件与精确依赖 |
| `lock` | 查询/配置的共享锁，或事务独占锁 |
| `transactions/<transaction_id>/` | 中断操作的持久日志与发布证据 |

prefix 位置文件将扩展安装绑定到所选数据库与管理者，不重复保存包信息。
AW 拒绝 ANOLISA 所有的位置文件，不导入原生 RPM 管理权。显式 prefix 查询和
配置生成核验后按位置文件读取，也可使用可读的 system 安装。不指定 prefix 时只
读取所选 AW 数据库。文件损坏由 `doctor` 报告；删除全部扩展后保留空登记，便于
安全复用。

通过普通 Makefile 安装的 AW core 二进制没有扩展记录。由 RPM 或其他管理者安装
的 Provider 可以直接通过显式 `transport.argv` 运行，无需 AW 数据库。
带外部路径的配置不会接管这些文件的所有权。

## 事务与恢复

查询持有已有数据库读锁。配置按数据库、prefix 的顺序取得读锁，直到文件核验与
私有输出发布结束。变更按数据库、prefix 的顺序取得写锁，复查计划快照，再调用
私有 `store` 事务实现。私有 staging 和拒绝链接的对象身份检查防止路径替换重定向写入。

持久替换 `installed.json` 表示操作提交。事务中断后，后续变更会阻断，直到显式
`aw recover` 完成已提交清理或回滚未提交写入。恢复保留冲突字节并报告阻碍，
不覆盖它们。包引擎的文件、字节、操作数和日志上限在变更前及持锁后都生效。

## 运行时文件引用

`aw-exec::PackageLeases` 为已知可执行文件和 argv 中字面的绝对路径持有已有
prefix 读锁。CLI、服务和 Host 在所属进程运行与清理期间保留这些锁。这个
`aw-exec` 辅助模块不读集中清单，也不创建包记录。没有托管位置文件的普通或原生程序仍可使用。

这些引用不能发现任意外部进程或 shell 动态生成的路径，因此包卸载只检查能够
建立的证据；后端生命周期与原生包卸载仍由其 owner 负责。原生 Hook 注册使用
`aw agent connect`，与 Provider bundle 安装分开。

## 验证

AW 准入测试覆盖非 Provider 与 manifest 输入拒绝、独立 Provider ID/版本、精确
依赖、过期计划、文件所有权与恢复。`store` 单元测试覆盖底层有界文件事务。
Provider 协议与原生 Agent 采用测试另行执行；安装 bundle 不等于框架采用了效果。
