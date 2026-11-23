# ActPlane 文件拦截兼容 Profile

[English](actplane-file-enforcement-profile.md)

## 状态

已实现，并在内核版本为 `6.6.102-6.alnx4.x86_64` 的 Alibaba Cloud Linux
上完成验证。这是针对 ActPlane 官方版本
`a62e5d9d96f91101cda019519053e950d532380a` 的临时兼容层；上游合入等价能力
后应删除该兼容层。

## 决策

AgentSight 继续将 ActPlane 作为固定 revision 的 Cargo dependency，不复制其源码，
也不增加 Git submodule。enforcer 构建过程拉取该上游 revision，并应用
`patches/actplane/` 中的补丁队列。如果补丁无法应用，或源码树包含任何未经评审的
变化，构建会直接失败，从而显式暴露上游漂移。

systemd service 通过 `ACTPLANE_PINNED_PROFILE=file-enforcement` 选择兼容
profile。该 profile 只预留第一阶段渐进拦截所需的 map、tracepoint 和 BPF-LSM
程序：阻止 Agent 进程打开配置的敏感文件。如果 BPF LSM 未启用、profile 名称未知，
或现有 bpffs singleton 的 hook 集合不一致，启动会直接失败。
singleton 还会携带包含 ActPlane revision、profile 和兼容 schema 的 pinned metadata
alias；metadata 缺失或不匹配会被视为需要显式迁移的错误。

## 为什么需要精简 Profile

固定的上游全量 profile 在已验证内核上存在两个相互独立的 verifier 加载问题：

1. `trace_openat_exit` 和 `trace_rename_exit` 的 BPF 组合栈需要 544 字节，
   超过内核 verifier 的 512 字节限制。
2. `enforce_path_truncate` 调用 `bpf_d_path`，但已验证内核不允许该
   BPF-LSM hook 使用此 helper。

精简 profile 不会削弱已声明的文件打开策略。它只是不加载当前产品范围之外的
数据流 tracepoint 和文件 hook。网络拦截、进程执行拦截、taint 传递和时序策略
执行仍属于后续 profile，不能宣称已由本 profile 启用。

## 精确文件禁读

结构化契约增加可选的 `protections.read`，默认 false。
HTTP 模板通过独立的 `POST /api/enforcement/file-read-bindings` 接口选择该保护，
旧服务端会返回 404，而不是忽略未知字段后只下发禁止打开的策略；目录树禁读目标遵循下文的
独立契约，不支持该契约的后端会拒绝。协议保持 v5，策略 schema 版本不变：省略 false 字段保留旧载荷，
旧后端遇到值为 true 的未知字段会拒绝策略。

激活时对已持有的设备/inode 身份安装 `INODE_GUARD_READ`。文件打开、权限、
mmap 和 mprotect 钩子在深层策略求值前检查守卫，事件操作号 9 映射为 `read`。
`agent-file-guard` pinned marker 提升到 v6，并预留映射钩子；仅在全部所需 link
存在时，上报 `file_read_protection`。守卫覆盖别名及已打开的只读 FD，不影响未绑定
进程，也不隐含禁止删除或写入。

准入检查使用目标拓扑解析出的身份，拒绝已有的全部文件映射，包括私有映射与
PROT_NONE 映射。检查无法撤回已读取的内存，也不提供进程树或文件访问并发变化
期间的原子隔离保证。外部文件替换或密钥轮换后需重新下发；动态拓扑及 io_uring/AIO
仍在覆盖范围外。实测在 ALinux 6.6 上使用模拟文件及独立 enforcer、HTTP 服务、
UDS 和 pin root。

## 目录树禁读

健康接口上报 `directory_tree_read_protection` 时，`directory_tree` 目标接受
`protections.read`。禁读接口新增可选字段 `kind: "directory_tree"`；旧服务端忽略该字段后会因
目录不是普通文件而拒绝，旧 enforcer 在规范化阶段拒绝该目标，双方都不会静默缩小请求范围。

inode 守卫无法覆盖 Apply 之后新建的目录项，因此目录树禁读复用删除和普通写保护的按名称祖先
分类。降级时只为目录树禁读目标设置策略域 `read_mask`；精确文件禁读仍使用 `INODE_GUARD_READ`，
只含精确文件的策略不会增加每次读取的分类开销。设置该掩码后，打开、读权限、mmap 和 mprotect
钩子对普通文件分类，仅 PROTECTED 结果拒绝。UNKNOWN 与普通写保护一样放行，因为内部挂载上的
目录项无法走到命名空间根。列目录和元数据不受管控。

