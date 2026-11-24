# Lua-LSM 文件保护后端

[English](README.md)

Lua-LSM 后端与 AgentSight 共用 Cedar 文件策略解析，独立生成 Lua，
直接读写 securityfs，不向 AgentSight 发请求。内核基线为
`92a8552ad51490433034423ef33c3f61fcb3cdb9`、ABI version 1。
旧版 inode 标签模板的原生 Client 防护用例已在 Alibaba Cloud Linux 4、内核
`6.6.102-008.0.2.ali6000.pro.alnx4.x86_64` 上通过。隔离运行的 daemon 在同一台主机上下发了
10 个 Binding，并确认正常重启后防护继续生效。当前路径匹配模板及安装包的 systemd 部署仍待实机验证。
随后同机的完整 daemon 测试在卸载模块时触发内核崩溃。Lua-LSM 负责人已确认是已知内核问题，正在回移修复，修复版本待实机验证。

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
Lua-LSM 只接受 Cedar 策略。SecCore 原生模板在翻译时返回 `LUA_LSM_CEDAR_REQUIRED`。
不支持的策略整体拒绝。SecCore 的 AgentSight Translator 支持精确文件和目录树禁读。
模块名为 `asc_{binding_id}_r{binding_revision}`。PAP/Scope/Binding 对外字段及
reconciler 的四个 Client 方法保持兼容。

Lua-LSM 的普通禁写同时约束修改内容和创建命中的目录项，覆盖普通文件、目录、符号链接、
特殊文件、硬链接和 rename 目的名称，包括对精确文件的原子替换。排除项继续生效。
Schema 区分 `create` 与 `write`，本后端不支持单独的 create-only 策略。
AgentSight 按其自身契约执行。禁写策略要求内核识别 18 个 hook，包括目录项操作和两种
truncate hook。缺少必要 hook 的内核无法确认部署成功。

Cedar 表达式 `resource.path like "/data/*.txt"` 同时匹配 `/data/a.txt` 和
`/data/nested/a.txt`。Cedar 的 `*` 可跨目录，`?` 按字面匹配，`\*` 表示字面星号。
对于 `resource.path like "/data/*" && resource.path like "*.txt"`，Lua-LSM Translator
选取 `/data` 作为扫描目录，Lua 执行两个条件。每个可独立成立的分支都要有正向条件
限定 SecCore 挂载视图中的范围。可以在文件或目录出现前下发规则。每次操作匹配当时的路径，
此后新建的目录项、同名替换后的文件都受约束。精确排除只作用于该目录项，子树排除覆盖其后代。

如果目标位于 overlayfs，或保护的目录树包含 overlayfs 子挂载，下发会返回
`LUA_LSM_OVERLAYFS_UNSUPPORTED`。空目录和所有文件操作都适用。目标尚不存在时，
按最近已存在的祖先判断挂载类型。即使根文件系统是 overlayfs，单独挂载的 tmpfs 仍可受保护。
部分 overlayfs 挂载下已有映射的设备号、inode 与目标路径不一致，因此所有内核版本都执行此检查。

例如，用 `resource.path like "/work/secrets/*"` 禁止写入后，即使内核回收并重新载入 inode，
`/work/secrets/key` 仍受保护。如果同一文件另有硬链接 `/tmp/key`，规则仍可下发。
通过范围外名称读写文件或删除该名称，暂时不在防护保证内。通过范围内名称访问仍按规则执行。
受控进程尝试跨保护边界建立新硬链接时，操作也可能被拒绝。

Client 在注册时解析目标或其祖先路径上已有的符号链接。悬空目标链接会被拒绝，改变链接指向后需重新部署。
树中新建的符号链接不会将外部引用目标纳入范围。Cedar `like` 按顺序比较字面片段。
每个模式最多 4,096 字节、64 个未转义的通配符。

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

注册后，Client 从已核验的根进程开始，给已有进程标记成员身份。进程在访问文件或 fork 时，
如果能在自己的 PID namespace 中找到同一个已标记的根任务，也可通过当前祖先链确认归属。
fork 继承成员身份，exec 保留身份。私有命令要求调用者具备初始 user namespace 的
`CAP_MAC_ADMIN`，以 `EALREADY` 确认。进程成为成员后，文件 hook 即按当时的路径执行策略。
初始化失败时可能保留部分防护，直到重试或清理。

模板在访问任务属性前将 `current` 绑定到模块环境。文件和目录 inode 均不保存策略状态。

