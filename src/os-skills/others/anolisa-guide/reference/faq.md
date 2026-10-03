> **爬取时间**: 2026-06-17 16:29:26
> **原文链接**: https://help.aliyun.com/zh/alinux/faq
> **文档更新**: 2026-05-28T14:44:30+08:00

---

#### **什么是** **ANOLISA****？**

阿里云专门为 AI Agent 打造的操作系统，基于 Alinux4 构建，完全兼容 Alinux4 的所有能力，可以理解为 Alinux4 的 Agent 增强版。

官网产品介绍文档：[产品概览](https://help.aliyun.com/zh/alinux/agentic-os)。

#### **ANOLISA** **对于 Agent 都有哪些增强？**

ANOLISA 目前对于 Agent 的增强主要来源 5 个部分：

1.  内置 Skills 提升 Agent 工作效率，覆盖系统相关场景，后续会扩散到通用场景；
    
2.  Copilot Shell（cosh）替换传统命令行，通过自然语言交互，降低与 Agent 交互的难度；
    
3.  系统层的 Agent 安全增强：包含 Skills 签名防投毒、沙箱隔离与系统调用管控、安全基线加固等。
    
4.  AgentSight 可观测：零侵入业务逻辑的前提下，实现了对 Agent 运行全链路的细粒度数据采集与关联分析
    
5.  Token-Less ，面向 LLM 的 Token 优化工具包：通过模式压缩、响应压缩及命令重写三大核心策略，显著降低上下文窗口的 Token 消耗并提升运行效率。
    

#### **什么场景适合用****ANOLISA****？是否有最佳实践？**

所有用来运行 Agent 的业务都适合选择 Agentic OS，其他业务请选择 Alibaba Cloud Linux 3/4 基础版。

[一句话部署 Openclaw/Claude Code](https://help.aliyun.com/zh/alinux/deploy-openclaw-claude-code-in-one-step)

#### **ANOLISA** **收费吗？**

镜像本身免费。但使用过程中的资源消耗正常计费，包括 ECS 实例（vCPU、内存、存储、公网带宽、快照）和大模型调用费用。

#### **怎么创建** **ANOLISA** **实例？**

ECS 创建页选择系统镜像为 **Alibaba Cloud Linux 4 LTS 64 位 Agentic 版**，**需绑定公网 IP，建议内存 ≥ 2GB**，仅支持 x86 架构。

快速入门：[快速入门](https://help.aliyun.com/zh/alinux/agentic-os-getting-started)

使用手册：[如何使用Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）](https://help.aliyun.com/zh/alinux/how-to-use-alibaba-cloud-linux-4-agentic-edition)

#### **大模型调用怎么计费？**

取决于你选择的认证方式。使用阿里云认证有一定免费额度；使用自己的 API Key 则按对应服务商的定价计费。

-   **API Key**：支持百炼 / OpenAI 兼容端点，依赖各模型提供商的定价。
    
-   **阿里云认证**：用阿里云认证（ECS 角色或 AK/SK），免费（仅支持千问系列文本模型）。不保证速度和成功率，仅支持体验使用。
    

#### **是否只支持配置阿里云的大模型？如果三种模式都配置了哪种生效？**

不是，OpenAI 兼容的模型厂商都支持。三种模式以最后配置的模式为准，开始对话以及结束对话会有使用模型的说明。

#### **认证失败怎么排查？**

检查 API Key 是否正确（注意前后空格）、网络是否能访问 API 端点。也可以 `/bash` 切到 bash 后用 `co --debug` 查看详细错误。

#### **cosh 是什么？**

ANOLISA 的默认命令行入口，替代了传统 bash，可以用自然语言与OS交互。

#### **cosh 怎么切换到传统 bash？**

在 cosh中输入 /bash 即可切换。

#### **在 bash 中怎么切换到 cosh？**

在 bash 中执行 exit 命令或 Ctrl+D 切换回 cosh。

#### **多用户登录是否共用配置？新建用户后配置是否独立？**

是的，当前系统多用户登录不会共享配置，具有独立的模型配置和 API Key。

#### **是否支持中文语言设置？**

支持，可在 Copilot Shell 中设置语言为中文。

#### **内置技能能否被 OpenClaw 等 Agent 使用？**

可以，安装 OpenClaw 后，系统内置技能会自动加载至其技能库中，实现兼容。

#### **ANOLISA** **是否已经开源？**

ANOLISA 代码已开源，开源地址：[https://github.com/alibaba/anolisa](https://github.com/alibaba/anolisa)。

#### **如何获取技术支持？**

技术支持钉钉群：90400034325