按名称分类意味着绑定进程可以通过新名称把 inode 带出范围。新增的 `path_link` LSM 钩子拒绝会
丢失禁读位的硬链接（事件操作号 10，`link`）；重命名守卫拒绝丢失禁读位的移动，以及替换或交换
稳定根目录或其祖先目录，否则替换后的目录会在之后接收未受保护的密钥。钩子看不到已有的外部
硬链接，因此 Apply 会在目标挂载命名空间中遍历已持有的目录树，不跟随软链接，拒绝链接数大于 1
的普通文件，跳过子树排除项，最多检查 100,000 个目录项。遍历同时记录范围内每个文件的身份，已有映射
既按身份也按目录树前缀拒绝：通过 bind 别名建立的映射能被识别，前缀匹配仍可覆盖已从目录树中删除但
仍被映射的文件。未绑定进程
之后创建的硬链接，以及未绑定进程替换根目录，仍在覆盖范围外。

激活记录从 56 字节增至 64 字节，因此所有 pinned profile marker 都会提升（file v8、credential v9、
full v9、agent-file-guard v10），旧 pin 会被重新安装，而不是按新布局误读。验证覆盖 Anolis
5.10.134（BPF LSM，回合了 `bpf_loop`）上的 inode-only 对象，使用私有 pin：引擎 map 级测试，
以及在 bind 别名、下层挂载、排除项和已持有 FD 条件下下发真实目录树策略的 enforcer 测试。
面向 `bpf_d_path` 内核的 full 对象和两个 aarch64 对象已重新编译，但未在真实内核上加载。

## 构建约定

`make build-enforcer` 会调用 `scripts/build-enforcer.sh`。该脚本：

1. 使用与 AgentSight Cargo workspace 声明一致的 ActPlane revision；
2. 支持通过 `ACTPLANE_SOURCE_DIR` 指定经过认可的镜像或预拉取 checkout；
3. 否则在 Cargo target 目录中执行浅 fetch；
4. 校验 checkout revision，并且只应用一次兼容补丁；
5. 校验补丁后 loader 和官方预编译 BPF object 的 blob，拒绝 tracked、untracked、
   ignored 或 dirty-submodule 输入，并禁止 `ACTPLANE_REBUILD_BPF`；
6. 仅构建启用真实 ActPlane backend 的 `agentsight-enforcer`。

源码目录会被缓存以支持可重复构建，并通过构建锁避免并发修改。如果缓存不完整、
revision 不匹配、源码校验失败或补丁不匹配，脚本会给出可操作错误并停止，不会静默
回退到 mock backend。

## 已验证闭环

### Pinned runtime 重启边界

AgentSight 会复用 `/sys/fs/bpf/actplane/v1` 下 pinned 的 ActPlane map 和
link。仅清理策略状态并不会消费已经写入 pinned ring buffer 的记录；旧
domain 事件可能耗尽新 enforcer 的轮询预算，延迟当前违规事件的交付。

因此，enforcer 按以下顺序准备由其独占的 singleton runtime：

1. 获取 ActPlane runtime lock，并保护 enforcer 进程；
2. 清理旧策略和 capability 状态；
3. 在创建实时 binding registry 前排空有限的 pinned event queue；
4. 启动实时 poller，然后才报告 backend ready。

先清理 capability 状态可以避免旧 binding 在 drain 期间持续产生事件。
无法打开或排空 ring buffer 时，backend 初始化必须失败；如果不能证明事件流
从干净的 ownership boundary 开始，服务就不能接受策略请求。每次重装 pin
tree 会破坏 singleton 生命周期，而只忽略未知 domain ID 仍会让旧记录阻塞在
新事件之前，因此这两种方案都不采用。

隔离的端到端验证使用 `127.0.0.1:17400` 上的 AgentSight、独立 UDS 和独立
bpffs root，未改动 7396 端口上的现有服务。验证覆盖以下顺序：

1. 使用存活 Python 进程及其 `/proc` start time 应用策略 binding；
2. 收到 ActPlane 返回的 `enforced` 确认；
3. 尝试读取受保护文件并收到 `EPERM`；
4. 查询到规范化 violation，其中包含 binding、session、rule、target、拦截结果和
   ActPlane revision；
5. detach binding 并收到 HTTP 204；
6. 对无效策略 DSL 和过期进程身份返回可由客户端处理的 HTTP 错误。

## 上游退出条件

将兼容行为及其回归测试提交到 ActPlane。上游 release 提供可选择的最小 hook
profile 和幂等的缺失 map 删除后，更新固定 revision，执行完整升级测试矩阵，并删除
本地补丁和 Cargo source override。