映射预检按设备号和 inode 匹配 `/proc/<pid>/maps` 与策略范围内的文件。首次下发的最终预检
覆盖两轮扫描中发现且仍存活的进程，包括首轮发现后被重新托管到树外的后代。
首轮快照仅保存在本次下发的内存中。如果一个后代尚无成员标签，在被任一轮扫描发现前就被托管到树外，
即使 Binding 进入 `Ready`，该进程仍可能不受保护。需要覆盖全部后代时，应在首次下发期间保持进程树稳定。
策略更新时检查旧模块保有的成员。禁读时拒绝已有文件映射，包括 private 和
PROT_NONE；禁写时拒绝已有共享可写映射。读取映射失败会阻止下发。已有可写 FD
和只读共享映射可以保留，后续写入及共享映射写权限升级由文件 hook 检查。
从读取进程的映射快照到相关 hook 开始阻断访问之间，进程仍可能建立新的映射。
AgentSight 的预检也有这一限制。

更新时向旧模块查询并迁移已有成员，不通过重新遍历父子关系替代成员身份。
原子成员/退出计数用于确认并发 fork、退出及孤儿后代、线程的接管完成，再卸载旧版。
每次注册持久化独立 instance 标识，避免重新注册后误用旧迁移标记。迁移未完成时
保留旧防护，沿用现有重试策略。

Client 持久化 boot ID、完整部署输入摘要（含源码和进程身份）和初始化完成状态。
同一 boot 内重启 daemon 会保留 Ready 和混合 Binding 的两端回执。等待中的工作继续执行，
中断的 apply/delete 按原重试预算恢复。重试 apply 时，Client 通过内核模块列表和保存的记录复核，
记录匹配且已完成时跳过初始化；匹配但未完成时，在已注册的模块上重新执行初始化。
模块缺失时向原进程重下发；只有名称、缺少匹配
证据时不覆盖、不接管。卸载前先使本地证据失效。模板升级需要新 revision 或显式清理。

## 仍需注意的边界

- 已有映射预检每次最多扫描 65,536 个目录项。从发现进程到防护生效期间，工作负载仍可运行。
- 文件 hook 使用已解析的命名空间路径。目录项 hook 只提供文件系统内的相对路径和文件系统类型，
  Client 根据注册时的挂载信息还原策略路径。同类型的不同文件系统存在同名相对路径时，可能误拒绝。
  下发时包含已核验根进程挂载视图内已有的 bind 别名。新增别名、变更命名空间、chroot 或替换挂载后，
  需要重新部署以覆盖新视图。procfs 的 PID 视图须与 daemon 一致。

- 首次下发、或缺少已确认旧模块的恢复，仍要求原根进程存在，不能从重用的 PID
  重建已丢失的内核成员身份。并发 fork/退出已有确定性模拟覆盖，持续内核压力
  及 namespace 组合仍未验证。
- **PID 复用的已知问题。** 部署或迁移成员身份时，Lua 按 PID 给已有后代打标。
  如果 PID 在 SecCore 核验进程身份后被复用，Lua 可能给无关进程打标。
  打标后的身份复核即使失败，标签仍然保留，该进程及其后续创建的子进程都可能受到此 Binding 的限制。
  解决此问题需要 Lua-LSM 提供接口，在接收标签的同一个 task 对象上核验预期的进程实例身份。
- 跨读写保护范围的移动和新建硬链接、涉及模式或排除项的目录移动，以及在保护路径或其祖先上的
  挂载操作，可能被保守拒绝。

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
下表列出默认忽略的[原生 Client 用例](tests/client.rs)场景；已有实机记录使用旧版模板。

