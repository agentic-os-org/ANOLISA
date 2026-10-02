> **爬取时间**: 2026-06-17 16:29:17
> **原文链接**: https://help.aliyun.com/zh/alinux/manage-configurations
> **文档更新**: 2026-06-10T09:44:33+08:00

---

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）通过分层配置体系管理认证、模型、工具等设置，支持项目级与用户级配置文件多层覆盖。本文介绍了多种认证方式的接入方法、模型切换操作，以及配置文件的层级优先级和核心配置项说明。

## 1 认证方式

ANOLISA 第一次启动需要通过认证连接大语言模型服务。后续可以用 `/auth`命令切换认证方式。

目前支持以下认证方式。

### 阿里云认证

在交互式界面选择「Aliyun Authentication」，可获得一定的免费额度。

-   在 ECS 环境中，终端显示授权 URL 和二维码。在浏览器中打开该 URL 或手机扫描二维码，完成阿里云账号登录并授权
    
-   在非 ECS 环境中，使用阿里云 AK/SK 认证
    
    -   **获取 AK/SK：**
        
        1.  登录阿里云控制台
            
        2.  在 [AccessKey](https://ram.console.aliyun.com/profile/access-keys) 管理页面创建 Key
            

通过阿里云认证的用户可享受免费调用额度，适合个人开发和测试使用。

> 注意：阿里云认证方式仅支持使用千问系列文本模型，不支持多模态模型。

### Custom Provider（OpenAI 兼容端点 API Key 认证）

在交互式界面选择「Custom Provider」，准备 API Key ，支持的 OpenAI 兼容端点包括：

-   预填 Base URL：各云厂商的模型服务（DashScope、DeepSeek、GLM、Kimi、Minmax等）
    
-   自定义 Base URL：本地部署的模型（如 vLLM、Ollama）、第三方 API 代理服务等。
    

![image](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/2785501871/p1080458.png)

### 认证管理

| **操作** | **命令** |
| --- | --- |
| 查看当前认证状态 | `/auth` |
| 切换认证方式 | `/auth`（在交互菜单中选择） |
| 登出  | `/auth logout` |

> **常见问题**：认证失败时，检查 API Key 是否正确（注意前后空格）、网络是否能访问对应的 API 端点。可 /bash 切换到 bash 命令行使用 `co --debug` 启动查看详细错误信息。Agentic OS 支持同时配置多种认证，会按优先级选择，也可通过 `/auth` 手动切换。

## 2 模型配置

### 切换模型

使用 `/model` 命令切换当前会话使用的模型：

```
> /model
# 弹出可用模型列表，选择目标模型
```

## 3 配置文件

### **交互式配置**

使用 `/setting`命令进入交互界面，可以修改大部分配置。

### 配置层级与优先级

Agentic OS 的 系统 Shell 入口 cosh 使用 JSON 格式的配置文件管理设置，优先级从高到低：

```
命令行参数 > 环境变量 > 项目配置 > 用户配置 > 默认值
```

### 配置文件位置

| **类型** | **路径** | **说明** |
| --- | --- | --- |
| 用户设置 | `~/.copilot/settings.json` | 个人全局配置 |
| 项目设置 | `.copilot/settings.json`（项目根目录） | 项目级配置，可团队共享 |

### 核心配置项

#### general — 通用设置

| **配置项** | **类型** | **默认值** | **说明** |
| --- | --- | --- | --- |
| `preferredEditor` | string | —   | 打开文件使用的编辑器 |
| `vimMode` | boolean | false | 启用 Vim 键绑定 |
| `enableAutoUpdate` | boolean | true | 自动检查更新 |
| `checkpointing.enabled` | boolean | false | 启用会话检查点 |
| `defaultFileEncoding` | string | "utf-8" | 文件编码 |

#### model — 模型设置

| **配置项** | **类型** | **默认值** | **说明** |
| --- | --- | --- | --- |
| `name` | string | —   | 使用的模型名称 |
| `maxSessionTurns` | number | \\-1 | 最大会话轮数 |
| `chatCompression.contextPercentageThreshold` | number | 0.7 | 上下文压缩阈值 |

#### tools — 工具设置

| **配置项** | **类型** | **默认值** | **说明** |
| --- | --- | --- | --- |
| `approvalMode` | string | "default" | 审批模式 |
| `sandbox` | boolean/string | —   | 沙箱环境 |
| `core` | array | —   | 工具白名单 |
| `exclude` | array | —   | 排除的工具 |
| `allowed` | array | —   | 免确认的工具 |

#### context — 上下文设置

| **配置项** | **类型** | **默认值** | **说明** |
| --- | --- | --- | --- |
| `fileName` | string/array | "COPILOT.md" | 上下文文件名 |
| `includeDirectories` | array | —   | 额外包含的目录 |
| `fileFiltering.respectGitIgnore` | boolean | true | 遵守 .gitignore |

#### security — 安全设置

| **配置项** | **类型** | **说明** |
| --- | --- | --- |
| `folderTrust.enabled` | boolean | 启用目录信任机制 |
| `auth.selectedType` | string | 当前认证方式 |
| `auth.enforcedType` | string | 强制认证方式 |
