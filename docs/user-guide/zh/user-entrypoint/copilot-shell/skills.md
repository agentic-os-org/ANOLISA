# 技能系统

技能（Skills）让 Copilot Shell 能执行特定领域的任务，如系统诊断、
安全审计、项目初始化等。技能以 Markdown 文件形式定义，支持本地加载，
并可通过 Clawhub 获取社区技能。

## 查看可用技能

```
/skills
```

列出当前会话中所有已发现的技能。

## 技能发现优先级

Copilot Shell 按以下优先级搜索技能（高优先级覆盖低优先级的同名技能）：

1. **项目级技能**：`.copilot-shell/skills/`
2. **自定义路径**：通过 `skills.customPaths` 配置的目录
3. **用户级技能**：`~/.copilot-shell/skills/`
4. **扩展技能**：扩展目录下的 `skills/`
5. **系统级技能**：`/usr/share/anolisa/skills`

## 技能结构

每个技能是一个目录，包含一个 `SKILL.md` 文件：

```
~/.copilot-shell/skills/
└── my-skill/
    └── SKILL.md
```

`SKILL.md` 是一个 Markdown 文件，描述技能的名称、触发条件、
执行步骤等信息。AI 会根据这些指令完成任务。

## 自定义技能路径

通过配置添加额外的技能搜索目录：

```json
{
  "skills": {
    "customPaths": [
      "~/my-skills",
      "/opt/team-skills"
    ]
  }
}
```

路径支持 `~`（主目录）和 `$VAR`/`${VAR}`（环境变量）展开。

## Clawhub 远程技能

Clawhub 是 Copilot Shell 的远程技能注册表，提供社区共享的技能。

### 搜索技能

```
/clawhub search <keyword>
```

### 安装技能

```
/clawhub install <skill-name>
```

### 更新技能

```
/clawhub update <skill-name>
```

### 配置注册表地址

```json
{
  "clawhub": {
    "registry": "https://cn.clawhub-mirror.com"
  }
}
```

## OS 技能

ANOLISA 预置了一组操作系统技能（`os-skills`），覆盖：

- **系统管理**：用户管理、服务管理、网络配置
- **监控性能**：系统资源分析、性能诊断
- **安全**：安全审计、漏洞扫描
- **DevOps**：CI/CD、容器管理
- **AI**：AI Agent 部署

这些技能随 ANOLISA 安装后自动可用。
