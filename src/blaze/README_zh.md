# ANOLISA Blaze

[English](README.md)

面向 AI Agent 工作负载的单机 sandbox 编排 daemon。

Blaze 通过 HTTP API 管理 sandbox 实例的完整生命周期，支持策略驱动的后端选择。
它提供多后端回退（Firecracker → Bubblewrap → Mock）和 Prometheus 指标导出，
设计为 E2B 类编排平台的单机执行代理。

## 特性

- **HTTP API** — Unix domain socket (`/run/blaze/api.sock`) + TCP (`:14159`)
- **策略驱动后端选择** — workload class → 后端优先级列表
- **生命周期状态机** — 持久化状态，并支持重启恢复，共 13 种状态：Pending、
  Creating、Running、Paused、Checkpointed、Restoring、Hibernating、
  Hibernated、Resuming、RecoveryRequired、Reset、Warm 和 Destroyed
- **检查点捕获** — 对支持该能力的后端和存储提供程序捕获完整 VM 状态、客体
  内存和可写根文件系统，并提供历史查询
- **休眠与恢复** — 在发布经过校验的休眠镜像后释放运行中的后端，之后再恢复
  运行，跨 daemon 重启也可以恢复
- **Guest 操作** — 对提供 guest endpoint 的运行中后端执行有界命令和文件传输
- **Template catalog** — 有界导入并原子发布可复用 artifact
- **内核 hook 注册** — 前/后置 hook 状态追踪
- **Prometheus 指标** — 请求和实例计数
- **Spawner 后端** — FirecrackerSpawner、BubblewrapSpawner、MockSpawner
- **可选 VM 网络** — 每台 Firecracker VM 独立使用 netns、tap、veth 和 NAT

## 快速开始

```bash
# 构建
cd src/blaze
cargo build --release

# 运行 daemon（开发环境：覆盖 policy.dir 使用本地示例）
sudo ./target/release/blazed daemon start --config examples/config.toml
# 注意：默认配置设置 policy.dir = /etc/anolisa/blaze/policies。
# 源码开发测试时，创建符号链接或覆盖：
#   sudo mkdir -p /etc/anolisa/blaze
#   sudo ln -s $(pwd)/examples/policies /etc/anolisa/blaze/policies

# 健康检查
curl --unix-socket /run/blaze/api.sock http://localhost/v1/health

# 创建 sandbox
curl -X POST --unix-socket /run/blaze/api.sock http://localhost/v1/sandboxes \
  -H 'Content-Type: application/json' \
  -d '{"workload_class":"agent-tool","image_digest":"sha256:..."}'
```

快速开始使用关闭 Firecracker guest transport 的示例策略，因此没有兼容
guest agent 的镜像不会等待 guest 就绪。只有镜像运行了对应 agent 时才应
启用该 transport。

## 配置

daemon 读取 TOML 配置文件（默认：`/etc/anolisa/blaze/config.toml`）
以及包含按 workload class 划分的策略文件的策略目录。

```
/etc/anolisa/blaze/
├── config.toml
└── policies/
    ├── agent-rl.toml
    └── agent-tool.toml
```

参见 `src/blaze/examples/` 获取带注释的示例配置。

### VM 资源配置

Blaze 使用三层回退链解析 vCPU 和内存设置：

1. **后端特定**（`[backend.firecracker].vcpus` / `.memory`）— 最高优先级
2. **策略级**（`[vm].vcpus` / `[vm].memory`）— 跨后端共享
3. **代码默认值**（1 vCPU, 256 MiB）— 未指定时的兜底

策略文件示例：

```toml
[vm]
vcpus = 2
memory = "512Mi"

[backend.firecracker]
vcpus = 4        # 仅对 Firecracker 覆盖 [vm].vcpus
memory = "1Gi"   # 仅对 Firecracker 覆盖 [vm].memory
enable_network = false
```

设置 `enable_network = true` 后，每台 Firecracker VM 会获得独立的网络
slot。显式销毁 sandbox 和启动失败补偿会在进程确认终止后删除对应的 netns、
tap 和 veth。daemon 重启后再次销毁时可以根据记录恢复清理，但不会在后台
自动扫描。slot 创建和删除使用主机级锁，避免多个 daemon 同时分配相同的主机
设备名。加载的 Firecracker 策略启用该选项时，backend probe 还会检查所需
命令和主机权限；网络关闭时跳过这些检查。上游路由和 DNS 仍由主机运维方
配置。

### 存储配置

`[storage]` 部分控制 sandbox 存储后端：

