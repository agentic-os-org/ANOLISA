# Lua-LSM 文件保护后端

[English](README.md)

Lua-LSM 后端与 AgentSight 共用 Cedar 文件策略解析，独立生成 Lua，
直接读写 securityfs，不向 AgentSight 发请求。内核基线为
`92a8552ad51490433034423ef33c3f61fcb3cdb9`、ABI version 1。
原生 Client 防护用例已在 Alibaba Cloud Linux 4、内核
`6.6.102-008.0.2.ali6000.pro.alnx4.x86_64` 上通过。隔离运行的 daemon 在同一台主机上下发了
10 个 Binding，并确认正常重启后防护继续生效。安装包的 systemd 部署仍待验证。
随后在同机执行的完整 daemon 测试中，模块开始卸载后发生内核崩溃，原因尚未确定。

## 配置与生命周期

daemon 环境设置 `AGENT_SEC_FILE_BACKEND=lua-lsm`。默认仍为 `agentsight`，
其它值使启动失败；已有部署保留原 route，Gateway 凭证下发不变。
管理员可给当前 system unit 添加以下 drop-in，并在目标发行版验证：

```ini
[Service]
Environment=AGENT_SEC_FILE_BACKEND=lua-lsm
CapabilityBoundingSet=CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER CAP_SYS_PTRACE CAP_MAC_ADMIN
ReadWritePaths=/sys/kernel/security/lua
```

进程需要在初始 user namespace 中实际具备 `CAP_MAC_ADMIN`。
Client 私有状态位于 `/var/lib/agent-sec/lua-lsm`，恢复时应与 Policy 数据库一起保留。
停止 SecCore 时 Lua 模块继续运行。发现根进程退出或管理员删除 Scope 时，daemon 会调度
Binding 清理。模块卸载后，仍存活的后代失去这条 Binding 的保护。
删除 Policy Template 仍保留已有 Scope 的冻结快照。

## 实现

生产代码按 AgentSight 相同的 Translator／协议／Client 职责拆分：

- `asc-policy-translator-lua-lsm` 将 Binding 翻译为 Lua 产物，不执行 I/O。
- `asc-lua-lsm-protocol` 定义内部计划、模块名和标准 Lua 模板编码，供生成及
  Client 源码一致性校验共用。
- `asc-lua-lsm-client` 负责 securityfs 读写、初始化、恢复及清理，不依赖 Translator
  或 Cedar。
- `asc-file-policy` 只保存后端共享的数据类型；共用的 Cedar 翻译位于
  `asc-policy-cedar::file_protection`。

`asc-lua-lsm` 保留兼容导出和集成测试命令，daemon 直接使用具体 crate。
内部计划不属于内核 ABI：`register` 接收 Lua 源码，`unregister` 接收模块名。

一个模板分别处理读、普通写和删除，支持精确普通文件、目录树、路径模式及各目标的排除项。
不支持的策略整体拒绝。SecCore 的 AgentSight Translator 支持精确文件和目录树禁读。
模块名为 `asc_{binding_id}_r{binding_revision}`。PAP/Scope/Binding 对外字段及
reconciler 的四个 Client 方法保持兼容。

普通写保护在创建文件或子目录前检查新条目的名称。模板测试验证了两个创建 hook 会拒绝
命中的名称并允许例外；拒绝后是否不留下目录项，修订后的 17-hook 模板尚待宿主内核复测。
已驻留的 15-hook
旧版写策略在替换为新 revision 前沿用原有行为。Client 确认新版写策略需要全部
17 个 hook。

