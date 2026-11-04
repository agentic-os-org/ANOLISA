# Skills

[English](../../../en/cosh-ng/core/skills.md)

Skills 是可复用的操作指令，用于处理重复任务。添加 Skill 后，任务匹配时 Agent 会加载它。

## 在 cosh 中管理 Skills

```text
/skills
/skills detail <name>
/skills enable <name>
/skills disable <name>
```

名称冲突时使用 `detail` 查看最终采用的来源。被禁用的 Skill 不会提供给 Agent。

## Skill 的搜索位置

同名 Skill 按下面的顺序查找，先找到的版本生效：

1. `<workspace>/.copilot-shell/skills/`
2. `skills.custom_paths` 中的路径
3. `~/.copilot-shell/skills/`
4. 用户安装时选择的目录，然后是 `$XDG_DATA_HOME/cosh-ng/skills/`
   和 `$XDG_DATA_HOME/anolisa/skills/`
5. Extensions 提供的 Skill 目录
6. 系统安装时选择的额外目录，以及 `$XDG_DATA_DIRS` 下的 `cosh-ng/skills/`
   和 `anolisa/skills/` 目录
7. `skills.system_paths` 中的附加目录

安装记录中的系统目录若已在标准列表中，则保留原有顺序；只有额外安装路径优先于标准目录。
使用默认系统数据目录时，RPM 安装记录 `/usr/share/anolisa/skills/` 后，
`/usr/local/share/anolisa/skills/` 仍优先于它。

`anolisa install os-skills` 的 raw backend 在用户级安装时使用上述用户数据目录，
在系统级安装时使用 `/usr/local/share/anolisa/skills/`。
`XDG_DATA_HOME` 默认为 `~/.local/share`；`XDG_DATA_DIRS` 默认为
`/usr/local/share:/usr/share`。变量未设置或为空时使用默认值。
无效的 `XDG_DATA_HOME` 也会使用默认值；`XDG_DATA_DIRS` 中的无效项会被忽略。
路径必须为绝对路径，且不能包含 `.`、`..` 路径段。
同一优先级内，同名 Skill 采用第一个包含该名称的目录，列表与加载结果一致。

使用自定义系统前缀时，应运行安装在同一前缀下的 cosh。
例如，安装在 `/opt/x` 下的 cosh 会依次搜索
`/opt/x/usr/local/share/anolisa/skills/` 和
`/opt/x/usr/share/anolisa/skills/`，替代宿主机的系统目录；
用户和项目目录的优先级保持不变。源码安装也会使用所选的 `PREFIX`、`AWPRJ`
和 `SKILLDIR`。若需以系统优先级加载其他已安装的技能集合，可使用 `skills.system_paths`。

已有目录会被监视，文件变化后自动重新扫描。

## 创建 Skill

推荐使用 `<skill-name>/SKILL.md` 目录布局，也兼容 `<name>.md` 单文件。
配置路径还可以直接指向包含 `SKILL.md` 的单个 Skill 目录。

```markdown
---
name: service-health
description: Inspect a systemd service and summarize actionable evidence
allowedTools:
  - shell
---

# Service health

Inspect status and recent logs before proposing a change. Ask for approval
before restarting the service.
```

`name` 和 `description` 必填。`allowedTools` 可省略，可以使用 YAML 列表或逗号分隔字符串。

## 添加共享目录

使用 `skills.custom_paths` 搜索团队维护的目录，无需复制文件：

```toml
[skills]
custom_paths = ["~/team-skills", "/opt/company/skills"]
```

路径支持展开 `~`、`${VAR}` 和 `$VAR`。项目路径以 Core 启动时的工作空间为基准。

对于附加的已安装技能集合，在系统或用户配置中设置 `skills.system_paths`。
这些目录排在默认系统目录之后，不会覆盖用户或 Extension 提供的 Skill：

```toml
[skills]
system_paths = ["/usr/local/share/agent-sec-core/skills", "/usr/local/share/ws-ckpt/skills"]
```
