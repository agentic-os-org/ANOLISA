# Template Catalog

[English](template-catalog.md)

daemon 可以将一组可复用的 runtime artifact 发布到
`template.dir` 配置的目录。只有同时配置
`template.import_root` 后，导入功能才会启用。

`/v1/templates` 是 daemon 唯一公开的 template 资源。该 catalog 提供持久化
发布和查询，当 create 请求指定某个条目时，sandbox create 会从该条目恢复。

create 请求通过 `POST /v1/sandboxes` 上可选的 `template` 字段选择条目，并从
这个 catalog 解析该名称；不会再引入第二套 template registry 或另一个
template API namespace。

## 使用 catalog 条目创建 sandbox

create 请求通过把可选的 `template` 字段设为某个 catalog 名称，从已发布条目
恢复：

```json
{
  "workload_class": "agent-tool",
  "image_digest": "sha256:...",
  "template": "runtime-base"
}
```

该名称必须出现在所匹配 policy 的 `select.templates` 允许列表中，否则请求会在
写入任何生命周期状态之前被拒绝。随后 daemon 解析该条目，对每个 artifact 按
manifest 重新做摘要校验，并核对记录的镜像标识、backend、精确 backend 版本、
快照类型，以及（对 Firecracker）policy 将要启动的 guest 通信与 VM 规格。只有
这些校验全部通过后，才会发布 create intent。

因此已发布的 `template.json` 必须完整描述该条目：`format_version` 为 `1`、`name`
与 catalog 名称一致、非空的 `image_digest`、捕获时的 `backend`、`snapshot_kind`、
捕获时的 `expose_guest_socket` 与 `network` 开关、非零的 `rootfs_size` 与
`memory_size`，以及恰好三项 `artifacts`（`vmstate.snap`、`mem.bin`、
`rootfs.ext4`），每项带 `size_bytes` 和小写十六进制 `sha256`。记录的 rootfs 与内存
大小必须与对应 artifact 的大小一致。Firecracker 条目还必须提供
`resource_layout` 为 `portable-v1`、捕获时的内核 `boot_args`、非零的 `vcpus`
与 `memory_mib`，且 `memory_size` 等于 `memory_mib` 换算后的字节数。

`backend`、`backend_version` 和 `snapshot_kind` 会与所选 backend restore adapter
报告的值做相等比对，这一比对适用于所有 backend，而不只是 Firecracker。因此缺少
`backend_version` 的清单虽然能发布，却无法用于创建：内置 Mock adapter 报告
`mock-v1`，Firecracker 报告其精确二进制版本。两种拒绝的语义不同，因为发现阶段不
同——违反清单规则属于冲突，而 adapter 无法承载所记录的身份属于不支持的操作。

内置 Mock adapter 无法恢复 guest 通信或宿主网络，因此 Mock 条目必须把
`expose_guest_socket` 与 `network` 都记录为 `false`。任一不支持的形态都会在发布
create intent 之前被拒绝。

恢复适配器还必须区分“从模板恢复”和“回滚”。模板快照记录的是源沙箱标识，
但它可以用于创建多个新沙箱，所以恢复请求会标明快照来自其他沙箱。回滚请求
不设置该标记，因为它只能恢复当前沙箱自己捕获的检查点。如果某个后端在
快照中记录了沙箱标识，它只能在模板恢复时放宽这一项标识比较；格式、快照
类型、后端及后端版本检查仍须保持不变。

由于 Firecracker 快照会记录其根设备的宿主路径，而 `PUT /snapshot/load` 只覆盖网络
和 vsock 资源，因此每个 owner 都会把自己的 rootfs 绑定到一个稳定的命名空间内路径，
即记录在机器配置中的那个路径。`portable-v1` 就是这一布局的标记，也正是它让同一个
已发布条目能够恢复成多个彼此独立的 sandbox。
恢复同时会沿用快照中捕获的内核启动参数，因为该路径不会重新写入机器配置。
因此 template 预检要求 `boot_args` 必须与所匹配 policy 冷启动时的实际值完全一致；
启用网络时，该值包括 Blaze 自动追加的固定 `ip=` 参数。
启动前，daemon 会在固定 mount 目标不存在时创建普通文件；如果目标已经是目录、
符号链接或其他非普通对象，则拒绝启动。

