# Blaze Firecracker 网络

[English](../../en/runtime/blaze.md)

Blaze 可以为每个 Firecracker sandbox 分配独立的 network namespace、tap
设备、veth pair 和地址 slot。该能力需要显式开启，默认保持关闭。

## 前置条件

Blaze daemon 必须运行在 Linux 上，并具有管理主机网络的权限。主机需要安装且
能够执行 `ip`、`sysctl` 和 `iptables`。此外还需要准备可用的 Firecracker、
guest kernel 和 root filesystem image。

当已加载的策略同时启用网络并将 Firecracker 作为候选 backend 时，Blaze 会
检查这些前置条件。网络保持关闭的策略不要求主机提供这些能力。

## 配置方法

在 workload 策略的 Firecracker 配置中设置 `enable_network`：

```toml
[select]
backend_priority = ["firecracker"]

[backend.firecracker]
enable_network = true
```

该选项仅作用于 Firecracker，默认值为 `false`。现有策略只有在显式开启后才会
改变原有行为。

## 运行行为

请求选中已启用网络的 Firecracker 策略后，sandbox 创建流程会：

1. 分配一个主机级 network slot；
2. 创建带有实例 owner 标识的 network namespace；
3. 创建 tap 和 veth 设备，并配置地址、转发和 namespace 内的 NAT；
4. 启动 Firecracker，并将 tap 设备连接到 VM。

分配与删除过程使用 `/run/lock/blaze-network.lock`，避免同一主机上的两个
Blaze daemon 进程同时选中相同 slot。Blaze 会在创建依赖设备前记录 namespace
的 owner，使未完全完成的网络配置仍能归属于对应 sandbox。

显式销毁 sandbox 时，Blaze 会先确认 backend 进程已经停止，再删除其拥有的
namespace 和设备。启动失败的补偿流程执行相同的清理。如果无法确认清理完成，
Blaze 会保留 ownership，不会将 slot 重新交给分配器，以便后续 destroy 请求
重试。

daemon 重启后，后续 destroy 请求可以根据已有记录重新识别 network slot。
Blaze 不会在后台扫描或自动重试孤立的网络资源。

开始任何启动恢复之前，Blaze 会完整读取已持久化的 sandbox 生命周期清单。每个
UUID 所属目录及其中的 `state.json` 都必须使用规范形式并且可以直接读取。接受
清单前，Blaze 会先完成第二次规范 UUID 名称枚举，并将完整集合与首次扫描结果
比较；随后再逐一确认保留的 owner 目录和 `state.json` 仍是先前读取的对象。如果
第二次枚举发现 UUID 缺失或新增、后续对象检查发现替换，或者 `Destroyed` 记录
未能证明清理完成，daemon 会在打开 API 监听器前停止，并保留原条目供运维人员
修复。

Blaze 只支持通过 `StateStore` 写入状态。生产 daemon 会一直持有 state root 的
排他 advisory lock，启动扫描也会持有进程内 ownership map 锁直至发布，因此遵守
这些锁的 Blaze 写入操作会被串行化。未参与 state root 锁、直接修改状态文件的
外部进程不属于这一致性合同的支持范围。

### 识别清单校验失败

Blaze 会在绑定 Unix listener 或可选 TCP listener 之前完成清单校验。如果校验
失败，daemon 会以非零状态退出，所有 API endpoint 都不可用；因此请求
`/v1/health` 时会连接失败，而不是收到表示降级状态的健康检查响应。

使用随软件包提供的 systemd service 时，可通过 `systemctl status blazed` 和
`journalctl -u blazed` 查看校验错误；与记录有关的错误会包含受影响的 sandbox
ID。Blaze 会保留被拒绝的记录。修复或恢复该记录后，重新启动 service，并确认
`/v1/health` 可以响应。

## 沙箱 API

Blaze 通过 `/v1/sandboxes` 提供沙箱生命周期和客户机操作。客户端使用该
命名空间列出、创建、查看和删除沙箱，以及在沙箱内执行命令、读取文件和写入
文件。销毁沙箱使用 `DELETE /v1/sandboxes/{id}`。检查点捕获与历史查询分别使用
`POST /v1/sandboxes/{id}/checkpoint` 和
`GET /v1/sandboxes/{id}/checkpoints`；无法到达的历史分支通过
`POST /v1/sandboxes/{id}/checkpoints/prune` 删除；恢复使用
`POST /v1/sandboxes/{id}/rollback/{checkpoint_id}`；休眠与恢复运行使用
`POST /v1/sandboxes/{id}/hibernate` 和 `POST /v1/sandboxes/{id}/resume`。