原生文件规则中，`*`、`?` 在单个路径段内匹配，整段 `**` 可跨零层或多层目录。
例如 `/data/*.txt` 匹配 `/data/a.txt`，`/data/**/*.txt` 还匹配
`/data/nested/a.txt`。Cedar 表达式 `resource.path like "/data/*.txt"` 对两个文件
都生效：Cedar 的 `*` 可跨目录，`?` 按字面匹配，`\*` 表示字面星号。
对于 `resource.path like "/data/*" && resource.path like "*.txt"`，Lua-LSM Translator
选取 `/data` 作为扫描目录，Lua 执行两个条件。每个可独立成立的分支都要有正向条件
限定 SecCore 挂载视图中的范围。扫描根须存在，初始匹配数可以为零。
初始化的 65,536 个对象上限按前缀下扫描到的条目计算。

Lua 模块在初始化、新建文件和建立硬链接时匹配路径。命中标记随 inode 保留至模块卸载，
其它硬链接共享保护。受控进程的部分改名和建链操作可能被拒绝，包括需要重标后代的
目录移动。如果其它进程搬动已有目录树，需要重新部署以按新路径核对已有后代。
初始化期间，Client 使用 Lua 返回的 inode 命中结果检查已有内存映射。

混合 Cedar Binding 共用一份已校验的 PST：选定的文件 Translator 消费文件投影，
Gateway 消费网络投影。所有计划完成 prepare 后才修改目标，清理 Binding 时会清理两端部署。

`prepare_apply` 只做内部校验；`create` 一次写完整源码；`update` 先确认新版，
再卸载旧版。初始化失败时旧模块保持注册。`delete` 按保存的 name 卸载。
短写、`EBUSY`、无法确认的既有模块保持 Unknown。同步修改操作不会被后台遗留。

Client 在写入 `register` 前先保存本地部署记录。写入返回完整源码长度表示内核已
同步注册模块。Client 随后读取 `modules`，核对模块名和已识别的 hook 数（`nlsm`），
待初始化完成并保存结果后报告 Present。更新时先完成新版部署，再卸载旧模块，
随后回读确认新版仍在列表中。

`unregister` 返回完整长度或 `ENOENT` 后，Client 都会读取 `modules`。只有成功读取
列表且目标模块名已消失，才报告 Absent。回读无法确认的目标记为 Unknown，协调器据此
保留待处理或失败状态，不能将该次下发或删除记为成功。

注册后，Client 在 SecCore 的文件视图下通过 `O_PATH` 固定文件对象，再打开私有
命令文件。模板通过 `/proc/self/fd` 给真实 inode 打标，保持硬链接和跨文件系统
的对象身份。Client 从已核验的根进程开始，给已有进程打标。尚未打标的进程在访问文件或
fork 时，如果能在自己的 PID namespace 中用保存的 PID 找到同一个已标记根任务，
也可通过当前祖先链确认归属并补标。fork 继承标记，exec 保留标记。命令只接受初始 user
namespace 中具备 `CAP_MAC_ADMIN` 的调用者，以 `EALREADY` 确认成功。
这些命令由模板的文件打开 hook 处理。进程和 inode 都打标后，hook 立即按策略执行。
因此初始化期间即可生效，下发失败也可能保留部分防护，直到重试或清理。

对象及共享缓存只保存标量。模板将排除路径转换为 Lua 表中的状态转换，在 inode 上
保存数字状态编号；访问 `current` 的缓存前通过 `debug.setfenv` 绑定模块环境，
弥补基线分发器创建该 userdata 时缺失模块绑定的行为。确认码直接使用 Linux
EALREADY（114），因为 Lua errno 库未导出它；位掩码计算也不依赖缺失的 math 库。
`inode_setattr` 兼容实测内核的两个参数和源码核对基线的三个参数；仅凭 ABI version 1
无法判定回调参数数量。

映射预检按设备号/inode 匹配 `/proc/<pid>/maps` 与策略范围内的文件，覆盖首次下发
的进程树和更新时旧模块保有的成员。禁读时拒绝已有文件映射，包括 private 和
PROT_NONE；禁写时拒绝已有共享可写映射。读取映射失败会阻止下发。已有可写 FD
和只读共享映射可以保留，实机用例验证后续写入及共享映射写权限升级会被拦截。
从读取进程的映射快照到相关 hook 开始阻断访问之间，进程仍可能建立新的映射。
AgentSight 的预检也有这一限制。

