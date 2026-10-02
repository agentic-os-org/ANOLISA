> **爬取时间**: 2026-06-17 16:29:15
> **原文链接**: https://help.aliyun.com/zh/alinux/agentic-os-getting-started
> **文档更新**: 2026-05-27T18:40:32+08:00

---

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）是基于 Alibaba Cloud Linux 打造的 Agent 优先操作系统。本文帮助你几分钟内完成从创建实例到体验自然语言交互的完整流程。

## **第一步：创建ECS**实例

1.  前往[实例创建页](https://ecs-buy.aliyun.com/)；
    
2.  系统镜像选择 **Alibaba Cloud Linux** ，在下拉菜单中选择：**Alibaba Cloud Linux 4 LTS 64位 Agentic 版**；
    
3.  需勾选绑定公网 IP (EIP 或公网带宽)
    
4.  为保证使用体验，建议实例内存大于 4 GiB。其他参数配置按照界面提示完成，参见[自定义购买实例](https://help.aliyun.com/zh/ecs/user-guide/create-an-instance-by-using-the-wizard)。
    

## **第二步：首次配置**

登录实例后，系统自动进入 Copilot Shell（cosh），首次使用需配置模型授权。推荐使用 Aliyun Authentication 以获得快速、免配置的使用体验。不同授权方式的区别与使用，请参见：[管理配置](https://help.aliyun.com/zh/alinux/manage-configurations)

![image](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/1348789771/p1073966.png)

## **第三步：开始使用**

配置完成后，即可在 cosh 中用自然语言与系统交互。

ANOLISA 内置丰富的 Alibaba Cloud Linux 系统级 Skills，涵盖系统运维、安全加固、故障诊断等场景。配合安全防护机制，你可以直接在 cosh 中用自然语言执行操作系统级复杂任务，例如内核升级、漏洞修复、性能调优等，无需记忆繁琐的命令参数。

### **示例 1：**查看系统的发行版信息，并总结硬件详情

输入：查看系统的发行版信息，并总结硬件详情

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/6029384771/p1063214.png)

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/6029384771/p1063216.png)

### **示例 2：一键安装 OpenClaw**

输入：帮我安装 OpenClaw 并配置钉钉

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/6029384771/p1063218.png)

![image.png](https://help-static-aliyun-doc.aliyuncs.com/assets/img/zh-CN/6029384771/p1063219.png)

**说明**

需要提前在[钉钉开发者后台](https://open-dev.dingtalk.com/)创建企业内部应用，并获取 AppKey 和 AppSecret，以及在[百炼控制台](https://bailian.console.aliyun.com/)获取 API Key，详见：[一句话部署 Openclaw/Claude Code](https://help.aliyun.com/zh/alinux/deploy-openclaw-claude-code-in-one-step)

### 切回 Bash

如需 cosh 中切换 Bash，手动执行命令：

```
/bash
```

在 Bash 中执行 exit 或按 Ctrl+D 即可返回 cosh，会话历史自动恢复。

## 常用命令速查

| **命令** | **功能** |
| --- | --- |
| !命令 | 快速执行 Shell 命令 |
| @文件路径 | 引用文件作为上下文 |
| /stats | 查看 Token 消耗 |
| /approval-mode yolo | 开启全自动模式（免确认） |
| /skills | 查看加载skill |
| /help | 查看帮助 |
| /quit | 退出 cosh |

## **技术支持**

使用过程中如有疑问，可加入技术支持钉钉群：90400034325，联系技术支持获取帮助。

*温馨提示：体验完成后，如不再使用 ECS 实例，建议及时释放以避免产生费用。操作路径：ECS 控制台 → 实例详情 → 释放设置 → 立即释放。*