物化会把 VM-state、内存和 rootfs 复制到一个全新的 provider 独占 slot，因此每个
template-backed sandbox 都拥有独立副本；修改或销毁其中一个，永远不会改变
catalog 或从同一条目创建的其他 sandbox。带网络的 template 会获得一次新的网络
分配，而不是继承源 sandbox 的 slot。sandbox 通过 backend restore 路径启动，其
catalog 名称会被持久化，因此普通的 checkpoint、rollback 和 delete
仍可正常工作。复制、恢复、就绪和终态失败沿用既有的可恢复 create 清理；
当回滚无法完成时，会保留残留存储供后续 destroy 使用。


## 导入请求

```http
POST /v1/templates/import
Content-Type: application/json

{
  "name": "runtime-base",
  "source": "runtime-base",
  "description": "base runtime"
}
```

`source` 是配置的导入根目录下的相对路径。绝对路径、父目录跳转和路径中的
符号链接都会被拒绝。每一级源目录和源文件都必须属于 daemon 用户，并且不能
允许 group 或其他用户写入。

源目录必须包含顶层普通文件 `vmstate.snap`、`mem.bin` 和 `rootfs.ext4`。
可选的 `template.json` 必须是 JSON object。导入只校验这一层结构，因此要供 create
选择的条目还必须带上
[使用 catalog 条目创建 sandbox](#使用-catalog-条目创建-sandbox) 一节描述的完整启动
清单；缺少清单的条目仍能发布，但会在 create 时被拒绝。嵌套目录、链接和特殊文件都会
被拒绝。daemon 会使用请求中的 `name`，采用请求中非空的 `description`，
并在 `rootfs_size` 或 `memory_size` 缺失或不是无符号整数时填入默认值。目标名称
已存在或同名导入正在执行时返回 `409 Conflict`。

## 上限和目录边界

以下配置会在发布前限制一次导入所做的工作：

| 配置 | 含义 |
|------|------|
| `max_files` | 单个发布条目的文件数上限，包括 `template.json` |
| `max_bytes` | 单个条目的 artifact 与生成后元数据的总字节数上限 |
| `max_metadata_bytes` | 输入与生成后元数据的大小上限 |
| `max_total_bytes` | 已发布字节与并发预留字节之和的上限 |
| `max_entries` | 已发布条目与并发导入预留条目之和的上限 |

`template.dir` 与 `template.import_root` 必须是绝对路径，
不能包含父目录组件，也不能互相重叠。它们还不能与存储镜像、存储实例、配置的
policy 目录、`[backends]` 中配置的任一 executable 路径、本次
启动打开 daemon 配置文件时捕获的解析位置、该文件的配置路径以及
`daemon.socket` 路径或宿主机网络协调路径
`/run/lock/blaze-network.lock` 重叠。宿主机上两种常见的命名网络空间目录
`/var/run/netns` 和 `/run/netns` 也属于受保护边界；每个 Firecracker owner 用作
bind-mount 目标的固定路径 `/run/blaze-snapshot-view/rootfs.ext4` 同样受保护。该
路径的字面值与其解析目标都会被保留，因此符号链接父目录也无法把该文件放进 catalog
根目录。
`[backends]` 中的相对路径会根据 daemon 启动时的工作目录解析一次，目录边界
检查、backend probe 和 launch 随后复用该绝对路径。配置 backend 符号链接时，链接
位置和解析目标都不能进入 template catalog ownership。daemon 配置路径为符号链接时，
配置的链接位置和已打开文件的解析位置也遵循相同规则。启动时会先解析路径中已经
存在的部分，再根据 mount table 比较其底层 Linux 文件系统位置；两个
template catalog 根目录中的符号链接组件也会被拒绝。检查期间会保留已打开的
配置文件，并在捕获的解析位置重复核对其身份，避免通过符号链接或 bind mount
别名绕过 ownership 边界或改变 catalog 初始化目标。发现重叠时，启动会在修改 catalog
权限或扫描已发布条目之前拒绝继续。
如果 catalog 根目录尚不存在，启动会保留路径中最深的现有父目录，并通过该目录的
描述符创建计划中的缺失路径段。计划创建的路径段若在边界检查期间出现，启动会拒绝
接管它，也不会修改新出现对象的权限。policy 条目边界检查遵循
`policy.on_load_error`：`warn` 模式下如果条目发现失败，则不加入条目目标，因为
policy 加载会使用空 engine；成功发现的所有目标仍受边界保护。启动还会从
`PATH` 解析 Blaze 使用的所有宿主机辅助程序候选，并保护每个程序的配置位置和
解析目标不被 catalog 接管。
启用导入后，daemon 会在启动时打开并保留已验证的 import root 目录；每次查找
源目录都从这个已保留的目录对象开始。因此，即使启动后替换配置路径，也不会把
导入重定向到另一个目录。
根目录可以使用 `daemon.state_dir` 下的非 UUID 子目录，包括默认的 catalog
目录，但不能占用 state 根目录，也不能进入某个 sandbox 的 UUID 子树。

catalog、staging 目录和已发布目录的权限为 `0700`，已发布文件的权限为
`0600`，并且只能有一个硬链接。catalog 条目和 staging 目录必须留在 catalog
根目录所在的挂载点；如果启动检查或 API 读取发现条目跨越嵌套挂载边界，处理会
立即停止。
启动检查和 API 读取会在取得可读句柄前先对 artifact 判型，并在读取前重新核对
已打开对象的身份。Linux 上先用仅供 metadata 操作的句柄固定判型对象，再从该对象
取得可读句柄，因此替换目录项不能重定向检查。

## 发布与恢复

导入器打开源条目时不会跟随链接；它会先预留 catalog 容量，再把文件复制到
私有且名称唯一的 staging 目录。复制完成后会再次检查源文件的身份和大小。
完整目录同步后以不覆盖已有条目的方式改名发布，因此读取方只会看到“没有条目”
或完整条目。

导入失败会移除对应的 staging 目录；这也包括目录已经创建、但尚未成功打开和
校验时发生的失败。如果清理无法完成，或者条目已经发布但 catalog 持久性无法
确认，daemon 会拒绝后续导入，直到修复 catalog 并重启。启动时，daemon 会先在
已打开的 catalog 根目录上取得并持续持有独占锁，然后才扫描条目或移除上次中断后
遗留且归自己所有的 staging 目录。使用同一 catalog 的第二个 daemon 会在干扰
正在进行的导入前失败。持锁的 daemon 还会校验已发布条目的类型、所有者、权限、
内容和容量。

正常关闭时，daemon 会拒绝新导入，请求取消正在执行的导入，等待相关文件句柄
和 staging 数据释放，然后从 service loop 返回。排空已经接收的连接以及释放
daemon 持有的全部 runtime 资源属于其他关闭职责。

## 查询与当前限制

已发布条目可通过以下接口查询：

- `GET /v1/templates`
- `GET /v1/templates/{name}`

集合接口只返回按名称排序的摘要。它每次完整校验一个条目，并在读取下一条前
释放该条目的完整元数据；单项接口返回完整元数据。集合响应同时受
`max_entries` 和模板名称 128 字节上限约束。任一时刻只允许一个列表响应处于
在途状态；在该响应 body 释放前，并发列表请求返回 `503 Service Unavailable`。
完整单项查询使用独立的 single-flight permit，同时覆盖 metadata 解析和返回的响应
body；在该 body 释放前，其他单项查询返回 `503 Service Unavailable`。已发布元数据
损坏时会返回错误，而不是静默隐藏条目。这些接口只管理已经保存的 artifact，校验
范围仅限结构，不证明快照能够启动或与某个 backend 兼容——只有当 create 请求
选择该条目时才会核对启动兼容性。catalog 尚未提供删除或引用跟踪。

上述 catalog 上限针对导入的 artifact 和 metadata。本改动没有增加 daemon
级 HTTP request body 上限；该输入边界需要在生产发布前单独补齐。
