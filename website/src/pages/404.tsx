import Layout from '@theme/Layout';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import SiteLink from '../components/SiteLink';
import type {Locale} from '../../content.config';

export default function NotFound() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';

  return (
    <Layout title="404">
      <main className="notFound siteContainer">
        <p className="eyebrow">ERROR / 404</p>
        <h1>{locale === 'zh' ? '这里没有可执行路径。' : 'No executable path found here.'}</h1>
        <p>{locale === 'zh' ? '返回首页，或从文档索引继续。' : 'Return home or continue from the documentation index.'}</p>
        <div className="buttonRow">
          <SiteLink locale={locale} to="/" className="primaryButton">{locale === 'zh' ? '返回首页' : 'Go home'}</SiteLink>
          <SiteLink locale={locale} to="/docs/quickstart" className="secondaryButton">{locale === 'zh' ? '打开文档' : 'Open docs'}</SiteLink>
        </div>
      </main>
    </Layout>
  );
}
