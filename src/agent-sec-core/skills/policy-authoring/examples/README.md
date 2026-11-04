# 示例策略

每个 `.cedar` 文件是一份独立的规则集，也可以按需要拼成同一份 policy set：
permit 叠加授权，forbid 优先覆盖；排他要求需要同动作的补集 forbid。

这些文件全部写在官方校验接受的子集里：裸 `principal`、无注解、规范绝对路径。
示例中的 `/work/...`、`/usr/bin/git` 都是占位值，实际使用时必须换成任务里的真实路径。

本 Skill 只交付 Cedar 源码，不包装或提交。下游提交格式与校验边界见
[`语言参考`](../references/cedar-policy-language.md) 的"下游提交格式"。

下面的 Allow / Deny 只指 Cedar Authorizer 对构造请求的策略判定，不是操作系统实际读写或拦截结果。

## 每个示例怎么触发

下面每条的"触发句"是用户会说的话，"产出"是本 Skill 该给出的规则。可以把触发句直接发给
Agent 验收：如果它给出的规则和"产出"对不上，就说明 Step 1 没问清或 Step 2 写歪了。

### `readonly-review.cedar` —— 只读审查

- 触发句："让 Agent 帮我做代码审查，只能看代码，不许改文件、不许跑命令、不许联网。"
- 先问审查根目录及是否包含后代；用户确认 `/work/project` 自身及整棵子树后才使用本示例。
- 产出：1 条限定目录的 `file.read` permit + 4 条 forbid（目录外 read、所有 write / exec / network），
  是完整 Cedar policy set；下游提交时按参考说明包装。
- 仍被拒绝：范围外读取以及一切写、执行、联网；与全局 read permit 拼接仍拒绝范围外读取。
- 验收：读取 `/work/project`、`/work/project/src/main.rs` 为 Allow，
  `/home/user/.ssh/id_ed25519` 为 Deny。目录内按路径授权，不能自动区分代码与秘密。

### `allow-read-subtree.cedar` —— 只开一个目录的读取

- 触发句："只在 `/work/project` 里读代码，别的地方都不让读。"
- 产出：permit + 同动作 `unless` 补集 forbid，完整条件为
  `resource.path == "/work/project" || resource.path like "/work/project/*"`。
- 关键点：子树有两个 `||` 连接的路径子条件，`like` 不匹配目录自身；补集抵挡已有的宽 read permit。
- 仍被拒绝：目录外读取，单独使用或拼接全局 read permit 时都成立；未写的其他动作仍取决于最终集合。
- 验收：上述目录自身和后代为 Allow，`/etc/shadow` 与同前缀邻居 `/work/project-other` 为 Deny。

### `forbid-delete.cedar` —— 保留已有写许可但不能删

- 触发句："已有经过确认的写权限，保留这些权限，但禁止删除任何文件。"
- 产出：1 条 `file.write` forbid，带 `context.input.operation == SecCore::WriteOperation::"delete"`。
- 关键点：operation 条件不能省，否则会禁掉全部写操作；这是已有写许可的补充，不授予写权限。
- 仍被拒绝：所有 `delete`。其他 operation 是否允许取决于已有许可；单独使用本示例时正常 write 也 Deny。

### `protect-file.cedar` —— 单文件不可写

- 触发句："目录里其他文件随便改，但 `/work/config/important.txt` 一个字都不能动。"
- 产出：1 条 `file.write` forbid，条件 `resource.path == "/work/config/important.txt"`，
  与一个子树写 permit 配合使用。
- 关键点：forbid 覆盖 permit，只影响这个路径。
- 仍被拒绝：该文件的一切写操作。

### `allow-program-exec.cedar` —— 只许一个程序在一个目录执行

- 触发句："只允许在 `/work` 里跑 `/usr/bin/git`，其他程序一律不许执行。"
- 产出：permit + 同动作补集 forbid，两条条件相同。
- 关键点：只写 permit 不能收窄已有的宽执行许可，补集 forbid 不能省。
- 仍被拒绝：其他程序，以及在其他 `cwd` 下执行的 `git`。

### `exclusive-write.cedar` —— 只许写一个文件的一种 operation

- 触发句："Agent 只能往 `/work/report` 里写内容，别的文件不许碰，也不能删或改名。"
- 产出：permit（`resource.path == "/work/report"` 且 `operation == SecCore::WriteOperation::"write"`）
  + `unless { 完整许可条件 }` 补集 forbid。
- 关键点：补集必须连同路径和 operation 一起否定，拒绝其余全部写操作。
- 仍被拒绝：其他路径的全部写操作；`/work/report` 上的 create / delete / rename / setattr，
  单独使用或拼接全局 file.write permit 时都成立。
- 验收：`write /work/report` 为 Allow，`delete /etc/other`、`rename /work/report` 为 Deny。

### `deny-network.cedar` —— 关闭全部网络

- 触发句："这个任务不需要网络，把联网全部禁掉。"
- 产出：1 条 `network.use` forbid，`resource == SecCore::Network::"all"`。
- 关键点：只有全网络开关，host/port 级限制写不进来。
- 仍被拒绝：全部网络访问。

如果用户的触发句里带了 host、时间窗、身份这类信息，本 Skill 应当先在 Step 1 说明不可表达、
记进 unresolved，而不是硬套上面任何一个示例。

完整的不可表达清单见 [`../SKILL.md`](../SKILL.md) 的"无法表达的要求"：命中就记进 unresolved
并交回用户，不要自造字段或拿示例硬凑。
