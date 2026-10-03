import Layout from '@theme/Layout';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import CopyCommand from '../../components/CopyCommand';
import SiteLink from '../../components/SiteLink';
import type {Locale} from '../../../content.config';

const copy = {
  en: {
    eyebrow: 'PRODUCT / AGENT TERMINAL',
    title: 'Bring code, tools, and system work into one agent-aware terminal.',
    intro:
      'Copilot Shell is ANOLISA’s interactive terminal assistant for code understanding, task automation, and system management. It connects tools, skills, hooks, MCP servers, and multiple model providers without hiding the shell.',
    copy: 'Copy', copied: 'Copied', docs: 'Open the Copilot Shell guide',
  },
  zh: {
    eyebrow: '产品 / AGENT 终端',
    title: '把代码、工具与系统工作带进同一个 Agent 终端。',
    intro:
      'Copilot Shell 是 ANOLISA 面向代码理解、任务自动化与系统管理的交互式终端助手。它连接工具、Skills、Hooks、MCP Server 和多个模型提供方，同时保留 Shell 的直接性。',
    copy: '复制', copied: '已复制', docs: '打开 Copilot Shell 指南',
  },
} as const;

export default function CopilotShellProduct() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = copy[locale];
  const features = locale === 'zh'
    ? [
        ['自然语言编码', '理解项目、修改代码、执行任务并解释结果。'],
        ['多工具编排', '统一文件、Shell、搜索、Web、LSP 与 MCP 工具。'],
        ['交互式 Shell', '通过 /bash 进入原生交互式 Shell，并支持 PTY 与 sudo。'],
        ['Skills 与 Hooks', '按 Project、User、Extension 和 Remote 层级发现能力，并在工具执行前拦截。'],
        ['多模型提供方', '支持 Aliyun、Qwen OAuth 和 OpenAI-compatible endpoint。'],
        ['可扩展工作流', '通过 MCP Server、自定义 Skills 与 Extension 增加能力。'],
      ]
    : [
        ['Natural-language coding', 'Understand projects, change code, run tasks, and explain results.'],
        ['Multi-tool orchestration', 'Unify file, shell, search, web, LSP, and MCP tools.'],
        ['Interactive shell', 'Enter a native shell with /bash, including PTY and sudo support.'],
        ['Skills and hooks', 'Discover layered capabilities and intercept tool calls before execution.'],
        ['Multiple providers', 'Use Aliyun, Qwen OAuth, or OpenAI-compatible endpoints.'],
        ['Extensible workflows', 'Add capabilities through MCP servers, custom skills, and extensions.'],
      ];

  return (
    <Layout title="Copilot Shell" description={t.intro}>
      <main>
        <header className="productHero productShell">
          <div className="siteContainer narrowContainer">
            <p className="eyebrow">{t.eyebrow}</p>
            <h1>{t.title}</h1>
            <p className="productIntro">{t.intro}</p>
            <CopyCommand command="anolisa install cosh" label={t.copy} copiedLabel={t.copied} />
          </div>
        </header>
        <section className="section siteContainer narrowContainer">
          <div className="factStrip">
            <div><span>PLATFORM</span><strong>Linux · macOS · Windows</strong></div>
            <div><span>RUNTIME</span><strong>Node.js ≥ 20</strong></div>
            <div><span>ALIASES</span><strong>cosh · co · copilot</strong></div>
          </div>
          <div className="featureList">
            {features.map(([title, body], index) => (
              <article key={title}>
                <span>{String(index + 1).padStart(2, '0')}</span>
                <div><h2>{title}</h2><p>{body}</p></div>
              </article>
            ))}
          </div>
          <div className="codeExample">
            <p>{locale === 'zh' ? '常用命令' : 'Common commands'}</p>
            <pre><code>{locale === 'zh'
              ? `# 启动交互式终端（别名：co、copilot）
cosh

# 执行单个任务后退出
cosh -p "解释这个目录的构建流程"

# 执行任务后保持交互
cosh -i "总结最近的 git 提交"`
              : `# Start the interactive terminal (aliases: co, copilot)
cosh

# Run a single task and exit
cosh -p "explain the build process of this directory"

# Run a task, then stay interactive
cosh -i "summarize the recent git commits"`}</code></pre>
          </div>
          <SiteLink locale={locale} to="/docs/user-guide/user-entrypoint/copilot-shell/quickstart" className="primaryButton">
            {t.docs}
          </SiteLink>
        </section>
      </main>
    </Layout>
  );
}
