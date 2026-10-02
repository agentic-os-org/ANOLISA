> **爬取时间**: 2026-06-17 16:29:23
> **原文链接**: https://help.aliyun.com/zh/alinux/how-to-use-ws-ckpt
> **文档更新**: 2026-06-15T09:40:24+08:00

---

ws-ckpt 是一个 AI Agent 工作区快照工具，为 AI Agent 提供毫秒级 checkpoint/rollback 能力。

## Harness Agent 使用

### **1\. 安装**

#### **1.1 检查是否安装 ws-ckpt CLI 工具**

```
ws-ckpt --version
```

如果没有安装，使用 yum 安装

```
sudo yum install ws-ckpt
```

#### **1.2 OpenClaw 安装 ws-ckpt 插件**

执行下面命令安装插件：

```
# 执行脚本
/usr/share/anolisa/adapters/ws-ckpt/scripts/install-openclaw.sh 
```

默认工作区路径 \`~/.openclaw/workspace\`，**建议使用默认工作区路径**，如需自定义，安装插件后执行下面命令：

```
# 修改 config 文件指定工作区
openclaw config set plugins.entries.ws-ckpt.config.workspace /path/to/workspace
```

**重要**

1.  工作区路径**不可以**是 Openclaw 启动路径或其父路径
    
2.  由于目前快照功能涉及文件系统变更，将影响工作区内进程cwd，**禁止**设置工作区路径为系统重要路径，如主目录，根目录等
    

注：openclaw gateway 启动路径通常是主目录，TUI 启动路径为 TUI 命令执行路径

如需打开自动快照功能，安装插件后执行下面命令，或用自然语言告知 OpenClaw，插件将在**每轮会话结束时**自动快照：

```
# 修改 config 文件开启自动快照
openclaw config set plugins.entries.ws-ckpt.config.autoCheckpoint true --strict-json
```

**说明**

由于 OpenClaw 修改配置文件会触发 gateway 重启，因此**自然语言配置**的工作区或自动快照均未持久化，**仅当前会话生效。**

#### **1.3 Hermes 安装 ws-ckpt 插件**

执行下面命令安装插件：

```
# 执行脚本
/usr/share/anolisa/adapters/ws-ckpt/scripts/install-hermes.sh 
```

必须主动配置工作区路径，安装插件后执行下面命令：

```
# 修改 config 文件指定工作区
hermes config set plugins.ws-ckpt.workspace /path/to/workspace
```

**重要**

1.  工作区路径**不可以**是 Hermes 启动路径或其父路径
    
2.  由于目前快照功能涉及文件系统变更，将影响工作区内进程cwd，**禁止**设置工作区路径为系统重要路径，如主目录，根目录等
    

注：hermes gateway 启动路径通常是 \`~/.hermes/hermes-agent\`，TUI 启动路径为 TUI 命令执行路径

如需打开自动快照功能，安装插件后执行下面命令，或用自然语言告知 Hermes，插件将在**每轮会话结束时**自动快照：

```
# 修改 config 文件开启自动快照
hermes config set plugins.ws-ckpt.autoCheckpoint true
```

#### **1.4 其他 Agent 可安装ws-ckpt skill**

##### **本地安装：**

-   Skill 文件路径：/usr/share/anolisa/runtime/skills/ws-ckpt/SKILL.md
    
-   对 agent 说：**“帮我安装 /usr/share/anolisa/runtime/skills/ws-ckpt/SKILL.md 这个 skill”**
    

##### **Github 源安装：**

-   Skill 文件 github 链接：[https://github.com/alibaba/anolisa/blob/main/src/ws-ckpt/src/skills/ws-ckpt/SKILL.md](https://github.com/alibaba/anolisa/blob/main/src/ws-ckpt/src/skills/ws-ckpt/SKILL.md)
    
-   对 agent 说：**“帮我安装**[**https://github.com/alibaba/anolisa/blob/main/src/ws-ckpt/src/skills/ws-ckpt/SKILL.md**](https://github.com/alibaba/anolisa/blob/main/src/ws-ckpt/src/skills/ws-ckpt/SKILL.md) **这个 skill”**
    

**说明**

Plugin 和 Skill 不可同时安装，二者互斥。

Skill 有模型理解有误风险，**优先推荐使用Plugin**

### **2\. 自然语言使用**

#### **2.1 指定工作区目录**

```
# 用户输入
配置快照工作区目录为/path/to/workspace
```

重要：openclaw 配置修改落盘将触发 gateway 重启，因此会话中仅修改内存，仅当前会话生效

#### **2.2 创建快照**

```
# 用户输入
创建一个快照，名字是test1，msg是测试快照
```

重要：必须给出名字，此后操作以名字标识快照，该名字需要具备唯一性

#### **2.3 开启自动快照**

```
# 用户输入
开启自动快照
```

重要：openclaw 配置修改落盘将触发 gateway 重启，因此会话中仅修改内存，**仅当前会话生效**

#### **2.4 查看快照**

```
# 用户输入
列出所有快照
```

#### **2.5 回滚快照**

```
# 用户输入
回滚到快照test1
```

重要：必须给出名字，定位快照

#### **2.6 删除快照**

```
# 用户输入
删除快照test1
```

重要：必须给出名字，定位快照

#### **2.7 开启自动清理快照**

```
# 用户输入
开启自动清理快照，仅保留最近7天的快照
```

**说明**

自动清理**一经配置，全局生效**：服务器上所有 workspace 都会基于该配置进行自动清理。

## CLI 使用

### 1\. 创建快照

```
ws-ckpt checkpoint -w <workspace> -i <id> [-m <message>] [--metadata <json>]
```

| 参数  | 简写  | 必填  | 说明  |
| --- | --- | --- | --- |
| `--workspace` | `-w` | 是   | 工作区路径或 ID |
| `--id` | `-i` | 是   | 快照id 唯一标识快照 |
| `--message` | `-m` | 否   | 快照描述信息 |
| `--metadata` |     | 否   | JSON 格式的附加元数据 |

**示例**：

```
# 基本用法
ws-ckpt checkpoint -w ./my-project -i test

