# 自检清单

本 Skill 的校验路径：Agent 自己生成 Cedar，自己过这份清单，不调用任何命令。

清单覆盖语法、schema、写作约束与语义，属于必要条件检查，不是充分条件：全部通过也
不证明实际校验通过或语义正确，语义靠回译确认。

逐条过，命中就改到不命中为止。"检查性质"区分实际校验和写作/语义约束，后两类不代表入口会拒绝。

## 1. 结构

| 检查项 | 检查性质 |
|---|---|
| 每条规则以 `;` 结尾，`when` / `unless` 后的大括号正确配对 | 语法解析 |
| 效果只有 `permit` / `forbid` | 语法解析 |
| 作用域只有 `principal, action, resource`，没有 `context` | 语法解析 |
| 交付的文本至少一条静态规则；全部要求都不可表达时返回 unresolved 而不交付 policy，不交空文本 | 支持范围检查 |
| 没有注解：`@id`、`@advice`、`@description` 全部不写 | 支持范围检查 |
| 没有模板、没有 `?principal`、没有 `link` | 支持范围检查 |

## 2. 主体与资源

| 检查项 | 检查性质 |
|---|---|
| `principal` 裸写，不写 `principal == ...`、`principal is ...`、`principal in ...` | 支持范围检查 |
| 条件里不引用变量 `principal` | 支持范围检查 |
| 不出现 `SecCore::File::"..."`、`SecCore::Subject::"..."` 这类实体 UID | Skill 写作约束 |
| `file.read` / `file.write` / `file.exec` 的规则用 `resource is SecCore::File` | Skill 写作约束 |
| `network.use` 的规则用 `resource == SecCore::Network::"all"` | Skill 写作约束 |

## 3. 动作与字段

| 检查项 | 检查性质 |
|---|---|
| `action` 只取 `file.read`、`file.write`、`file.exec`、`network.use` | schema 校验 |
| `file.read` 只读 `context.input.program` | schema 校验 |
| `file.write` 只读 `context.input.program`、`operation`、`mode`、`ownerUid`、`ownerGid` | schema 校验 |
| `file.exec` 只读 `context.input.cwd`、`arg_count`、`args` | schema 校验 |
| `network.use` 的条件里不读任何 `context.input` 字段 | schema 校验 |
| 读 `mode` / `ownerUid` / `ownerGid` 前先写 `context.input has <字段>` | schema 校验 |
| `operation` 只与 `SecCore::WriteOperation::"write"`、`"create"`、`"delete"`、`"rename"`、`"setattr"` 比较 | schema 校验 |
| 不出现 schema 未声明的字段名 | schema 校验 |

## 4. 路径

| 检查项 | 检查性质 |
|---|---|
| `resource.path`、`context.input.program`、`context.input.cwd` 只与字面量比较，不与另一个属性比较 | Skill 写作约束 |
| 字面量是规范绝对路径：`/` 开头，无 `..`、无空段、无重复 `/`、无尾部 `/`、无真实反斜杠、无控制字符 | Skill 写作约束 |
| `like` 只有"字面量前缀 + 末尾 `/*`"，前缀以 `/` 结束 | Skill 写作约束 |
| 子树写成两个子条件：`== "/dir"` 或 `like "/dir/*"` | 语义 |
| 不假设请求路径已规范化：`..` / `.` 不折叠、符号链接不解析；物理边界未确认时列入 unresolved | 语义 |
| 等式中的真实星号原样写 `"/a*b"`，仅 `like` 模式写 `"/a\*b/*"`；等式 `"/a\*b"` 非法 | 语法解析 / Skill 写作约束 |
| 路径不是相对路径、不是变量拼接、不是运行时才能确定的值 | Skill 写作约束 |

## 5. 语义

| 检查项 | 检查性质 |
|---|---|
| "只允许 X"同时写了同动作的补集 `forbid`；用 `unless` 否定完整路径和 operation / 执行条件 | 语义 |
| 只读审查先确认根目录；目录外读取用补集 forbid 阻断，不给无条件全文件 read permit | 语义 |
| 排他读/写的回译在单独使用和拼接宽 permit 时都成立 | 语义 |
| 同动作多个允许范围已并成一条联合条件，只保留一组 permit + 补集 forbid，不叠加两组补集 | 语义 |
| 不可表达的限制若是许可的必要条件，则没有生成对应 permit；全部不可表达时只返回 unresolved | 语义 |
| 保留写权限、只封删除时，`forbid` 带 `operation == "delete"` 条件 | 语义 |
| 纯禁止不额外补 `permit`；依赖已有许可时写明前提，单独使用不会授予权限 | 语义 |
| 没有把"禁止删除"写成禁止全部写操作；未明确接受全部目的地就不生成全网络许可 | 语义 |
| 每条规则能在表格里说清"允许/拒绝谁在什么条件下做什么" | 自检 |

## 6. 交付标注

交付必须包含：

1. 一句话意图复述；
2. 假设（路径、`operation` 集合、子树还是单文件）；
3. **校验状态：已按清单自检**，列出过了哪几段（结构、主体与资源、动作与字段、路径、语义）；
4. Cedar 原文（本次不交付 policy 时说明原因）；
5. 每条规则的中文说明与仍然被拒绝的操作；
6. 无法表达的要求（unresolved）；
7. Agent 自身运行所需的权限（模型端点、凭据、工具）是否被这份 policy 或现有
   Binding 覆盖，没覆盖就说清缺哪一条，未确认或未明确授权的缺口列入 unresolved，不自动补权。

不能把自检写成"已校验"、"已验证"或"可以生效"；自检不证明实际校验通过或语义正确。
