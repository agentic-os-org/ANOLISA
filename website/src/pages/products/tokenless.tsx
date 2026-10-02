import Layout from '@theme/Layout';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import CopyCommand from '../../components/CopyCommand';
import SiteLink from '../../components/SiteLink';
import type {Locale} from '../../../content.config';
import repoIndex from '../../../.generated/static/agents/repo-index.json';

const copy = {
  en: {
    eyebrow: 'PRODUCT / CONTEXT EFFICIENCY',
    title: 'Compress what enters the context window. Recover it when needed.',
    intro:
      'Tokenless compresses tool schemas and responses before they enter model context. Its reversible stash lets an agent retrieve dropped payloads by marker when full fidelity is needed.',
    copy: 'Copy', copied: 'Copied', docs: 'Read the Tokenless guide',
    start: 'Start the 3-minute Quick Start',
    quickTitle: 'Get your first verified saving',
    quickIntro:
      'Install Tokenless, connect one Agent adapter, then confirm a before/after record.',
    quickSteps: [
      ['01 · INSTALL', 'Install the published Tokenless component with the ANOLISA CLI.'],
      ['02 · CONNECT', 'Scan your machine and enable Tokenless for the Agent you use.'],
      ['03 · VERIFY', 'Run one tool-heavy task, then check tokenless stats list and tokenless stats summary.'],
    ],
  },
  zh: {
    eyebrow: '产品 / 上下文效率',
    title: '进入上下文前先压缩，需要时原样取回。',
    intro:
      'Tokenless 在工具 Schema 和响应进入模型上下文前完成压缩；需要完整信息时，Agent 可通过标记从可逆 Stash 中取回原始 Payload。',
    copy: '复制', copied: '已复制', docs: '阅读 Tokenless 指南',
    start: '开始三分钟快速体验',
    quickTitle: '完成第一次可验证的节省',
    quickIntro: '安装 Tokenless，接入一个 Agent Adapter，然后确认一条压缩前后的记录。',
    quickSteps: [
      ['01 · 安装', '通过 ANOLISA CLI 安装已发布的 Tokenless 组件。'],
      ['02 · 接入', '扫描当前机器，并为正在使用的 Agent 启用 Tokenless。'],
      ['03 · 验证', '运行一次工具密集型任务，再查看 tokenless stats list 和 tokenless stats summary。'],
    ],
  },
} as const;

type InstallTarget = {
  os: 'linux' | 'macos' | 'windows';
  architectures: string[];
};

function formatInstallTargets(targets: InstallTarget[]) {
  const operatingSystems = {
    linux: 'Linux',
    macos: 'macOS',
    windows: 'Windows',
  };
  return targets
    .map((target) => {
      const architectures = target.architectures.join(', ');
      return architectures
        ? `${operatingSystems[target.os]} (${architectures})`
        : operatingSystems[target.os];
    })
    .join(' · ');
}

export default function TokenlessProduct() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = copy[locale];
  const component = repoIndex.components.find((item) => item.id === 'tokenless');
  const version = component?.version;
  const installVariants = component?.install_variants ?? [];
  const preferredInstall = installVariants.find((variant) => variant.preferred);
  const platforms = preferredInstall
    ? formatInstallTargets(preferredInstall.platforms as InstallTarget[])
    : 'Unavailable';
  const features = locale === 'zh'
    ? [
        ['Schema 压缩', '减少 Function Calling 工具定义中的结构与描述开销。'],
        ['响应压缩', '针对 API 与工具输出移除冗余；压缩率是 Payload 指标，不等同于整场会话节省率。'],
        ['可逆 Stash', '截断内容以 <<tokenless:HASH>> 标记保存，可通过 retrieve 恢复。'],
        ['TOON 编解码', '为结构化 JSON 提供更紧凑的上下文表示。'],
        ['环境检查', '在调用工具前检查依赖、配置、权限与网络条件。'],
        ['统计', '按操作与 Session 查看 Payload 压缩记录，并支持基线对比。'],
      ]
    : [
        ['Schema compression', 'Reduce structural and descriptive overhead in Function Calling definitions.'],
        ['Response compression', 'Remove redundancy from API and tool output; payload compression is not a whole-session savings rate.'],
        ['Reversible stash', 'Store truncated content behind <<tokenless:HASH>> markers and recover it with retrieve.'],
        ['TOON encoding', 'Represent structured JSON more compactly in model context.'],
        ['Environment checks', 'Check dependencies, configuration, permissions, and network readiness before tool use.'],
        ['Statistics', 'Inspect payload compression by operation and session, with baseline comparison support.'],
      ];

  return (
    <Layout title="Tokenless" description={t.intro}>
      <main>
        <header className="productHero productTokenless">
          <div className="siteContainer narrowContainer">
            <p className="eyebrow">{t.eyebrow}</p>
            <h1>{t.title}</h1>
            <p className="productIntro">{t.intro}</p>
            <div className="installVariants">
              {installVariants.map((variant) => (
                <div className="installVariant" key={`${variant.method}:${variant.command}`}>
                  <div className="installVariantHeader">
                    <strong>{variant.method === 'cli' ? 'ANOLISA CLI' : variant.method}</strong>
                    <span>{formatInstallTargets(variant.platforms as InstallTarget[])}</span>
                  </div>
                  <CopyCommand
                    command={variant.command}
                    label={t.copy}
                    copiedLabel={t.copied}
                  />
                </div>
              ))}
            </div>
            <div className="buttonRow">
              <SiteLink
                locale={locale}
                to="/docs/user-guide/token-saving/tokenless/quickstart"
                className="primaryButton">
                {t.start} →
              </SiteLink>
            </div>
          </div>
        </header>
        <section className="section siteContainer narrowContainer">
          <div className="productQuickPath">
            <div>
              <h2>{t.quickTitle}</h2>
              <p>{t.quickIntro}</p>
            </div>
            <div className="productQuickSteps">
              {t.quickSteps.map(([title, body]) => (
                <article key={title}>
                  <strong>{title}</strong>
                  <p>{body}</p>
                </article>
              ))}
            </div>
          </div>
          <div className="factStrip">
            <div><span>PLATFORM</span><strong>{platforms}</strong></div>
            <div><span>CLI VERSION</span><strong>{version ? `v${version}` : 'unversioned'}</strong></div>
            <div><span>LICENSE</span><strong>Apache-2.0</strong></div>
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
              ? `# 压缩工具 Schema
tokenless compress-schema --file tool.json

# 压缩工具响应
cat response.json | tokenless compress-response

# 按标记取回被截断的内容
tokenless retrieve <HASH>

# 查看压缩前后记录
tokenless stats list --limit 5

# 查看汇总节省效果
tokenless stats summary

# 检查工具运行环境
tokenless env-check --all`
              : `# Compress a tool schema
tokenless compress-schema --file tool.json

# Compress a tool response
cat response.json | tokenless compress-response

# Recover stashed content by marker
tokenless retrieve <HASH>

# Inspect before/after records
tokenless stats list --limit 5

# Review aggregate savings
tokenless stats summary

# Check tool environments
tokenless env-check --all`}</code></pre>
          </div>
          <SiteLink locale={locale} to="/docs/user-guide/token-saving/tokenless/user-manual" className="primaryButton">
            {t.docs}
          </SiteLink>
        </section>
      </main>
    </Layout>
  );
}