| 维度 | Lua-LSM 用例场景 | 对标差异或剩余边界 |
| --- | --- | --- |
| 权限独立性 | 文件/目录树分别只禁读、只禁写、只禁删；其它操作、范围外路径、无关进程和解绑后恢复正常 | AgentSight 目录树禁读要求后端具备 `directory_tree_read_protection` 能力 |
| 读入口 | 已有 RW FD、pread/readv/preadv、dup、同进程 SCM_RIGHTS、sendfile/splice/copy_file_range、符号链接、private/shared/可执行映射、PROT_NONE 升级和加载可执行文件 | 启用前、解绑后的正向对照用于排除环境自身拒绝；跨进程 SCM_RIGHTS 已在隔离 QEMU 客体通过，宿主内核仍待复测 |
| 写入口 | 已有 RW FD、write/pwrite/writev/pwritev、truncate/ftruncate/O_TRUNC、打洞、copy_file_range、共享可写 mmap 和 mprotect | 拒绝后核对内容不变；private COW、private 升级、读和 chmod 保持可用 |
| 已有映射 | 拒绝已有 private/shared/PROT_NONE 读映射和共享可写映射；更新失败保留旧版；unmap 后重试 | AgentSight 还保守拒绝可写 FD/共享映射；Lua 依靠映射 hook 支持实测的较小拒绝范围 |
| 删除与路径变更 | unlink/rmdir、移出保护树、覆盖目标、双向 exchange、移动祖先，以及仅禁删时的树内改名 | 禁写还会拒绝创建命中的 rename 目的名称 |
| 文件对象身份 | 硬链接别名、符号链接读取、新建树成员、排除子树的正向对照 | Lua 对匹配路径独立防护，范围外硬链接访问暂不保证 |
| 进程身份 | 下发前已有子进程、新线程、fork/exec/setsid、模块仍加载时更新孤儿后代的防护、独立 PID namespace 内的子进程 | 直连 Client 测试由调用方负责清理。daemon 在根进程退出后调度清理。user namespace 和持续进程抖动尚未验证 |
| 挂载视图 | 独立 mount namespace、既有 bind 别名、拒绝覆盖保护根、允许范围外挂载 | Host 视图保持不变；move_mount/remount、叠层挂载与 chroot/pivot_root 待补测 |
| 权限边界 | setuid 后保护仍在；同 UID 无关进程可读；非特权进程不能卸载或访问私有控制文件 | 完整 systemd capability/挂载配置待验收 |
| Binding 隔离 | 同一 inode 叠加读/写 Binding；分别删除不影响另一条；清理后同名重新注册 | 并发更新/删除及中断清理仍需压力测试 |
| 恢复 | 同 boot Client 确认、更新成功与失败、显式清理，以及带 10 个有效 Binding 的 daemon 正常重启 | 崩溃恢复、数据库和状态目录故障注入待验收 |

上线前仍需补充：user namespace、chroot/pivot_root 及其它挂载操作；跨 Binding 的排除项和
重叠目录树、文件替换、深目录/非 ASCII 路径；fork/退出/建映射/更新竞争；实机
审计和运行时异常证据；安装包的 systemd 部署与重启。tmpfs、网络
文件系统分别验收。模板之外的 Lua 回调/VM 故障仍沿用内核当前的失败行为。

## 验证与回退

在 `src/agent-sec-core/v2` 下执行：

```sh
cargo test -p asc-lua-lsm -p asc-lua-lsm-client -p asc-policy-translator-agentsight
LUA_BIN=/path/to/lua5.1 cargo test -p asc-lua-lsm lua51_template_behavior -- --ignored
```

Rust 测试检查请求字节、错误、更新顺序及重启证据；Lua 5.1 用例使用模拟内核对象。
本地 procfs 回归还检查后代持有映射、文件/目录树身份、范围外硬链接映射和排除项，
这些预检测试不修改内核状态。
Ubuntu CI 执行 Lua 5.1 模拟用例，包括 Cedar 路径匹配。Repository/daemon
测试覆盖根进程退出后的清理和重启恢复。内核防护用例需要在专用主机上手工执行。
运行原生 exec 和 mmap 用例时，将 `TMPDIR` 设在可写且允许执行的非 overlay 文件系统上。

```sh
cargo test -p asc-lua-lsm live_kernel_ -- --ignored --test-threads=1
```

旧版 inode 标签模板的 15 组原生用例已于 2026-11-24 在 ABI 1 测试机全部通过，直接调用 Translator 和 Client。
覆盖文件和目录树共用尚未创建的祖先、首次拒绝创建后无残留目录项、原子替换、硬链接、
符号链接、FIFO、原生 glob、Cedar `like`、排除项及映射预检。另一次测试通过 daemon 为两种语法各下发
5 个 Binding，确认正常重启后防护持续生效，解绑后访问恢复。
降级到没有 Lua-LSM Client 的 daemon 之前，先用当前 daemon 删除所属 Scope。确认清理
完成，并在 `/sys/kernel/security/lua/modules` 中逐一核对回执里的模块名已消失。策略
数据库和 Client 状态需一起保留。只切换默认后端时，已有模块仍会执行策略。

如果已经降级，优先恢复兼容的 daemon 完成清理。紧急人工卸载时，由初始 user namespace
中具有有效 `CAP_MAC_ADMIN` 的管理员，将 `module` 设为该 Binding 清理回执中的确切模块名，
执行下面的命令并核对该名称已消失。写入报错或模块仍在列表中时，清理尚未确认。
随后用兼容的 daemon 完成持久化回执的对账。

```sh
printf '%s' "$module" > /sys/kernel/security/lua/unregister
cat /sys/kernel/security/lua/modules
```

V1 关系：新增 V2 后端，Python V1 不变。内部调整包括共用意图、直接 Client、
后端选择与恢复，保留现有 Client 接口和持久化 TargetRef 结构。
