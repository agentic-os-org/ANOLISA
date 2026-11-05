import {loadDocumentRedirects, redirectDestination, writeDocumentRedirects} from '../scripts/document-routes.mjs';

export default async function stableDocumentRedirects(context) {
  const redirects = await loadDocumentRedirects();
  return {
    name: 'stable-document-redirects',
    async contentLoaded({actions}) {
      const locale = context.i18n.currentLocale;
      const baseUrl = process.env.BASE_URL ?? '/';
      for (const [from, to] of Object.entries(redirects)) {
        const data = await actions.createData(`redirect-${from.replaceAll('/', '-') || 'index'}.json`,
          JSON.stringify({to: redirectDestination(to, locale, baseUrl)}));
        actions.addRoute({
          path: `${baseUrl}${locale === 'zh' ? 'zh/' : ''}docs/${from ? `${from}/` : ''}`,
          component: '@site/src/components/DocumentRedirect.tsx',
          modules: {destination: data},
          exact: true,
        });
      }
    },
    async postBuild({outDir}) {
      // Static assets are copied once per locale; restore locale-specific
      // destinations after that copy rather than letting English redirects win.
      await writeDocumentRedirects(outDir, context.i18n.currentLocale, redirects, process.env.BASE_URL ?? '/');
    },
  };
}