## 主机集成边界

Blaze 负责配置 sandbox 本地的网络路径。主机以外的路由和 DNS 仍由主机运维方
负责。生产环境开启该选项前，需要配置所需的上游路由或地址转换，并在目标主机
环境中验证 guest 连通性。

如需关闭该能力，将 `enable_network` 设置为 `false` 或删除该配置项，再通过
沙箱 API 销毁已经启用网络的 sandbox。

## Guest 操作

只有 sandbox 处于 `Running` 且 backend 报告兼容的 guest endpoint 时，
才能执行 guest 操作。冷启动 backend 如果报告了该 endpoint，创建流程会在
发布 `Running` 前等待 guest agent。没有 endpoint 的 backend（包括生产环境
mock fallback）会跳过等待，guest 操作返回 HTTP 409。

Guest 操作和 lifecycle 变更使用同一个 sandbox operation lock。取得锁后，
manager 会再次检查 `Running`，避免并发 lifecycle 变更后请求仍访问旧 runtime。

Sandbox 路由包括：

- `POST /v1/sandboxes/{id}/exec` — 执行一条命令；
- `POST /v1/sandboxes/{id}/read` — 读取一个文件；
- `POST /v1/sandboxes/{id}/write` — 替换一个文件。

Exec 请求格式如下：

```json
{"cmd":"uname -a","cwd":"/","env":{"LANG":"C"},"timeout":10}
```

Write 请求提供路径和 standard-base64 数据：

```json
{"path":"/tmp/input","data_b64":"aGVsbG8="}
```

Read 请求只提供 `path`。成功的文件读取结果和命令输出使用 standard base64。
Exec timeout 范围是 1 至 20 秒。Guest 路由会在读取过程中拒绝超过 22 MiB 的
HTTP envelope，文件数据解码后最多为 16 MiB。

Exec 或 write 在送达前失败时，可以由调用方决定重试；送达前超时使用
`"code": "guest_timeout"`。如果已经开始送达，但 daemon 无法确定结果，
返回 HTTP 504 和 `"code": "guest_outcome_unknown"`；此时应先核对 guest
状态，不能自动重放。Read 不改变 guest 状态。输入过大时返回 HTTP 413；
read 响应过大时返回 HTTP 502 和
`"code": "guest_response_too_large"`。

每个请求都会在单请求上限内完整缓冲。该上限不限制所有并发请求的总量，调用方
还需要控制 guest 操作并发数。当前不支持文件流式传输、交互式终端和会话复用。

