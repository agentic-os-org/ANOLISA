import useBaseUrl from '@docusaurus/useBaseUrl';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import type {Locale} from '../../content.config';

const content = {
  en: {
    eyebrow: 'anolisa://agents',
    title: 'ANOLISA Agent Entry Point',
    intro:
      'Read the repository index and CLI reference for current capabilities, platform scope, and setup methods.',
    note:
      'Automated setup currently covers Linux and macOS. Ask before making system changes.',
    endpoints: 'Machine-readable endpoints',
  },
  zh: {
    eyebrow: 'anolisa://agents',
    title: 'ANOLISA Agent 入口',
    intro:
      '读取仓库索引和 CLI 参考，获取当前可用能力、平台范围与安装方式。',
    note: '自动安装目前面向 Linux 与 macOS；执行系统变更前应先征得用户确认。',
    endpoints: '机器可读入口',
  },
} as const;

export default function AgentsPage() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = content[locale];
  const endpointItems = [
    ['repo-index.json', useBaseUrl('/agents/repo-index.json')],
    ['repo-index.txt', useBaseUrl('/agents/repo-index.txt')],
    ['cli-reference.txt', useBaseUrl('/agents/cli-reference.txt')],
    ['changelog.txt', useBaseUrl('/agents/changelog.txt')],
    ['llms.txt', useBaseUrl('/llms.txt')],
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