# 带message
ws-ckpt checkpoint -w ./my-project -i test -m "initial state"

# 带元数据
ws-ckpt checkpoint -w ws-6d5aaa -i test --metadata '{"tool":"write","file":"main.py"}'
```

### 2\. 回滚到指定快照

```
ws-ckpt rollback -w <workspace> -s <snapshot>
```

`--snapshot`简写 `-s` 接受快照 ID（如 `test`）

`--workspace`简写 `-w`，工作区路径或 ID

| 参数  | 简写  | 必填  | 说明  |
| --- | --- | --- | --- |
| `--snapshot` | `-s` | 是   | 快照id 唯一标识快照 |
| `--workspace` | `-w` | 是   | 工作区路径或 ID |

**示例**：

```
# 按快照 ID 回滚
ws-ckpt rollback -w ./my-project -s test
```

### 3\. 列出快照

```
ws-ckpt list [-w <workspace>] [--format <table|json>]
```

| 参数  | 简写  | 必填  | 说明  |
| --- | --- | --- | --- |
| `--workspace` | `-w` | 否   | 省略 `-w` 时列出所有工作区的快照。 |
| `--format` |     | 否   | 输出格式，table 或 json |

**示例**：

```
# 列出所有工作区的快照
ws-ckpt list

# 列出指定工作区
ws-ckpt list -w ./my-project

# JSON 格式输出
ws-ckpt list -w workspace-6d5aaa --format json
```

### 4\. 删除指定快照

```
ws-ckpt delete [-w <workspace>] -s <snapshot> [--force]
```

| 参数  | 简写  | 必填  | 说明  |
| --- | --- | --- | --- |
| `--snapshot` | `-s` | 是   | 快照id 唯一标识快照 |
| `--workspace` | `-w` | 否   | 工作区路径或 ID，如果 snapshot id 全局唯一无需 `-w`参数，如果跨工作区id重复 必须指定工作区 |

**示例**：

```
# 删除单个快照
ws-ckpt delete -w ./my-project -s test

# 按快照 ID 全局删除（无需 -w，若 ID 全局唯一）
ws-ckpt delete -s test
```

### 5\. 查看状态

```
ws-ckpt status [-w <workspace>] [--format <table|json>]
```

| 参数  | 简写  | 必填  | 说明  |
| --- | --- | --- | --- |
| `--workspace` | `-w` | 否   | 省略 `-w` 时显示全局状态 |
| `--format` |     | 否   | 输出格式，table 或 json |

**示例**：

```
# 全局状态
ws-ckpt status

# 指定工作区
ws-ckpt status -w ./my-project
```

### 6\. 配置自动快照清理

```
ws-ckpt config 
      [--enable-auto-cleanup]
      [--disable-auto-cleanup]
      [--auto-cleanup-keep] <AUTO_CLEANUP_KEEP>
      [--auto-cleanup-interval] <AUTO_CLEANUP_INTERVAL>
```

| 参数  | 必填  | 说明  |
| --- | --- | --- |
| `--enable-auto-cleanup` | 否   | 开启自动快照清理 |
| `--disable-auto-cleanup` | 否   | 禁用自动快照清理 |
| `--auto-cleanup-keep` | 否   | 设置清理保留时间：整数（计数模式，0表示禁用）或过期时间，如“30d”（时效模式，单位为秒/分钟/小时/天/周） |
| `--auto-cleanup-interval` | 否   | 设置自动清理间隔（以秒为单位）（0表示禁用调度循环） |

**说明**

自动清理**一经配置，全局生效**：服务器上所有 workspace 都会基于该配置进行自动清理。

**示例**：

```
# 开启快照自动清理，保留最近10个快照
ws-ckpt config --enable-auto-cleanup --auto-cleanup-keep 10

# 开启快照自动清理，保留7天内的快照
ws-ckpt config --enable-auto-cleanup --auto-cleanup-keep 7d

# 禁用快照自动清理
ws-ckpt config --disable-auto-cleanup
```
