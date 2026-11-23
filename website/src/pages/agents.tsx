import useBaseUrl from '@docusaurus/useBaseUrl';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import type {Locale} from '../../content.config';

const content = {
  en: {
    eyebrow: 'anolisa://agents',
    title: 'ANOLISA Agent Entry Point',
    intro:
      'One entry point for AI agents exploring what ANOLISA is, how to install and use it, or how to contribute.',
    questionsTitle: 'Start with the question you need answered',
    questions: [
      {
        label: 'What is ANOLISA?',
        detail:
          'Read llms.txt or repo-index.json for ANOLISA’s positioning, non-goals, and three-layer system software stack.',
      },
      {
        label: 'How do I install and use it?',
        detail:
          'Use repo-index.json and cli-reference.txt to check host support and run the safe setup workflow.',
      },
      {
        label: 'How do I join and contribute?',
        detail:
          'Follow the contribution links in llms.txt and specs/contributor-onboarding-guide.md in the repository.',
      },
    ],
    note:
      'Automated setup currently covers Linux and macOS. Ask before making system changes.',
    endpoints: 'Machine-readable endpoints',
  },
  zh: {
    eyebrow: 'anolisa://agents',
    title: 'ANOLISA Agent 入口',
    intro:
      '面向 AI Agent 的统一入口：快速了解 ANOLISA 是什么、如何安装使用，以及如何参与项目协作。',
    questionsTitle: '按问题快速进入',
    questions: [
      {
        label: 'ANOLISA 是什么？',
        detail:
          '读取 llms.txt 或 repo-index.json，获取项目定位、能力边界与三层系统软件栈结构。',
      },
      {
        label: 'ANOLISA 怎么用？',
        detail:
          '读取 repo-index.json 与 cli-reference.txt，确认平台支持范围并按安全流程安装。',
      },
      {
        label: '如何加入这个项目？',
        detail:
          '通过 llms.txt 中的贡献入口或仓库内的 specs/contributor-onboarding-guide.md 匹配赛道与开放任务。',
      },
    ],
    note: '自动安装目前面向 Linux 与 macOS；执行系统变更前应先征得用户确认。',
    endpoints: '机器可读入口',
  },
} as const;

export default function AgentsPage() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = content[locale];
  const endpointItems = [
    ['llms.txt', useBaseUrl('/llms.txt')],
    ['repo-index.json', useBaseUrl('/agents/repo-index.json')],
    ['repo-index.txt', useBaseUrl('/agents/repo-index.txt')],
    ['cli-reference.txt', useBaseUrl('/agents/cli-reference.txt')],
    ['changelog.txt', useBaseUrl('/agents/changelog.txt')],
    ['llms-full.txt', useBaseUrl('/llms-full.txt')],
  ];

  return (
    <Layout title="ANOLISA for Agents" description={t.intro}>
      <main className="agentEntry">
        <header className="agentEntryHero">
          <div className="siteContainer narrowContainer">
            <p className="eyebrow">{t.eyebrow}</p>
            <h1>{t.title}</h1>
            <p>{t.intro}</p>
          </div>
        </header>
        <div className="siteContainer narrowContainer agentEntryBody">
          <section>
            <h2>{t.questionsTitle}</h2>
            <ol>
              {t.questions.map((item) => (
                <li key={item.label}>
                  <strong>{item.label}</strong> {item.detail}
                </li>
              ))}
            </ol>
          </section>
          <section>
            <h2>{t.endpoints}</h2>
            <p>{t.note}</p>
            <div className="agentEndpointGrid">
              {endpointItems.map(([label, href]) => (
                <a href={href} key={label}>
                  <span>{label}</span>
                  <b aria-hidden="true">→</b>
                </a>
              ))}
            </div>
          </section>
        </div>
      </main>
    </Layout>
  );
}