```toml
[storage]
provider = "file"       # 存储 provider 选择。当前支持："file"、"auto"。
                        # "auto" 按优先级探测可用 provider（当前等同于 "file"）。
                        # 其他值将记录告警并回退到 file。
images_dir = "/var/lib/blaze/images"
sync_interval = "disabled" # 设置正数 duration 后持久化 slot 中已经写入的制品。
sync_timeout = "30s"       # scheduler 等待 slot 重建与制品同步的最长时间。
```

Blaze 当前不支持可复用实例设置。`storage.pool_size` 和 `storage.prefork`
始终会导致配置校验失败；除历史软件包的精确默认值外，任何 `[pool]` 配置段
也会失败。软件包升级时有一项临时例外：旧版 `config.toml`、`agent-rl.toml` 和
`agent-tool.toml` 原样附带的 `[pool]` 默认值会被接受并忽略，同时记录警告。
这样，RPM 通过 `%config(noreplace)` 保留的管理员自定义文件不会阻止新版服务
启动，但也不会启用尚未完整实现的功能。管理员应合并对应的 `.rpmnew` 文件，
或删除旧 `[pool]` 配置段；后续版本可能取消这项兼容。其他策略 `[pool]` 配置
会导致策略加载失败。启动时，`policy.on_load_error = "fail"` 会让守护进程停止，
`"warn"` 则会使用空策略集继续启动。通过管理接口或信号重新加载策略失败时，
当前生效的策略保持不变。

`file` provider 使用标准文件系统操作管理 sandbox 存储。`auto` 按优先级探测可用 provider（当前等同于 `file`）。无法识别的值将记录告警并回退到 `file`。
启用周期同步后，已经返回的 provider 失败不会中断后续 sandbox。如果 provider
在 deadline 到达时仍无法停止文件系统操作，该操作会继续持有 sandbox operation
lock 和唯一的同步许可直至完成；后续同步会被推迟而不会不断累积。service loop
结束时，worker 会停止调度新任务。

