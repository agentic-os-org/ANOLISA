import Head from '@docusaurus/Head';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import CopyCommand from '../components/CopyCommand';
import HomeCapabilityDemo from '../components/HomeCapabilityDemo';
import HeroHighlights from '../components/HeroHighlights';
import SiteLink from '../components/SiteLink';
import {installCommand, repositoryUrl, type Locale} from '../../content.config';
import componentContent from '../../.generated/homepage-components.json';
import layerContent from '../../.generated/homepage-layers.json';
import styles from './index.module.css';

type LocalizedText = Record<Locale, string>;
type ComponentInfo = {label: LocalizedText; description: LocalizedText; to: string};
type LayerInfo = {id: string; label: LocalizedText; components: string[]};
const components = componentContent as Record<string, ComponentInfo>;
const layers = layerContent as LayerInfo[];

const content = {
  en: {
    title: 'System Software Stack for AI Agents.',
    description: 'ANOLISA provides context management, Skills, workspace recovery, security, and observability for AI Agent workloads.',
    scopeLine: 'Context. Skills. Recovery. Security. Observability.',
    statement: 'The users of operating systems have changed. ANOLISA makes Agents first-class citizens.',
    quickstart: 'Get started',
    watch: 'Explore the stack',
    installLabel: 'Install the ANOLISA CLI',
    copy: 'Copy',
    copied: 'Copied',
    imagesTitle: 'Images & cloud services',
    imagesBody: 'Start with a preinstalled environment, or build from a container base image.',
    systemTitle: 'Turn your system into an Agentic OS',
    systemBody: 'Install ANOLISA on supported Linux systems and keep your existing environment.',
    agentTitle: 'Enable ANOLISA for your Agent',
    agentBody: 'Add system capabilities through adapters and plugins. Keep the Agent you use.',
    whyEyebrow: '02 / WHY ANOLISA',
    whyTitle: 'Better systems. Better Agents.',
    whyIntro: 'Enhance the Agent applications, frameworks, and harnesses you already use.',
    benefits: [
      {title: 'Keep context useful.', body: 'Compress tool output and reuse memory across sessions.'},
      {title: 'Find the right Skills.', body: 'Discover OS instructions and Skills when a task needs them.'},
      {title: 'Keep control. Roll back changes.', body: 'Constrain actions with policies. Inspect and restore workspace snapshots.'},
      {title: 'See what happened.', body: 'Inspect model and tool calls, trajectories, and Token usage.'},
    ],
    ecosystemTitle: 'Build it into your ecosystem.',
    osRole: 'OS vendors & distribution communities',
    osRoleBody: 'Integrate ANOLISA into an existing distribution, or include it in your Agentic OS design from day one. Adopt and help build the stack.',
    platformRole: 'Agent governance platforms & sandbox providers',
    platformRoleBody: 'Preinstall ANOLISA in your runtime environments to give Agent workloads system support.',
    catalogEyebrow: '03 / THE SYSTEM SOFTWARE STACK',
    catalogTitle: 'Three layers. One stack.',
    catalogIntro: 'From user interaction to unified integration and Agent Semantic Layer capabilities.',
    auxiliary: 'Installation and maintenance: ',
    distribution: 'ANOLISA CLI',
    distributionBody: ' manages component installation and lifecycle. ',
    howEyebrow: '04 / OPEN ARCHITECTURE',
    howTitle: 'Extend the stack through AW.',
    howIntro: 'Connect your Agent, or bring your own system software optimized for Agent workloads.',
    sdk: 'SDK',
    sdkBody: 'Call component capabilities from application code.',
    awIntegration: 'Agent integration',
    awBody: 'Connect Agents and frameworks to AW.',
    providers: 'Provider development',
    providersBody: 'Bring your system software into ANOLISA.',
    awGuide: 'AW configuration and usage',
    communityEyebrow: '05 / COMMUNITY',
    communityTitle: 'Build ANOLISA with us.',
    contribute: 'Contribute',
    contributeBody: 'Improve the code, refine the docs, or share an image or cloud service entry.',
    contributeLink: 'Contribution guide',
    issues: 'Open an issue',
    submitImage: 'Add an image or service',
    community: 'Stay connected',
    communityBody: 'Share what you build and help shape what comes next.',
    discussion: 'Join the community',
    securityPolicy: 'Security policy',
    forAgents: 'For Agents',
    changelog: 'Changelog',
    securityReferences: 'Agent Security Core references: ',
    owasp: 'OWASP Agentic Top 10 control mapping',
  },
  zh: {
    title: 'Agent 原生的操作系统软件栈',
    description: 'ANOLISA 为 AI Agent 工作负载提供上下文管理、Skills、工作区恢复、安全与观测能力。',
    scopeLine: '上下文管理 · Skills 供给 · 工作区恢复 · 安全 · 观测',
    statement: '操作系统的使用者已经改变。ANOLISA 让 Agent 成为系统中的一等公民。',
    quickstart: '开始使用',
    watch: '探索系统能力',
    installLabel: '安装 ANOLISA CLI',
    copy: '复制',
    copied: '已复制',
    imagesTitle: '从镜像与云服务开始',
    imagesBody: '使用预装环境，或以容器基础镜像搭建自己的 Agent 环境。',
    systemTitle: '将已有系统扩展为 Agentic OS',
    systemBody: '在受支持的 Linux 系统上安装 ANOLISA，保留已有环境。',
    agentTitle: '为已有 Agent 启用 ANOLISA',
    agentBody: '通过适配器与插件接入系统能力，继续使用原来的 Agent。',
    whyEyebrow: '02 / 为什么需要 ANOLISA',
    whyTitle: '让系统更懂 Agent。',
    whyIntro: '增强你正在使用的 Agent 应用、框架与 Harness。',
    benefits: [
      {title: '把上下文留给任务。', body: '精简工具输出，跨会话复用记忆。'},
      {title: '按任务获取 Skills。', body: '让 Agent 按需查阅 OS 说明，发现并读取 Skills。'},
      {title: '操作有边界，工作区有退路。', body: '用策略约束操作，用快照检查与恢复工作区。'},
      {title: '看清每一次运行。', body: '查看模型与工具调用、运行轨迹和 Token 开销。'},
    ],
    ecosystemTitle: '融入你的系统，共建开放生态。',
    osRole: 'OS 发行商与社区',
    osRoleBody: '你可以将 ANOLISA 集成到现有发行版，也可以在设计 Agentic OS 时将它纳入整体架构。欢迎采用与共建。',
    platformRole: 'Agent 治理平台开发者与沙箱服务提供方',
    platformRoleBody: '你可以在运行环境中预置 ANOLISA，为 Agent 工作负载提供系统支持。',
    catalogEyebrow: '03 / 系统软件栈',
    catalogTitle: '三个层次，一套软件栈。',
    catalogIntro: '从用户交互、统一接入，到 Agent 语义层能力供给。',
    auxiliary: '安装与维护工具：',
    distribution: 'ANOLISA CLI',
    distributionBody: ' 负责组件安装与生命周期管理。',
    howEyebrow: '04 / 开放架构',
    howTitle: '通过 AW 扩展软件栈。',
    howIntro: '你可以接入自己的 Agent，也可以接入面向 Agent 工作负载优化的自有系统软件。',
    sdk: 'SDK',
    sdkBody: '在应用代码中调用组件能力。',
    awIntegration: 'Agent 接入',
    awBody: '将 Agent 与框架接入 AW。',
    providers: 'Provider 开发',
    providersBody: '将自有系统软件接入 ANOLISA。',
    awGuide: 'AW 配置与使用指南',
    communityEyebrow: '05 / 社区',
    communityTitle: '一起构建 ANOLISA。',
    contribute: '参与贡献',
    contributeBody: '欢迎改进代码、完善文档，也欢迎补充镜像与云服务入口。',
    contributeLink: '贡献者指南',
    issues: '提交 Issue',
    submitImage: '补充镜像或云服务',
    community: '保持交流',
    communityBody: '分享你的实践，一起讨论下一步。',
    discussion: '加入社区交流',
    securityPolicy: '安全策略',
    forAgents: 'Agent 入口',
    changelog: '变更日志',
    securityReferences: 'Agent Security Core 参考资料：',
    owasp: 'OWASP Agentic Top 10 控制映射',
  },
} as const;

