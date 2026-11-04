# 示例策略

每个 `.cedar` 文件是一份独立的规则集，也可以按需要拼成同一份 policy set：
permit 叠加授权，forbid 优先覆盖，不需要额外写 deny 兜底。

这些文件全部写在官方校验接受的子集里：裸 `principal`、无注解、规范绝对路径。
示例中的 `/work/...`、`/usr/bin/git` 都是占位值，实际使用时必须换成任务里的真实路径。

## 每个示例怎么触发

下面每条的"触发句"是用户会说的话，"产出"是本 Skill 该给出的规则。可以把触发句直接发给
Agent 验收：如果它给出的规则和"产出"对不上，就说明 Step 1 没问清或 Step 2 写歪了。

### `readonly-review.cedar` —— 只读审查

- 触发句："让 Agent 帮我做代码审查，只能看代码，不许改文件、不许跑命令、不许联网。"
- 产出：1 条 `file.read` permit + 3 条 forbid（`file.write` / `file.exec` / `network.use`），
  是完整 policy set，可整份提交。
- 仍被拒绝：一切写、执行、联网。

### `allow-read-subtree.cedar` —— 只开一个目录的读取

- 触发句："只在 `/work/project` 里读代码，别的地方都不让读。"
- 产出：1 条 permit，条件为 `resource.path == "/work/project" || like "/work/project/*"`。
- 关键点：子树必须两条，`like` 不匹配目录自身。
- 仍被拒绝：该目录之外的读取（默认拒绝，不需要额外 forbid）。

### `forbid-delete.cedar` —— 能写但不能删

- 触发句："Agent 可以改文件，但绝对不能删任何东西。"
- 产出：1 条 `file.write` forbid，带 `context.input.operation == "delete"`。
- 关键点：operation 条件不能省，否则会禁掉全部写操作。
- 仍被拒绝：所有 `delete`（`create` / `rename` / `setattr` 不受影响）。

### `protect-file.cedar` —— 单文件不可写

- 触发句："目录里其他文件随便改，但 `/work/config/important.txt` 一个字都不能动。"
- 产出：1 条 `file.write` forbid，条件 `resource.path == "/work/config/important.txt"`，
  与一个子树写 permit 配合使用。
- 关键点：forbid 覆盖 permit，只影响这个路径。
- 仍被拒绝：该文件的一切写操作。

### `allow-program-exec.cedar` —— 只许一个程序在一个目录执行

- 触发句："只允许在 `/work` 里跑 `/usr/bin/git`，其他程序一律不许执行。"
- 产出：permit + 同动作补集 forbid，两条条件相同。
- 关键点：只写 permit 等于放开全部执行，补集 forbid 不能省。
- 仍被拒绝：其他程序，以及在其他 `cwd` 下执行的 `git`。

### `exclusive-write.cedar` —— 只许写一个文件的一种 operation

- 触发句："Agent 只能往 `/work/report` 里写内容，别的文件不许碰，也不能删或改名。"
- 产出：permit（`path == report` 且 `operation == "write"`）+ 补集 forbid（`operation == "write"`
  且 `path != report`）。
- 关键点：补集必须保留同一 operation 条件，否则会顺带禁掉本来允许的其他写。
- 仍被拒绝：其他路径的一切写；`/work/report` 上的 `delete` / `rename` / `setattr`。

### `deny-network.cedar` —— 关闭全部网络

- 触发句："这个任务不需要网络，把联网全部禁掉。"
- 产出：1 条 `network.use` forbid，`resource == SecCore::Network::"all"`。
- 关键点：只有全网络开关，host/port 级限制写不进来。
- 仍被拒绝：全部网络访问。

如果用户的触发句里带了 host、时间窗、身份这类信息，本 Skill 应当先在 Step 1 说明不可表达、
记进 unresolved，而不是硬套上面任何一个示例。

完整的不可表达清单见 [`../SKILL.md`](../SKILL.md) 的"无法表达的要求"：命中就记进 unresolved
并交回用户，不要自造字段或拿示例硬凑。