更新时向旧模块查询并迁移已有成员，不通过重新遍历父子关系替代成员身份。
原子成员/退出计数用于确认并发 fork、退出及孤儿后代、线程的接管完成，再卸载旧版。
每次注册持久化独立 instance 标识，避免重新注册后误用旧迁移标记。迁移未完成时
保留旧防护，沿用现有重试策略。

Client 持久化 boot ID、完整部署输入摘要（含源码和进程身份）和初始化完成状态。
启动时将 Ready Lua Binding 重新入队，Client 通过内核模块列表和保存的记录复核，
记录匹配且已完成时跳过进程和文件打标；匹配但未完成时，在已注册的模块上重新执行初始化。
模块缺失时向原进程重下发；只有名称、缺少匹配
证据时不覆盖、不接管。卸载前先使本地证据失效。模板升级需要新 revision 或显式清理。

## 仍需注意的边界

- 初始化最多处理 65,536 个文件对象。从发现进程到防护生效期间，工作负载仍可运行。
- 验收机之外的文件系统、user namespace、chroot/pivot_root 及其它挂载操作待实测。
  独立 PID/mount namespace 和既有 bind mount 别名已有实机覆盖。procfs PID
  视图必须与 daemon 一致；前后身份校验没有增加内核侧原子的 start-time 比较。
- 首次下发、或缺少已确认旧模块的恢复，仍要求原根进程存在，不能从重用的 PID
  重建已丢失的内核成员身份。并发 fork/退出已有确定性模拟覆盖，持续内核压力
  及 namespace 组合仍未验证。
- 当前基线缺少可用的 rename 完成回调。部分跨读写保护范围及涉及排除项的移动
  会被保守拒绝；link/rename 提交前的标记可能在操作失败后保留，比部分
  AgentSight 操作更严格。为避免替换受保护拓扑，也会拒绝在保护对象及其祖先上
  挂载、移动挂载。
- 已有内存映射和已读数据不能撤回。模板内异常拒绝并区分审计原因；进入回调前的
  错误沿用内核行为。审计尽力写入。
- 原生接口没有 LIVE、源码摘要或 CAS。确认依赖一个可信 SecCore 写者，缺失证据时
  不推测成功。
- io_uring/AIO、跨进程内存访问和异步代理操作未纳入已验证的读写防护承诺。

## 与 AgentSight 的覆盖对照

对照依据为当前 checkout 的
[读与运行时验收](../../../../agentsight/crates/agentsight-enforcer/tests/inode_runtime.rs)、
[写保护](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_write_tests.rs)、
[删除](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_delete_tests.rs)、
[rename](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_rename_tests.rs)
和[映射预检](../../../../agentsight/crates/agentsight-enforcer/src/actplane/preexisting_maps.rs)。
本轮核对了这些 AgentSight 源码和用例，没有重跑 BPF-LSM 测试。
下表 Lua 侧证据来自默认忽略的[原生 Client 用例](tests/client.rs)和隔离 daemon 重启测试。