[存储制品同步用户指南](../../docs/user-guide/zh/runtime/blaze.md#存储制品同步)进一步说明
配置、选择、重试和 worker 关闭行为。

## API 端点

Blaze 通过 `/v1/sandboxes` 提供沙箱生命周期和客户机操作。

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| GET | `/v1/health` | 健康检查 |
| GET | `/v1/sandboxes` | 列出所有 sandbox |
| POST | `/v1/sandboxes` | 创建 sandbox |
| GET | `/v1/sandboxes/{id}` | 获取 sandbox 详情 |
| DELETE | `/v1/sandboxes/{id}` | 销毁 sandbox |
| POST | `/v1/sandboxes/{id}/exec` | 执行 guest 命令 |
| POST | `/v1/sandboxes/{id}/read` | 读取 guest 文件 |
| POST | `/v1/sandboxes/{id}/write` | 替换 guest 文件 |
| POST | `/v1/sandboxes/{id}/checkpoint` | 捕获完整检查点 |
| GET | `/v1/sandboxes/{id}/checkpoints` | 列出已提交的检查点历史 |
| POST | `/v1/sandboxes/{id}/checkpoints/prune` | 删除运行中 sandbox 已经无法到达的分支；完整历史校验可能产生大量存储读取，其他状态返回 `409` |
| POST | `/v1/sandboxes/{id}/rollback/{checkpoint_id}` | 使用经过校验的检查点替换正在运行的 sandbox |
| POST | `/v1/sandboxes/{id}/hibernate` | 持久化 VM 状态并释放正在运行的后端 |
| POST | `/v1/sandboxes/{id}/resume` | 恢复休眠的 sandbox，并等待已启用的 guest 通信就绪 |
| GET | `/v1/pools` | 预留接口；返回 `501` |
| GET | `/v1/pools/{backend}/{class}` | 预留接口；返回 `501` |
| POST | `/v1/pools/{backend}/{class}/drain` | 预留接口；返回 `501` |
| PUT | `/v1/pools/{backend}/{class}/sizing` | 预留接口；返回 `501` |
| GET | `/v1/templates` | 列出已发布 template 的名称 |
| GET | `/v1/templates/{name}` | 查看已发布 template 的 metadata |
| POST | `/v1/templates/import` | 从配置的导入根目录发布 template |
| GET | `/v1/policies` | 列出已加载策略 |
| GET | `/v1/hooks` | 列出内核 hook |
| GET | `/v1/metrics` | Prometheus 指标 |
| POST | `/v1/admin/reload` | 热加载策略 |

升级兼容仅接受并忽略以下内容完全一致的 daemon `[pool]` 配置段：

```toml
[pool]
default_warm_ttl = "30m"
gc_interval = "5m"
```

可以接受的策略配置必须恰好包含六个字段，并且属于以下两个软件包内置策略之一：

| 策略名称 | 工作负载类型 | `min` | `target` | `max` |
|---|---|---:|---:|---:|
| `agent-rl-default` | `agent-rl` | 4 | 16 | 64 |
| `agent-tool-default` | `agent-tool` | 2 | 8 | 32 |

两行都要求 `enabled = true`、`warm_ttl = "30m"` 和
`reset_mode = "full-recreate"`。缺少或增加字段、改变值或类型、策略名称或工作
负载类型不同、任何其他 `[pool]` 配置，以及所有 `storage.pool_size` 或
`storage.prefork` 设置都会被拒绝。接受这些值不会启用实例复用；序列化配置时也会
省略这些值。

Blaze 仍可读取旧版本写入的 `Reset`、`Warm` 和 `start_path = "warm"` 持久化
值。启动恢复会把包含这些值的未终止记录作为清理对象，且不会复用这些记录。
清理失败时，内存记录会保留为 `RecoveryRequired`，并尝试持久化该状态。如果
持久化也失败，启动警告会记录附加错误，磁盘上的记录可能仍是先前状态。其他已通过
校验的记录仍会继续恢复。

`/v1/templates` 是唯一面向运维人员的 template catalog。create 请求通过
`POST /v1/sandboxes` 上可选的 `template` 字段从同一个 catalog 解析条目并恢复。
配置方法、接受的 artifact、上限和发布规则参见
[Template catalog 用户指南](../../docs/user-guide/zh/runtime/blaze.md#template-catalog)。

### 生命周期管理与恢复

创建和销毁会在修改存储或后端资源之前记录当前操作。创建成功后状态为
`Running`，销毁成功后状态为 `Destroyed`。如果失败补偿不能释放全部已有
资源，sandbox 会保留为可查询的 `RecoveryRequired`，后续可以再次执行销毁。

daemon 启动时，Blaze 会先校验完整的生命周期清单。只有清单完整且一致时，
daemon 才会逐个处理未结束的 sandbox。后续逐项恢复期间，如果单个 sandbox
清理失败，该 sandbox 会保留为 `RecoveryRequired`，但不会阻止其他已通过校验的
记录继续处理，也不会阻止 API 启动。

已经完成休眠的 sandbox 不在这轮清理范围内，会保留下来等待以后恢复；中断的
休眠或恢复操作会以 `RecoveryRequired` 保留，等待显式销毁，而不会被误认为
仍在运行。

正常关闭时，daemon 会先停止接收新请求，再关停其后台 worker。此时不会拆除仍在
运行的后端：它们的持久化记录留待下一次 daemon 启动时校验，届时已完成的休眠仍可
恢复，被中断的操作则以 `RecoveryRequired` 保留、等待显式处理。

清单校验采用 fail-closed 策略。如果 UUID 所属条目不是规范命名的目录，
`state.json` 缺失、不可读、是符号链接或目录、存在其他
硬链接，记录内的 sandbox ID 与目录名不同，或者 `Destroyed` 记录仍保留活动操作
或可能仍存活的后端所有权，daemon 都会在打开 API 监听器前停止。Blaze 不会自动
修复或删除这些记录。接受这份清单前，Blaze 还会确认每个 UUID 名称和其中的
`state.json` 仍然指向刚才读取的对象；具体流程是先完成第二次规范 UUID 名称
枚举并比较完整集合，再逐一复验保留的目录和记录。如果第二次枚举发现条目新增
或删除，或者后续对象检查发现保留对象消失或被替换，daemon 会停止启动。这一
一致性合同面向 Blaze 状态写入者：生产 store 持有 state root advisory lock，扫描
也会持有进程内 ownership map 锁直至发布。绕过 state root 锁直接修改文件的外部
进程不在支持范围内。

写入协调、清单发布、重置拒绝、旧状态清理和失败边界参见
[生命周期状态一致性与兼容性设计](docs/design/lifecycle-state-consistency_zh.md)。

操作记录会保存创建和销毁操作，以及检查点捕获已经完成的持久化阶段。中断的
创建会被清理而不是从原位置继续，重启后也不会接管先前的后端进程。启动恢复会
销毁捕获中断的 sandbox，而不是从其检查点恢复。恢复失败后目前没有后台循环自动
重试。重置接口仍不可用，也不会恢复检查点。

### 检查点捕获、历史与恢复

当运行中的 sandbox 所使用的后端和存储提供程序都声明支持完整捕获时，
`POST /v1/sandboxes/{id}/checkpoint` 会创建检查点。请求成功时，Blaze 会暂停后端，
捕获 VM 状态、客体内存和可写根文件系统，发布包含完整性信息的自包含清单，更新
该 sandbox 的检查点 HEAD，然后恢复后端。响应包含完整清单，以及
`checkpoint_id` 和 `instance_id` 字段。后端或存储组合不支持该能力时，接口会在
改变 sandbox 状态前返回 HTTP 501。

`GET /v1/sandboxes/{id}/checkpoints` 返回已提交检查点的历史摘要，包括父检查点、
逻辑大小、是否为当前 HEAD，以及能否从 HEAD 到达。

`POST /v1/sandboxes/{id}/checkpoints/prune` 删除无法从当前 HEAD 到达的历史分支。
该接口没有请求体字段：当前 HEAD 及其全部祖先始终保留，其他已提交分支都可以被删除。
为与 Go Blaze 保持一致，服务端不会读取或解析请求体；不发送请求体、发送 `{}`、已经
删除的旧字段或非 JSON 内容都可以调用，且这些内容会被忽略。响应包含 `status`、
`removed_count` 和被删除的检查点标识符。只有处于运行状态且没有未结束操作的 sandbox
才能执行清理；其他生命周期状态返回 HTTP 409。

Blaze 会在改变检查点目录前记录清理操作，并先把每个候选检查点原子改名为唯一的
清理标记，再递归删除版本 2 的载荷目录。只有本次请求创建的清理标记全部删除，并且
检查点目录同步成功后，接口才返回 HTTP 200。清理部分完成或结果无法确认时，sandbox
会进入 `RecoveryRequired`；再次请求清理会返回 HTTP 409，且不会继续修改检查点目录。
运维人员应销毁受影响的 sandbox，或者在 daemon 重启后让正常的启动恢复完成清理；不要
重试清理，也不要把错误文字中的检查点编号当成完整、可信的删除结果。销毁或启动恢复会
清理该 sandbox 持有的运行资源和检查点目录。

Blaze 不会把无法读取或无法校验的检查点目录当成空历史；检查点目录已有内容却缺少
HEAD 文件时也不会开始删除。清理操作读取检查点条目前，Blaze 会检查顶层目录的全部
内容；这里只允许可选的 HEAD 文件和名称规范的已提交检查点目录。未知文件、未知目录、
暂存内容或清理残留都会在删除开始前阻止清理。Blaze 在选择删除项之前会核对每个
已提交检查点的完整文件集合、记录大小和 SHA-256 摘要，还会校验全部分支，确认每个
父检查点都存在且父子关系中没有环。如果目录内容、检查点记录、父子关系或制品完整性
在第一次改名前校验失败，清理接口会返回 HTTP 500、清除本次操作记录，并保持 HEAD
和全部检查点目录不变。运维人员应排查存储损坏，而不是反复调用清理接口。这项预检会
读取全部已保存制品，因此清理耗时和存储读取量会随检查点历史的总大小增长。

`POST /v1/sandboxes/{id}/rollback/{checkpoint_id}` 用于把一个正在运行的
sandbox 回退到它此前捕获的某个检查点：丢弃当前的运行状态，改用该检查点保存
的那一份状态重新运行。只有当前使用的存储提供程序，以及捕获该检查点的后端，
都支持恢复能力时，这个接口才可用；否则 Blaze 不改动 sandbox 的任何状态，
直接返回 HTTP 501。

在真正改动运行状态之前，Blaze 会先做一整轮校验：确认所选检查点存在、它一直
回溯到最初检查点的整条父链完整、检查点记录的运行环境标识与当前一致，并逐个
核对所有制品文件的哈希。任意一项不通过都会中止，sandbox 保持原样。

恢复过程刻意遵循“先备好新状态、再切换、最后清理旧状态”的顺序，以免中途失败
损坏 sandbox。具体来说，旧后端还在运行时，Blaze 会先在旁边准备好一份独立的
根文件系统；只有等旧后端完全停止，才改用这份新的根文件系统启动并接管新的
后端，把检查点历史的当前指针（HEAD）指向所选检查点，最后才释放旧的根文件
系统。这里的分界点是 Blaze 是否已经开始停止旧后端：如果失败发生在这之前，
也就是仍处于校验和准备新根文件系统的阶段，旧后端一直照常运行，原来的运行
实例不受影响，相当于这次恢复没有发生。一旦 Blaze 开始停止旧后端，此后的
任何失败——包括停止操作本身失败、无法确认旧后端是否真的已经停止——都可能
留下清理不彻底的资源；此时 Blaze 会保留磁盘上确实存在的那部分资源，并把
sandbox 标记为 `RecoveryRequired`（需要恢复）状态，这样之后调用销毁接口时
仍能找到并清理这些残留资源。

`last_checkpoint` 字段始终指向最近一次成功捕获的检查点。回退只移动检查点
历史的当前指针，不会改写或删除已经捕获的历史记录。

响应字段、受支持的能力组合和失败处理方式参见
[检查点捕获、清理与恢复用户指南](../../docs/user-guide/zh/runtime/blaze.md#检查点捕获历史与恢复)。

### 休眠与恢复

休眠用于在暂时不需要一个 sandbox 时释放它占用的后端运行资源，同时把运行
状态完整保存下来，之后再原样恢复运行。只有当运行中的后端支持完整快照捕获，
并且配置的适配器能够恢复相同的后端版本时，休眠才可用；这些兼容性检查都在
改动生命周期记录之前完成，因此不满足条件的组合会让 sandbox 保持运行。至于
如何把工作负载带到一致的停止点，则交给后端的“捕获前静止（quiesce）”钩子：
其默认行为是暂停后端，而自冻结后端（捕获原语自身会停止工作负载的后端）会
覆盖该钩子，因而无需单独支持暂停。一次成功的休眠依次完成四件事：

1. 记录操作意图、为捕获而静止后端，并把后端载荷和内存写入隐藏的暂存目录；
2. 刷新保留下来的存储空间，并在清单中记录各文件的大小和 SHA-256 摘要；
3. 在停止后端之前，先把整份休眠镜像同步落盘；
4. 发布休眠目录，并提交 `Hibernated`（已休眠）状态。

后端被停止之前失败会让 sandbox 保持 `Running`，但有一个例外：若持久化休眠意图
时跨越了不确定的耐久性边界（状态改名成功但其目录同步失败），或此后暂存镜像
失败，则持久化记录可能与存活的运行实例不一致，此时 sandbox 会保留为
`RecoveryRequired`。

恢复的顺序同样是先校验、再切换。Blaze 会先核对清单记录的身份、文件集合是否
完全一致以及每个文件的摘要，确认无误后才启动替代后端。manager 先取得这个新
后端的归属，再等待可选的 guest 通信就绪，只有最后一次存活检查也通过，才提交
`Running`。如果在替代后端启动之前失败，sandbox 会回到 `Hibernated`，可以重试；
如果无法确认替代后端的清理是否彻底，其归属和操作记录会通过 `RecoveryRequired`
保留下来，交由显式销毁收尾。

休眠期间存储空间会继续保留，不会被回收。恢复成功后，最近一次的休眠镜像也会
保留，直到下一次休眠覆盖它，或者显式销毁把它删除——这是用磁盘空间换取可重复
恢复能力的取舍。daemon 重启后不会自动续做中断的休眠或恢复操作。

状态码约定、制品校验和失败归属参见
[休眠与恢复运行用户指南](../../docs/user-guide/zh/runtime/blaze.md#休眠与恢复运行)。

### Guest 操作

当 backend 提供兼容的 guest endpoint 时，运行中的 sandbox 可以执行有界
命令和文件传输。生产环境的 mock fallback 不会声明该能力。请求格式、上限、
就绪检查、错误处理和当前关闭边界参见
[Blaze 用户指南](../../docs/user-guide/zh/runtime/blaze.md#guest-操作)。

#### 健康检查

`GET /v1/health` 返回 daemon 状态，包含存储容量信息：

```json
{
  "status": "ok",
  "version": "0.3.0",
  "storage_pool": { "ready": 0, "capacity": 0, "pending": 0, "quarantined": 0 }
}
```

## 项目结构

```
src/blaze/
├── crates/
│   ├── blaze-core/   # 库：策略、生命周期、模板、内核、配置
│   └── blazed/       # 二进制：daemon、API server、spawner、指标
├── examples/         # config.toml、policies/
├── dist/             # blazed.service、blaze.spec、tmpfiles
└── manifests/        # 组件元数据
```

## 环境要求

- Rust 1.88+（参见 `src/blaze/rust-toolchain.toml`）
- 具有 root 权限的 Linux 主机（sandbox 后端需要）
- 启用 VM 网络时需要 `ip`、`iptables`、`sysctl` 和 netns 管理权限

## 许可证

Apache-2.0