可选 TCP listener 目前没有 daemon 级访问边界。在
[issue #2223](https://github.com/alibaba/anolisa/issues/2223) 解决前，生产配置应
保持 `listen.http_addr` 关闭。Daemon 停止时也不会等待全部 HTTP handler 或
释放所有 runtime owner，因此正在执行的请求可能看到连接关闭。

## 可复用实例管理

四个 `/v1/pools` 管理接口同样返回 HTTP 501。`storage.pool_size` 和
`storage.prefork` 始终会被拒绝；除历史软件包的精确默认值外，任何 `[pool]`
配置段也会失败。软件包升级时，只会临时接受并忽略旧版守护进程配置和两份默认策略
原样附带的 `[pool]` 默认值，同时记录警告。这项例外用于避免 RPM 通过
`%config(noreplace)` 保留的管理员自定义文件阻止新版服务启动，并不会启用
可复用实例。管理员应合并每个 `.rpmnew` 文件，或删除旧配置段；后续版本可能
取消这项兼容。其他策略 `[pool]` 配置会导致策略加载失败。启动时，
`policy.on_load_error = "fail"` 会让守护进程停止，`"warn"` 则会使用空策略集
继续启动。通过管理接口或信号重新加载策略失败时，当前生效的策略保持不变。

可以接受的 daemon `[pool]` 配置段必须恰好包含以下两个键值：

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
负载类型不同，或者出现任何其他 `[pool]` 配置，都会被拒绝。接受的兼容值会被
忽略，序列化配置时也会省略。

Blaze 仍可读取旧版本写入的 `Reset`、`Warm` 和 `start_path = "warm"` 持久化
值。启动恢复会把包含这些值的未终止记录作为清理对象，且不会复用这些记录。
清理失败时，内存记录会保留为 `RecoveryRequired`，并尝试持久化该状态。如果
持久化也失败，启动警告会记录附加错误，磁盘上的记录可能仍是先前状态。其他已通过
校验的记录仍会继续恢复。监控接口不再输出 `blaze_instances_resets_total`、
`blaze_pool_hits_total` 和 `blaze_pool_misses_total`。

这些兼容响应背后的生命周期约束记录在
[生命周期状态一致性与兼容性设计](../../../../src/blaze/docs/design/lifecycle-state-consistency_zh.md)
中。

## 检查点捕获、历史与恢复

Blaze 通过 `POST /v1/sandboxes/{id}/checkpoint` 捕获运行中的 sandbox。

所选后端和存储提供程序必须同时声明支持完整检查点捕获。内置文件存储提供程序
负责捕获可写根文件系统；Firecracker 通过自身的快照接口捕获客户机内存与设备
状态，内置 `mock` 后端提供完整的开发环境实现。当前版本的 Bubblewrap 和其他
进程后端尚未声明支持捕获。不支持的组合会在暂停 sandbox 或修改其生命周期记录
前返回 HTTP 501。

Firecracker 的检查点会记录当时运行的虚拟机监控器的确切版本，因为快照只能由
同一版本载回。只要还打算从某个检查点恢复，就需要保留其记录的那个版本：升级
Firecracker 可执行文件不会让已有检查点失效，但在该版本重新可用之前，这些检查
点无法恢复。

正因为这个记录下来的版本决定了检查点能否被恢复，如果监控器没有报告版本，捕获会在
暂停 sandbox 之前就拒绝该请求。因此不会产生缺少版本记录的检查点；读取清单时遇到
同样的形态也会被拒绝。

对于受支持且正在运行的 sandbox，Blaze 会持有该 sandbox 的操作锁，验证当前
检查点的父项，让后端进入静止状态，并以两棵由生产者各自持有的子树捕获载荷：
后端适配器在 `backend/` 下写入自己的私有布局（VM 后端在其中保存 VM 状态与
客户机内存），存储提供程序把可写根文件系统捕获为 `storage/rootfs.snap`。
随后 Blaze 清点每个捕获文件、同步并计算摘要、发布清单、原子更新该 sandbox 的
检查点 HEAD，再让工作负载恢复执行。捕获期间，对虚拟机内部执行的命令和文件
操作，以及其他生命周期变更都会等待同一把操作锁。

成功响应包含已发布的完整清单。现有 `checkpoint_id` 和 `instance_id` 字段与
`id` 和 `sandbox_id` 分别指向同一个检查点和 sandbox：

```json
{
  "checkpoint_id": "ckpt-11111111-1111-4111-8111-111111111111",
  "instance_id": "22222222-2222-4222-8222-222222222222",
  "format_version": 2,
  "id": "ckpt-11111111-1111-4111-8111-111111111111",
  "parent": null,
  "sandbox_id": "22222222-2222-4222-8222-222222222222",
  "policy_name": "agent-tool",
  "image_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
  "backend": "mock",
  "backend_version": "mock-v1",
  "created_at": "2026-08-14T00:00:00Z",
  "snapshot_kind": "full",
  "artifacts": [
    {
      "name": "backend/memory.snap",
      "size_bytes": 8192,
      "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    },
    {
      "name": "backend/vmstate.snap",
      "size_bytes": 4096,
      "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    },
    {
      "name": "storage/rootfs.snap",
      "size_bytes": 8589934592,
      "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    }
  ]
}
```

`artifacts` 清单按字典序列出每个捕获文件相对检查点目录的斜杠分隔路径。清单的
具体内容由产生该检查点的后端决定：上例为内置 mock 后端，容器形态的后端可能记录
整棵镜像目录。此前格式（`format_version: 1`）的检查点仍可恢复，但新捕获一律
发布版本 2。

可以通过 `GET /v1/sandboxes/{id}/checkpoints` 查询已提交的历史。每个列表项
包含 `id`、`parent`、`created_at`、总逻辑大小 `size_bytes`、`is_head` 和
`on_head_chain`。该列表只提供摘要，不会重复捕获响应中的完整制品清单。

能够确认发生在发布前的失败会删除临时数据、恢复后端，并让 sandbox 保持运行。
如果 Blaze 无法确认发布、HEAD 更新、持久化或后端恢复的结果，则会保留持久记录并
报告 `RecoveryRequired`；在 sandbox 完成恢复处理或销毁前，不应重试捕获。已经提交但
未成为 HEAD 的检查点仍可能出现在历史列表中，其 `is_head` 为 `false`。

可以使用以下接口删除已经无法到达的检查点分支：

```http
POST /v1/sandboxes/{id}/checkpoints/prune
```

该接口没有请求体字段。为与 Go Blaze 保持一致，服务端不会读取或解析请求体，因此
调用方可以不发送请求体，也可以发送 `{}`、其他字段或非 JSON 内容；服务端会忽略这些
内容。Blaze 会保留当前 HEAD 及其完整父链，并删除其他全部已提交分支。

成功响应会准确列出已经离开已提交历史的检查点：

```json
{
  "status": "pruned",
  "removed_count": 1,
  "removed": ["ckpt-44444444-4444-4444-8444-444444444444"]
}
```

服务端会忽略调用方提供的请求体，因此请求体不能指定要删除的检查点，也不能保留某个
无法到达的分支。只有处于 `Running` 且没有未结束操作的 sandbox 才能执行清理；休眠中、
需要恢复处理或其他不可用状态返回 HTTP 409。

捕获、查询、清理、客户机操作和生命周期变更使用同一把单 sandbox 操作锁。删除前，
Blaze 会先持久记录清理操作。随后每个候选检查点都会原子移动到唯一的
`.prune.<检查点>.<uuid>.tombstone` 目录，再连同后端的嵌套载荷递归删除。每次移动前
都会重新确认候选目录仍是原来的对象，且当前 HEAD 没有变化；HEAD 可达链不会成为
候选项。

如果失败能够确认发生在任何移动之前，Blaze 会清除操作记录，不删除检查点。清理
部分完成或移动结果无法确认时，sandbox 会进入 `RecoveryRequired` 并保留操作记录；
此时再次请求清理会返回 HTTP 409，且不会改变检查点目录。销毁操作或 daemon 重启后的
正常恢复会移除该 sandbox 持有的运行资源和完整检查点目录。运维人员应销毁受影响的
sandbox，或者在 daemon 重启后让正常的启动恢复完成清理；不要重试清理，也不要根据错误
文字中的检查点编号推断完整删除结果。只有本次请求创建的临时目录全部删除，并且检查点
目录同步成功后，接口才返回 HTTP 200。

Blaze 不会把无法读取或无法校验的检查点目录当成空历史；检查点目录已有内容却缺少
HEAD 文件时也不会开始删除。清理操作读取检查点条目前，Blaze 会检查顶层目录的全部
内容；这里只允许可选的 HEAD 文件和名称规范的已提交检查点目录。未知文件、未知目录、
暂存内容或清理残留都会在删除开始前阻止清理。Blaze 在选择删除项之前会核对每个
已提交检查点的完整文件集合、记录大小和 SHA-256 摘要，还会校验全部分支，确认每个
父检查点都存在且父子关系中没有环。如果目录内容、检查点记录、父子关系或制品完整性
在第一次改名前校验失败，清理接口会返回 HTTP 500、清除本次操作记录，并保持 HEAD
和全部检查点目录不变。运维人员应排查存储损坏，而不是反复调用清理接口。这项预检会
读取全部已保存制品，因此清理耗时和存储读取量会随检查点历史的总大小增长。

可以使用以下接口恢复正在运行的 sandbox：

```http
POST /v1/sandboxes/{id}/rollback/{checkpoint_id}
```

恢复要求目标是经过校验的完整检查点，且策略、镜像、后端和后端版本都与当前
sandbox 完全一致；后端适配器和存储提供程序还必须明确声明支持恢复。Firecracker
适配器、内置 mock 适配器与文件存储提供程序实现了这项约定。其他后端适配器在
实现恢复前会返回 HTTP 501，并且不会停止当前运行环境。

恢复 Firecracker sandbox 会用一个新进程替换原来的虚拟机监控器，由新进程载入
捕获下来的内存与设备状态，因此进程号会变，而 sandbox 的标识不变。新进程会按
检查点当时的宿主形态启动——同样的网络插槽、客户机通信通道和控制台记录设置——
因为快照是按名字引用这些设备的。恢复前记录的控制台输出和监控器诊断信息会保留，
不会被覆盖。

如果当前安装的 Firecracker 版本与检查点记录的版本不一致，恢复会在停止正在运行
的 sandbox 之前就被拒绝，因此版本不匹配不会造成任何损失。

`checkpoint_id` 不符合规范形式时返回 HTTP 400；符合规范但没有对应已提交检查点
时返回 HTTP 404。这两种结果都是终态：都不会改动正在运行的 sandbox，用同一个
标识符重试也不可能成功。

文件存储提供程序会在当前后端仍运行时准备目标根文件系统。随后 Blaze 停止旧后端、
启用暂存根文件系统、启动并检查替代后端、移动检查点 HEAD，最后提交存储变更。
判断边界是 Blaze 是否已经开始停止旧后端：在此之前失败（仍处于校验和准备根文件
系统的阶段）时，旧后端照常运行，sandbox 不受影响；一旦开始停止旧后端，此后
任何失败——包括停止操作本身失败或无法确认旧后端是否真正停止——都会让 Blaze
保留实际存在的资源并把 sandbox 标记为 `RecoveryRequired`，以便销毁操作完成
清理。恢复会移动检查点 HEAD，但不会改写 `last_checkpoint` 或捕获历史。

## 休眠与恢复运行

休眠解决的问题是：一个沙箱暂时不需要使用时，希望把它占用的宿主资源——后端
进程及其内存——先释放出来，但又不能丢掉客户机里已有的状态。它与销毁的区别是
沙箱的身份和存储都保留下来；与检查点捕获的区别是捕获之后后端继续运行，而休眠
之后后端会被停止。

```http
POST /v1/sandboxes/{id}/hibernate
POST /v1/sandboxes/{id}/resume
```

休眠要求沙箱处于运行中，其后端支持完整快照捕获，并且配置的适配器能够恢复
相同的后端版本。Blaze 会在改动生命周期记录之前完成这些兼容性检查，因此遇到
不支持的组合会返回 HTTP 501，沙箱仍照常运行。对状态不符合预期的沙箱发起
请求会返回 HTTP 409。把工作负载带到一致停止点的方式，交给后端的“捕获前
静止（quiesce）”钩子：其默认行为是暂停后端，而自冻结后端会覆盖该钩子，
因而无需单独支持暂停。

一次成功的休眠依次完成：记录操作意图、为捕获而静止后端、把后端载荷和客户机
内存写入私有暂存目录、刷新保留下来的存储空间、在清单中记录每个文件的大小与
SHA-256 摘要、把整份镜像同步落盘，最后才发布镜像并提交 `Hibernated` 状态。
发布后的镜像通过保留的沙箱目录描述符定位，因此即使实例目录被替换或被改成
符号链接，也无法把它指向别处。

恢复运行会先校验清单记录的身份、文件集合是否完全一致以及每个文件的摘要，确认
无误后才启动替代后端。Blaze 先取得该后端的归属，再等待可选的客户机通信就绪，
只有最后一次存活检查也通过，才提交 `Running`。文件损坏或缺失会在任何后端启动
之前就被拒绝。

失败处理沿用与恢复接口相同的“停止之前”边界：如果失败发生在 Blaze 开始停止
后端之前，原运行实例会被重新恢复，沙箱保持 `Running`——但有一个持久化例外：
若持久化休眠意图时跨越了不确定边界（状态改名成功但其目录同步失败），或此后
暂存镜像失败，则持久化记录可能已与存活的运行实例不一致，此时沙箱会保留为
`RecoveryRequired` 等待显式处理，而不再报告为 `Running`。恢复运行失败但残留
可以确认清理干净时，沙箱回到 `Hibernated`，可以重试；无法确认清理结果时，
替代后端的归属和操作记录会通过 `RecoveryRequired` 保留，等待显式销毁。

有两点持久化特性需要在容量规划时考虑。休眠期间存储空间会一直保留不被回收；
恢复成功后，最近一次的休眠镜像也会保留，直到下一次休眠覆盖它或销毁将其删除
——这是用磁盘空间换取可重复恢复的能力。daemon 重启后，已经完成的休眠会保留
下来以便继续恢复，但中断的休眠或恢复操作不会自动续做，而是以
`RecoveryRequired` 保留，等待显式销毁。

## 存储制品同步

Blaze 可以定期持久化 running sandbox 中已经写入的宿主机制品和目录元数据。
该 worker 默认关闭；只有配置同步周期后，现有部署的行为才会改变。

### 配置方法

在 daemon 配置中设置同步周期和单个 sandbox 的执行时限：

```toml
[storage]
sync_interval = "30s"
sync_timeout = "10s"
```

`sync_interval = "disabled"` 会关闭周期 worker。`sync_timeout` 限制
scheduler 等待单个完整 provider attempt 的时间，包括重建 storage slot 和
同步该 slot。

每次 storage-provider 同步调用会持久化本次调用可见、且已经写入的字节与目录
元数据。并发发生的制品更新可能在本次或后续 attempt 中变为可见。

### 运行行为

每轮 sweep 会选择处于 running 状态且仍持有完整 storage slot 的 sandbox。它会
对 operation lock 已被占用的 sandbox 直接推迟本轮处理，而不等待该 lock，使
sweep 可以继续处理后续 sandbox。Lifecycle 变更、guest 请求和存储制品同步共用这把
lock。取得可用 lock 后，worker 会在调用 storage provider 前再次检查 lifecycle
状态。如果取得 lock 后记录仍为 `Running`，但保留了未完成的 operation 或非
running 的 backend ownership，该记录属于不一致状态，会记为失败而不是推迟。
第一次 sweep 会在完整的配置周期过去后启动，而不是在 worker 启动时立即执行。
定时器错过的 tick 会被跳过而不是排队，避免慢速 sweep 累积任务。

已经返回的失败只影响对应 sandbox。Blaze 会保留 storage slot 的 ownership，
且不改变 lifecycle 状态，因此后续 sweep 或 destroy 仍可重试。如果文件系统
操作在 deadline 到达时无法停止，它会继续持有 sandbox operation lock 和唯一
的同步许可直至完成；后续 attempt 会被推迟，而不会累积更多 blocking 任务。
在此期间到达的 guest 和 lifecycle 操作会等待 provider 工作完成；
`sync_timeout` 只限制 scheduler 的等待时间，不限制这些操作的等待时间。

service loop 停止时，Blaze 会取消并等待周期 scheduler 退出。无法取消的
provider 工作会继续由对应 sandbox lock 持有直至完成；daemon 级连接排空和
runtime 清理仍属于独立职责。

## Template Catalog

Blaze 可以原子发布运维人员准备的 runtime artifact，并通过 daemon API 提供
其 metadata。`/v1/templates` 是唯一面向运维人员的 template 资源；
`POST /v1/sandboxes` 请求通过可选的 `template` 字段选择已发布条目，daemon
会从该条目恢复新的 sandbox。

sandbox create 会从同一个 catalog 解析可选的 template name；运维人员不需要
配置或监控另一套进程内 registry。所指定的条目必须出现在所匹配 policy 的
`select.templates` 允许列表中，且其记录的镜像、backend、版本，以及（对
Firecracker）VM 与 guest 通信规格必须与 policy 将要启动的一致。每个
template-backed sandbox 都会获得 artifact 的独立副本，因此可以像其他 sandbox
一样做 checkpoint、rollback 和 delete，而不会影响 catalog 或同源的
其他 sandbox。

### 配置方法

catalog 目录有默认值，但只有配置 import root 后才会启用导入：

```toml
[template]
dir = "/var/lib/blaze/templates"
import_root = "/var/lib/blaze/template-imports"
max_files = 32
max_bytes = 274877906944
max_metadata_bytes = 1048576
max_total_bytes = 1099511627776
max_entries = 128
```

两个根目录必须使用绝对路径，彼此不能重叠，也不能与 Blaze 的 image、instance、
policy 根目录、`[backends]` 中配置的任一 executable 路径、本次启动
打开 daemon 配置文件时捕获的解析位置、该文件的配置路径或配置的
`daemon.socket` 路径以及宿主机网络协调路径
`/run/lock/blaze-network.lock` 重叠，也不能与宿主机上两种常见的命名网络空间
目录 `/var/run/netns` 和 `/run/netns` 重叠，还不能与固定的 snapshot view rootfs
路径 `/run/blaze-snapshot-view/rootfs.ext4` 重叠。每个 Firecracker sandbox 都会把
该文件作为自身根文件系统的 bind-mount 目标，因此把 catalog 根目录配置在
`/run/blaze-snapshot-view`（或通过符号链接解析到该位置）会在启动时被拒绝，而不是
放任其中出现被 catalog 记账当成损坏条目的根级文件。
`[backends]` 中的相对路径会在启动时根据 daemon 的工作目录解析一次；目录边界
检查、backend probe 和 sandbox launch 随后复用该绝对路径。如果配置的 backend 路径
是符号链接，则该链接的配置位置及其解析目标都不能进入 template catalog ownership。
daemon 配置路径为符号链接时遵循相同规则：配置的链接位置与已打开文件的解析位置
都不能进入 template catalog ownership。
template catalog 根目录不能包含符号链接组件。在 Linux 上，Blaze 启动时会解析
路径中已经存在的部分，并根据 mount table 比较其底层文件系统位置，避免符号
链接或 bind mount 别名绕过目录边界。Blaze 会保留已打开的配置文件，并在捕获
的解析位置重复核对其身份，因此重定向配置路径不能换入另一个配置文件。发现
重叠时，启动会在修改 catalog
权限或扫描 catalog 条目之前拒绝继续。
template catalog 根目录可以像默认配置一样使用 `daemon.state_dir` 下的非 UUID
子目录，但不能接管 state root，也不能进入 sandbox UUID 子树。
如果 catalog 根目录尚不存在，Blaze 会保留路径中最深的现有父目录，并从该目录
创建缺失的路径段。如果计划创建的路径段在检查期间出现，启动会在修改该对象权限
之前停止。policy 条目边界检查遵循 `policy.on_load_error`：`warn` 模式下的条目
发现失败与 policy 加载一样使用空 policy engine；成功发现的 policy 目标仍受边界
保护。Blaze 通过 `PATH` 找到的宿主机辅助程序也受保护，检查同时覆盖程序的配置
位置和解析目标。
Blaze 会保留启动时打开并验证过的 import root 目录。之后替换配置路径不会改变
源目录查找的起点。

### 导入与查询

以下请求会发布 `import_root` 下的一个源目录：

```http
POST /v1/templates/import
Content-Type: application/json

{"name":"runtime-base","source":"runtime-base","description":"base runtime"}
```

`source` 必须是相对路径，不能跳转父目录或经过链接。源目录必须包含顶层普通文件
`vmstate.snap`、`mem.bin` 和 `rootfs.ext4`；可选的 `template.json` 必须是
JSON object。源目录和文件必须属于 daemon 用户，且不能允许 group 或其他用户
写入。嵌套目录、链接和特殊文件都会被拒绝。

如果条目要被 create 请求选择，`template.json` 必须包含完整的启动元数据。导入本身
只校验它是 JSON object，因此缺少这些元数据的条目仍能成功发布，但会在 create 时
返回 `409 Conflict`：

| 字段 | 含义 |
|------|------|
| `format_version` | 必须为 `1` |
| `name` | 必须与发布的 catalog 名称一致 |
| `image_digest` | 镜像标识，create 请求必须声明相同值 |
| `backend` | 捕获该快照的 backend |
| `backend_version` | 必须与该 backend restore adapter 报告的版本一致；内置 Mock backend 为 `mock-v1`，Firecracker 为捕获时的精确二进制版本 |
| `boot_args` | Firecracker 快照中捕获的内核启动参数，必须与所选 policy 冷启动时的实际参数完全一致；启用网络时包括 Blaze 自动追加的固定 `ip=` 参数 |
| `snapshot_kind` | 快照类型，当前为 `full` |
| `expose_guest_socket` | 捕获时是否暴露 guest 通信通道 |
| `network` | 捕获时是否持有宿主网络 slot |
| `vcpus` / `memory_mib` | Firecracker 快照中捕获的 VM 规格；两者必须非零，并与所选 policy 完全一致 |
| `rootfs_size` / `memory_size` | 字节大小，必须与 `rootfs.ext4`、`mem.bin` 一致 |
| `artifacts` | 恰好三项，对应 `vmstate.snap`、`mem.bin`、`rootfs.ext4`，每项含 `size_bytes` 和小写十六进制 `sha256` |

create 会把清单中的 `backend`、`backend_version` 和 `snapshot_kind` 与所选 backend
的 restore adapter 报告值逐项比对，不一致时返回 `501 Not Implemented`，即使该条目
本身已成功发布。具体状态码取决于问题在哪一步被发现：Firecracker 清单遗漏
`backend_version` 会先违反清单自身的可启动性规则，返回 `409 Conflict`；而 Mock
清单遗漏该字段能通过这些规则，最终由 adapter 比对返回 `501`。
例如，内置 Mock adapter 固定报告 `mock-v1`；清单填写 `mock-v2` 同样返回
`501`，表示应修正清单值，而不是改用其他 backend。

Firecracker 条目还必须提供 `resource_layout = "portable-v1"`、`boot_args`、
非零的 `vcpus` 与 `memory_mib`，且 `memory_size` 必须等于 `memory_mib`
换算成字节的值。这些同样属于清单可启动性校验，因此违反时返回
`409 Conflict`。policy 冷启动时实际生效的内核启动参数、VM 规格与 guest 通信设置
必须与这些值完全一致。启用网络时，实际启动参数包括 Blaze 自动追加的固定 `ip=`
参数。恢复使用快照中捕获的启动参数，不会根据当前 policy 重建。
如果 `vcpus`/`memory_mib` 缺失或为零，或者 VM 规格与 policy 不一致，会在
写入生命周期状态或分配存储之前返回 `409 Conflict`，因此不会留下残留的
sandbox 目录。

内置 Mock backend 不支持恢复 guest 通信或宿主网络，因此 Mock 条目的
`expose_guest_socket` 与 `network` 都必须为 `false`。请求任一不支持的资源会在写入
任何 sandbox 生命周期状态之前返回 `501 Not Implemented`。

从 template 创建 sandbox 与普通创建使用相同的可恢复清理机制：

- policy、镜像、backend、版本、VM 规格或 guest 通信校验失败时，尚未写入 create
  intent，也未分配存储。请求或清单冲突返回 `409 Conflict`；存储或恢复能力
  不支持时返回 `501 Not Implemented`，两者都不会留下 sandbox 独占存储。
- artifact 复制、backend 恢复、guest 就绪或最终状态持久化失败时，create
  intent 已经写入。Blaze 会先尝试停止 backend、释放存储，并把 sandbox 提交为
  `Destroyed`。如果补偿全部成功，返回原始错误，且不保留 sandbox 资源。
- 补偿未完成时返回 HTTP 500，错误文本以 `operation requires recovery`
  开头；错误中指明的 sandbox 会保持 `RecoveryRequired`，并可能仍持有存储或
  backend owner。后续发送 `DELETE /v1/sandboxes/{id}` 可重试清理。

```json
{
  "format_version": 1,
  "name": "runtime-base",
  "image_digest": "sha256:...",
  "backend": "firecracker",
  "backend_version": "Firecracker v1.16.0",
  "resource_layout": "portable-v1",
  "boot_args": "console=ttyS0 reboot=k panic=1 pci=off",
  "snapshot_kind": "full",
  "expose_guest_socket": false,
  "network": false,
  "vcpus": 1,
  "memory_mib": 256,
  "rootfs_size": 536870912,
  "memory_size": 268435456,
  "artifacts": [
    {"name": "vmstate.snap", "size_bytes": 14174, "sha256": "..."},
    {"name": "mem.bin", "size_bytes": 268435456, "sha256": "..."},
    {"name": "rootfs.ext4", "size_bytes": 536870912, "sha256": "..."}
  ]
}
```

create 请求选择条目时会按这些值重新校验每个 artifact 的摘要，因此摘要必须与已发布
文件完全对应。

已发布文件只能有一个硬链接，catalog 条目和 staging 目录也必须留在 catalog
根目录所在的挂载点。发现不满足这些边界的数据时，Blaze 会停止处理，不会修改或
继续遍历这些数据。
启动扫描或 list/get 读取 artifact 前，Blaze 会先在不取得可读句柄的情况下判型，
并在读取前重新核对对象身份。Linux 上的可读句柄来自已经固定的判型对象，因此替换
目录项不能把读取重定向到另一个对象。

`GET /v1/templates` 用于列出按名称排序的轻量摘要，
`GET /v1/templates/{name}` 用于读取一个条目的完整 metadata。列表读取会
逐条校验并释放完整 metadata，且任一时刻最多保留一个列表响应；在其 body 释放
前，并发列表请求返回 `503 Service Unavailable`。单项查询使用独立上限，任一时刻
最多保留一个完整单项响应；在该 body 释放前，其他单项查询返回
`503 Service Unavailable`。目标名称已存在或同名导入正在进行时返回
`409 Conflict`。

### 发布、上限与恢复

Blaze 在检查输入时执行单条目的文件数和字节数上限，并在复制到私有 staging
目录前预留 catalog 字节和一个 `max_entries` slot。复制后会再次检查源文件
身份，同步完整条目，再通过不覆盖现有目标的 rename 发布。因此读取方只会
看到“没有条目”或完整条目，名称摘要 list 响应也不会物化超过配置数量的条目。

导入失败时会删除 staging 数据；这也包括 staging 目录已创建、但后续打开或
校验失败的情况。如果无法确认清理完成或发布结果已持久化，后续导入会被拒绝，
直到修复 catalog 并重启 daemon。启动时会验证已发布条目，并删除中断导入遗留
且归 daemon 所有的 staging 目录。在执行扫描或清理前，daemon 会在已打开的
catalog 根目录上取得并持续持有独占锁；使用同一 catalog 的第二个 daemon 会在
检查或清理仍在使用的 staging 目录前直接失败。正常关闭时会拒绝新导入、取消
正在复制的任务，并等待相关文件句柄关闭。

API 只校验 artifact 结构，不证明 snapshot 能在特定 backend 上启动；只有当
create 请求选择该条目时才会核对启动兼容性。catalog 尚未提供删除或引用跟踪。