| 维度 | Lua-LSM 实机检查 | 对标差异或剩余边界 |
| --- | --- | --- |
| 权限独立性 | 文件/目录树分别只禁读、只禁写、只禁删；其它操作、范围外路径、无关进程和解绑后恢复正常 | AgentSight 目录树禁读要求后端具备 `directory_tree_read_protection` 能力 |
| 读入口 | 已有 RW FD、pread/readv/preadv、dup、同进程 SCM_RIGHTS、sendfile/splice/copy_file_range、符号链接、private/shared/可执行映射、PROT_NONE 升级和加载可执行文件 | 启用前、解绑后的正向对照用于排除环境自身拒绝；跨进程 SCM_RIGHTS 已在隔离 QEMU 客体通过，宿主内核仍待复测 |
| 写入口 | 已有 RW FD、write/pwrite/writev/pwritev、truncate/ftruncate/O_TRUNC、打洞、copy_file_range、共享可写 mmap 和 mprotect | 拒绝后核对内容不变；private COW、private 升级、读和 chmod 保持可用 |
| 已有映射 | 拒绝已有 private/shared/PROT_NONE 读映射和共享可写映射；更新失败保留旧版；unmap 后重试 | AgentSight 还保守拒绝可写 FD/共享映射；Lua 依靠映射 hook 支持实测的较小拒绝范围 |
| 删除与路径变更 | unlink/rmdir、移出保护树、覆盖目标、双向 exchange、移动祖先；树内正常 rename 可用 | 部分读写树移动和跨排除项操作仍更严格 |
| 文件对象身份 | 硬链接别名、符号链接读取、新建树成员、排除子树的正向对照 | Lua 保护同一 inode 的所有别名；AgentSight 部分结构化测试允许范围外别名写/删，语义有差异 |
| 进程身份 | 下发前已有子进程、新线程、fork/exec/setsid、模块仍加载时更新孤儿后代的防护、独立 PID namespace 内的子进程 | 直连 Client 测试由调用方负责清理。daemon 在根进程退出后调度清理。user namespace 和持续进程抖动尚未验证 |
| 挂载视图 | 独立 mount namespace、既有 bind 别名、拒绝覆盖保护根、允许范围外挂载 | Host 视图保持不变；move_mount/remount、叠层挂载与 chroot/pivot_root 待补测 |
| 权限边界 | setuid 后保护仍在；同 UID 无关进程可读；非特权进程不能卸载或访问私有控制文件 | 完整 systemd capability/挂载配置待验收 |
| Binding 隔离 | 同一 inode 叠加读/写 Binding；分别删除不影响另一条；清理后同名重新注册 | 并发更新/删除及中断清理仍需压力测试 |
| 恢复 | 同 boot Client 确认、更新成功与失败、显式清理，以及带 10 个有效 Binding 的 daemon 正常重启 | 崩溃恢复、数据库和状态目录故障注入待验收 |

上线前仍需补充：user namespace、chroot/pivot_root 及其它挂载操作；跨 Binding 的排除项和
重叠目录树、文件替换、深目录/非 ASCII 路径；fork/退出/建映射/更新竞争；实机
审计和运行时异常证据；安装包的 systemd 部署与重启。overlayfs、tmpfs、网络
文件系统分别验收。模板之外的 Lua 回调/VM 故障仍沿用内核当前的失败行为。

## 验证与回退

在 `src/agent-sec-core/v2` 下执行：

```sh
cargo test -p asc-lua-lsm -p asc-policy-translator-agentsight
LUA_BIN=/path/to/lua5.1 cargo test -p asc-lua-lsm lua51_template_behavior -- --ignored
```

Rust 测试检查请求字节、错误、更新顺序及重启证据；Lua 5.1 用例使用模拟内核对象。
本地 procfs 回归还检查后代持有映射、文件/目录树身份、范围外硬链接映射和排除项，
这些预检测试不修改内核状态。
Repository/daemon 测试覆盖根进程退出后的清理和重启复核。专用内核主机上显式运行：

```sh
cargo test -p asc-lua-lsm live_kernel_ -- --ignored --test-threads=1
```

14 组实机用例已于 2026-11-20 在 ABI 1 测试机全部通过，直接调用 Translator 和 Client，
覆盖原生 glob、Cedar `like`、排除项及映射预检。另一次测试通过 daemon 为两种语法各下发
5 个 Binding，确认正常重启后防护持续生效，解绑后访问恢复。
回退时先显式清理 Scope；只切换默认后端不会迁移或卸载
已有部署。

V1 关系：新增 V2 后端，Python V1 不变。内部调整包括共用意图、直接 Client、
后端选择与恢复，保留现有 Client 接口和持久化 TargetRef 结构。