function ComponentCard({id, locale}: {id: string; locale: Locale}) {
  const component = components[id];
  return (
    <SiteLink locale={locale} to={component.to} className={styles.componentCard}>
      <div className={styles.componentTitle}>
        <h4>{component.label[locale]}</h4><span aria-hidden="true">↗</span>
      </div>
      <p>{component.description[locale]}</p>
    </SiteLink>
  );
}

export default function Home() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = content[locale];
  const lockupDark = useBaseUrl('/img/brand/anolisa-lockup-dark.svg');
  const stackLayers = layers.map((layer) => ({...layer, components: layer.components.filter((id) => id !== 'ktuner')}))
    .filter((layer) => layer.components.length > 0);
  const starts = [
    {title: t.imagesTitle, body: t.imagesBody, to: '/docs/user-guide/getting-started/images/'},
    {title: t.systemTitle, body: t.systemBody, to: '/docs/user-guide/getting-started/installation/'},
    {title: t.agentTitle, body: t.agentBody, to: '/docs/user-guide/getting-started/installation/'},
  ];
  const resources = [
    {title: t.awIntegration, body: t.awBody, to: '/docs/developer-guide/aw/'},
    {title: t.providers, body: t.providersBody, to: '/docs/developer-guide/aw/providers/'},
    {title: t.sdk, body: t.sdkBody, to: '/docs/developer-guide/sdk/'},
  ];
  const communityReadme = `${repositoryUrl}/blob/main/${locale === 'zh' ? 'README_zh.md#社区' : 'README.md#community'}`;

  return (
    <Layout title={t.title} description={t.description}>
      <Head><meta property="og:type" content="website" /></Head>
      <main className={styles.page}>
        <section className={styles.hero} aria-labelledby="home-title">
          <div className={`${styles.container} ${styles.heroGrid}`}>
            <div>
              <img className={styles.wordmark} src={lockupDark} alt="ANOLISA" />
              <h1 id="home-title">{t.title}</h1>
              <HeroHighlights locale={locale} />
              <p className={styles.scopeLine}>{t.scopeLine}</p>
              <p className={styles.heroStatement}>{t.statement}</p>
              <div className={styles.actions}>
                <a href="#get-started" className={styles.primaryButton}>{t.quickstart}<span aria-hidden="true">→</span></a>
                <a href="#demos" className={styles.secondaryButton}>{t.watch}<span aria-hidden="true">↘</span></a>
              </div>
              <div className={styles.heroInstaller}>
                <p className={styles.eyebrow}>{t.installLabel}</p>
                <CopyCommand command={installCommand} label={t.copy} copiedLabel={t.copied} />
              </div>
            </div>
            <aside id="get-started" className={styles.startPanel} aria-labelledby="start-title">
              <h2 id="start-title" className={styles.startPanelHeading}>{t.quickstart}</h2>
              <div className={styles.startStack}>
                {starts.map((start, index) => (
                  <SiteLink locale={locale} to={start.to} className={styles.startCard} key={start.title}>
                    <span className={styles.cardNumber}>0{index + 1}</span><h3>{start.title}</h3><p>{start.body}</p>
                    <span className={styles.cardLink} aria-hidden="true">→</span>
                  </SiteLink>
                ))}
              </div>
            </aside>
          </div>
        </section>

        <HomeCapabilityDemo locale={locale} />

        <section id="why-anolisa" className={styles.section} aria-labelledby="why-title">
          <div className={styles.container}>
            <header className={styles.sectionHeading}>
              <p className={styles.eyebrow}>{t.whyEyebrow}</p><h2 id="why-title">{t.whyTitle}</h2>
              <p>{t.whyIntro}</p>
            </header>
            <div className={styles.benefitGrid}>
              {t.benefits.map((benefit) => (
                <article className={styles.benefitCard} key={benefit.title}>
                  <h3>{benefit.title}</h3><p>{benefit.body}</p>
                </article>
              ))}
            </div>
            <div className={styles.ecosystem}>
              <h3 className={styles.ecosystemTitle}>{t.ecosystemTitle}</h3>
              <div className={styles.roleGrid}>
                <article className={styles.roleCopy}><h4>{t.osRole}</h4><p>{t.osRoleBody}</p></article>
                <article className={styles.roleCopy}><h4>{t.platformRole}</h4><p>{t.platformRoleBody}</p></article>
              </div>
            </div>
          </div>
        </section>

        <section id="components" className={`${styles.section} ${styles.catalogSection}`} aria-labelledby="catalog-title">
          <div className={styles.container}>
            <header className={styles.sectionHeading}>
              <p className={styles.eyebrow}>{t.catalogEyebrow}</p><h2 id="catalog-title">{t.catalogTitle}</h2><p>{t.catalogIntro}</p>
            </header>
            {stackLayers.map((layer, index) => (
              <div className={styles.catalogLayer} key={layer.id}>
                <h3 className={styles.layerTitle}>3.{index + 1} / {layer.label[locale]}</h3>
                <div className={`${styles.componentGrid} ${layer.components.length === 1 ? styles.singleComponent : ''}`}>
                  {layer.components.map((id) => <ComponentCard id={id} locale={locale} key={id} />)}
                </div>
              </div>
            ))}
            <p className={styles.auxiliaryNote}>
              {t.auxiliary}<SiteLink locale={locale} to="/docs/user-guide/getting-started/anolisa-cli/">{t.distribution}</SiteLink>{t.distributionBody}
              <SiteLink locale={locale} to={components.ktuner.to}>{components.ktuner.label[locale]}</SiteLink>{locale === 'zh' ? '：' : ': '}{components.ktuner.description[locale]}
            </p>
          </div>
        </section>

        <section id="open-architecture" className={styles.section} aria-labelledby="how-title">
          <div className={styles.container}>
            <header className={styles.sectionHeading}>
              <p className={styles.eyebrow}>{t.howEyebrow}</p><h2 id="how-title">{t.howTitle}</h2><p>{t.howIntro}</p>
            </header>
            <div className={styles.resourceGrid}>
              {resources.map((resource) => (
                <SiteLink locale={locale} to={resource.to} className={styles.resourceCard} key={resource.to}>
                  <h3>{resource.title}<span aria-hidden="true"> ↗</span></h3><p>{resource.body}</p>
                </SiteLink>
              ))}
            </div>
            <SiteLink locale={locale} to="/docs/user-guide/aw/" className={styles.textLink}>{t.awGuide}<span aria-hidden="true"> →</span></SiteLink>
          </div>
        </section>

        <section id="community" className={`${styles.section} ${styles.resourcesSection}`} aria-labelledby="community-title">
          <div className={styles.container}>
            <header className={styles.sectionHeading}>
              <p className={styles.eyebrow}>{t.communityEyebrow}</p><h2 id="community-title">{t.communityTitle}</h2>
            </header>
            <div className={styles.communityGrid}>
              <div className={styles.communityCopy}>
                <h3>{t.contribute}</h3><p>{t.contributeBody}</p>
                <ul className={styles.communityLinks}>
                  <li><SiteLink locale={locale} to="/docs/contribute/">{t.contributeLink}</SiteLink></li>
                  <li><a href={`${repositoryUrl}/issues`}>{t.issues}</a></li>
                  <li><SiteLink locale={locale} to="/docs/user-guide/getting-started/images/">{t.submitImage}</SiteLink></li>
                </ul>
              </div>
              <div className={styles.communityCopy}>
                <h3>{t.community}</h3><p>{t.communityBody}</p>
                <ul className={styles.communityLinks}>
                  <li><a href={communityReadme}>{t.discussion}</a></li>
                  <li><a href={`${repositoryUrl}/blob/main/SECURITY.md`}>{t.securityPolicy}</a></li>
                  <li><SiteLink locale={locale} to="/agents/">{t.forAgents}</SiteLink></li>
                  <li><SiteLink locale={locale} to="/changelog/">{t.changelog}</SiteLink></li>
                </ul>
              </div>
            </div>
            <p className={styles.communityMeta}>
              {t.securityReferences}<a href="https://aarm.dev/builders/agentseccore-anolisa">AARM Aligned</a>{' · '}
              <SiteLink locale={locale} to="/docs/user-guide/agent-sec-core/owasp-agentic-top10/">{t.owasp}</SiteLink>
            </p>
          </div>
        </section>
      </main>
    </Layout>
  );
}
