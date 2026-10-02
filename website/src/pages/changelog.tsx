import Layout from '@theme/Layout';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type {Locale} from '../../content.config';
import changelogEn from '../../.generated/data/changelog-en.json';
import changelogZh from '../../.generated/data/changelog-zh.json';

type ChangelogDocument = {
  name: string;
  source: string;
  language: string;
  markdown: string;
};

export default function ChangelogPage() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const documents = (locale === 'zh' ? changelogZh : changelogEn) as ChangelogDocument[];

  return (
    <Layout
      title={locale === 'zh' ? '变更日志' : 'Changelog'}
      description={locale === 'zh' ? 'ANOLISA 项目与组件变更日志' : 'ANOLISA project and component changelogs'}>
      <main className="section changelogPage siteContainer">
        <header className="pageTitle">
          <p className="eyebrow">RELEASE HISTORY</p>
          <h1>{locale === 'zh' ? '变更日志' : 'Changelog'}</h1>
        </header>
        <div className="changelogLayout">
          <nav className="changelogNav" aria-label={locale === 'zh' ? '变更日志来源' : 'Changelog sources'}>
            {documents.map((document, index) => (
              <a key={document.source} href={`#changelog-source-${index}`}>{document.name}</a>
            ))}
          </nav>
          <div className="changelogDocuments">
            {documents.map((document, index) => (
              <article id={`changelog-source-${index}`} key={document.source} className="changelogDocument">
                <div className="sourceLabel">
                  <span>{document.name}</span>
                  <a href={`https://github.com/agentic-os-org/ANOLISA/blob/main/${document.source}`}>{document.source}</a>
                </div>
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{document.markdown}</ReactMarkdown>
              </article>
            ))}
          </div>
        </div>
      </main>
    </Layout>
  );
}
