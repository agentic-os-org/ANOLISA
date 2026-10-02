> **爬取时间**: 2026-06-17 16:29:24
> **原文链接**: https://help.aliyun.com/zh/alinux/deploy-openclaw-claude-code-in-one-step
> **文档更新**: 2026-03-27T09:39:26+08:00

---

## **背景信息**

-   [OpenClaw](https://github.com/openclaw) 是一款开源 AI Agent 框架，支持通过插件机制接入钉钉等通信渠道和百炼 Qwen 等大语言模型。
    
-   [Claude Code](https://docs.anthropic.com/en/docs/claude-code) 是 Anthropic 推出的 AI 辅助编程工具，支持在终端中通过自然语言与代码交互。
    
-   这两个工具在 Agentic OS ECS 上的安装和配置都存在一定门槛：
    
    1.  OpenClaw 配置文件结构无公开文档、钉钉通道不在官方支持中、Gateway 初始化顺序要求严格.
        
    2.  Claude Code 官方教程针对 macOS/Ubuntu 编写，Alinux 存在兼容性差异、npm 权限问题
        
    3.  二者如何配置 DashScope API 均缺少文档。
        
-   通过 Skill 加持，Agent 可以一句话完成全链路部署，将原本无法自动化或需要多轮试错的流程缩短至几分钟。
    

## **案例一：一句话部署 OpenClaw**

### **前提条件**

-   已创建 Agentic OS ECS 实例（RAM 4 GB+），且实例已联网。
    
-   已在[钉钉开发者后台](https://open-dev.dingtalk.com/)创建企业内部应用，并获取 AppKey 和 AppSecret。
    

> **重要：**钉钉应用需在开发者后台开启**机器人能力**，消息接收模式选择 **Stream 模式**。同时需开启以下权限：`Card.Streaming.Write`、`Card.Instance.Write`、`qyapi_robot_sendmsg`

-   已在[百炼控制台](https://bailian.console.aliyun.com/cn-beijing?tab=model#/api-key)获取 DashScope API Key。
    
-   可开启YOLO模式避免手动确认 Shell 命令的执行（`/approval-mode yolo`）
    

### **操作步骤**

**步骤一：发起部署指令**

1.  在 Agentic OS 终端中，向 Agent 发送以下指令：
    

```
帮我安装 OpenClaw，AppKey是 <appkey>，AppSecret是 <appsecret>，API Key是 <dashscope apikey>
```

2.  Agent 将自动执行以下操作：
    
    -   检测并安装 Node.js 运行环境。
        
    -   通过 npm 安装 OpenClaw 以及钉钉插件，自动添加 npmmirror 镜像以加速国内网络下载。
        
    -   初始化 OpenClaw 项目结构和配置文件。
        
    -   配置 Gateway 模式（`gateway.mode`）。
        
    -   执行 `openclaw doctor --fix` 生成 auth token。
        

> **重要：**Gateway 初始化顺序至关重要：必须先写入 `gateway.mode` 配置，再执行 `openclaw doctor --fix`。Skill 已内置正确的执行顺序，无需手动干预。

**步骤二：验证部署结果**

1.  部署完成后，在钉钉中找到对应的机器人，发送一条测试消息。
    
2.  如果机器人正常回复，则表示全链路部署成功。
    

### **注意事项**

-   如果部署过程中出现网关启动失败，请检查 Gateway 初始化顺序是否正确。
    
-   如果钉钉机器人无响应，请确认应用权限（Card.Instance.Write、Card.Streaming.Write 等）是否已全部开启。
    

## **案例二：一句话部署 Claude Code**

### **前提条件**

-   已创建 Agentic OS ECS 实例（RAM 4 GB+），且实例已联网。
    

> **重要：**Claude Code 运行时内存占用较高，请确保 ECS 实例 RAM 不低于 4 GB。

-   已在[百炼控制台](https://bailian.console.aliyun.com/cn-beijing?tab=model#/api-key)获取 DashScope API Key（如需配置第三方 API）。
    
-   可开启YOLO模式避免手动确认 Shell 命令的执行（`/approval-mode yolo`）
    

### **操作步骤**

**步骤一：发起安装指令**

1.  在 Agentic OS 终端中，向 Agent 发送以下指令：
    

```
帮我装一个 Claude Code，配置Dashscope APIKey：<dashscope apikey>
```

2.  Agent 将自动检测当前系统环境，并按优先级选择最优安装方式：
    
    -   **原生安装**：优先尝试系统自带的包管理器安装。
        
    -   **npm 安装**：如原生方式不可用，通过 npm 全局安装，自动修复 EACCES 权限问题。
        
    -   **nvm 安装**：如 npm 方式失败，自动通过 nvm 安装独立的 Node.js 环境后再安装。
        

> **说明：**三种安装方式会自动降级，无需手动干预。Skill 会自动处理 Alinux 4 的兼容性问题，包括 EACCES 权限修复。

**步骤二：验证安装结果**

Agent 执行以下命令，确认 Claude Code 已正确安装：

```
claude --version
```

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/5655754771/p1062786.png)

**步骤三：配置 Dashscope APIKey**

1.  Agent 自动为Claude Code配置 Dashscope APIKey
    
2.  启动 Claude Code，测试与模型的交互是否正常。
    

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/5655754771/p1062297.png)

### **注意事项**

-   DashScope API Key 请提前在阿里云百炼控制台获取并开通相关服务。
    
-   如果所有安装方式均失败，请检查网络连通性和 Node.js 版本（建议 18+）。
