import assert from 'node:assert/strict';
import {mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {
  documentSidebar, footerLinks, insertNavigation, loadNavigation, navbarItems,
  navigationTranslations, userGuideSidebar, websiteSidebars,
} from './navigation.mjs';
import {loadDocumentRedirects, publicDocumentPath} from './document-routes.mjs';
import redirectPlugin from '../plugins/document-redirects.mjs';

test('navigation requires bilingual sources and descriptions and prevents duplicate component entries', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'manual-navigation-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  for (const locale of ['en', 'zh']) {
    await mkdir(path.join(root, 'docs/user-guide', locale, 'aw'), {recursive: true});
  }
  const configuration = {
    version: 1,
    groups: [{id: 'aw', label: {en: 'AW stack', zh: 'AW 统一栈'},
      items: [{component: 'aw', label: {en: 'AW', zh: 'AW'},
        description: {en: 'Use shared Agent capabilities.', zh: '让 Agent 共用能力。'}}]}],
  };
  const save = () => writeFile(path.join(root, 'docs/navigation.yaml'), JSON.stringify(configuration));
  await save();
  await writeFile(path.join(root, 'docs/user-guide/en/aw/index.md'), '# AW\n');
  await assert.rejects(loadNavigation(root), /missing zh document aw\/index.md/);
  await writeFile(path.join(root, 'docs/user-guide/zh/aw/index.md'), '# AW\n');
  const navigation = await loadNavigation(root);
  assert.equal(userGuideSidebar(navigation)[1].items[0].link.id, 'user-guide/aw/index');
  const item = configuration.groups[0].items[0];
  delete item.description.zh;
  await save();
  await assert.rejects(loadNavigation(root), /aw needs a non-empty single-line zh description/);
  item.description.zh = '让 Agent 共用能力。\n保持一行。';
  await save();
  await assert.rejects(loadNavigation(root), /aw needs a non-empty single-line zh description/);
  item.description.zh = '让 Agent 共用能力。';
  configuration.groups[0].items.push(configuration.groups[0].items[0]);
  await save();
  await assert.rejects(loadNavigation(root), /duplicate document path aw\/index.md/);
});

test('regrouping components changes navigation without changing document routes or identities', () => {
  const item = {component: 'aw', label: {en: 'AW', zh: 'AW'},
    description: {en: 'Use shared Agent capabilities.', zh: '让 Agent 共用能力。'}};
  const initial = {groups: [{id: 'stack', label: {en: 'AW stack', zh: 'AW 栈'}, items: [item]}]};
  const regrouped = {groups: [{id: 'semantic', label: {en: 'Semantic layer', zh: '语义层'}, items: [item]}]};
  const first = userGuideSidebar(initial)[1];
  const second = userGuideSidebar(regrouped)[1];
  assert.notEqual(first.label, second.label);
  assert.deepEqual(first.items, second.items);
  assert.equal(first.items[0].label, 'AW');
  assert.ok(!JSON.stringify(first).includes(item.description.en));
  assert.deepEqual(userGuideSidebar(initial, [{target: 'user-guide/aw/index.md'}])[1].items,
    [{type: 'doc', id: 'user-guide/aw/index'}]);
  assert.equal(publicDocumentPath('user-guide/aw/index.md'), 'user-guide/aw');
  assert.equal(publicDocumentPath('user-guide/aw/security.md'), 'user-guide/aw/security');
  assert.equal(publicDocumentPath('user-guide/agent-memory/index.md'), 'user-guide/agent-memory');
  assert.equal(publicDocumentPath('user-guide/ktuner/index.md'), 'user-guide/ktuner');
  assert.equal(publicDocumentPath('user-guide/getting-started/quickstart.md'), 'user-guide/getting-started/quickstart');
  const source = '# Manual\n\nIntroduction.\n\n<!-- navigation:start -->\nold snapshot\n<!-- navigation:end -->\n';
  const output = insertNavigation(source, regrouped, 'zh');
  assert.ok(output.includes('Introduction.'));
  assert.ok(output.includes('## 语义层'));
  assert.ok(output.includes('[AW](aw/index.md)：让 Agent 共用能力。'));
  assert.ok(insertNavigation(source, regrouped, 'en')
    .includes('[AW](aw/index.md): Use shared Agent capabilities.'));
  assert.ok(!output.includes('old snapshot'));
});

test('website navigation separates SDK, AW integration, and contributions using stable routes', async () => {
  const navigation = await loadNavigation();
  const navbar = navbarItems(navigation);
  const developer = navbar.find((item) => item.label === 'Developer Guide');
  assert.deepEqual(developer.items.map(({label, to}) => ({label, to})), [
    {label: 'SDK', to: '/docs/developer-guide/sdk/'},
    {label: 'AW Integration', to: '/docs/developer-guide/aw/'},
  ]);
  assert.equal(navbar.find((item) => item.label === 'Contribute').to, '/docs/contribute/');
  const sidebars = websiteSidebars(navigation);
  assert.deepEqual(sidebars.sdkGuide[0].link,
    {type: 'generated-index', slug: '/developer-guide/sdk', title: 'SDK'});
  assert.equal(sidebars.sdkGuide[0].items[0].link.id, 'developer-guide/sdk/tokenless/index');
  assert.equal(sidebars.sdkGuide[0].items[0].items[0].id, 'developer-guide/sdk/tokenless/agentscope');
  assert.equal(sidebars.contributeGuide[0].id, 'contribute/index');
  assert.deepEqual(sidebars.contributeGuide.slice(1).map((item) => item.label), ['AW', 'cosh-ng', 'ws-ckpt']);
  assert.equal(sidebars.contributeGuide[1].link.id, 'contribute/aw/index');
  assert.equal(sidebars.contributeGuide[2].link.id, 'contribute/cosh-ng/index');
  assert.equal(documentSidebar(navigation, 'developer-guide/sdk/tokenless/index.md'), 'sdkGuide');
  assert.equal(documentSidebar(navigation, 'developer-guide/aw/providers/protocol.md'), 'awGuide');
  assert.equal(documentSidebar(navigation, 'contribute/aw/index.md'), 'contributeGuide');
  assert.equal(documentSidebar(navigation, 'developer-guide/index.md'), 'developerGuide');
  assert.equal(documentSidebar(navigation, 'user-guide/aw/index.md'), 'userGuide');
  assert.ok(footerLinks(navigation).some((group) =>
    group.items.some((item) => item.to === '/docs/developer-guide/')));
});

test('bilingual navigation and generated sidebar titles come from the same configuration', async () => {
  const navigation = await loadNavigation();
  const translated = navigationTranslations(navigation);
  assert.equal(navbarItems(navigation, 'zh').find((item) => item.label === '开发者指南').items[1].label, 'AW 接入');
  assert.equal(translated.navbar['item.label.Contribute'].message, '贡献者指南');
  assert.equal(translated.footer['link.item.label.AW Integration'].message, 'AW 接入');
  assert.equal(translated.sidebar['sidebar.contributeGuide.doc.overview'].message, '通用贡献指南');
  assert.equal(translated.sidebar['sidebar.sdkGuide.category.sdk.link.generated-index.title'].message, 'SDK');
  assert.equal(translated.sidebar['sidebar.awGuide.category.providers'].message, 'Provider 开发与接入');
  navigation.website.navbar[1].items[1].label.zh = 'AW 开发接入';
  assert.equal(navigationTranslations(navigation).navbar['item.label.AW Integration'].message, 'AW 开发接入');
  assert.equal(navbarItems(navigation, 'zh')[1].items[1].label, 'AW 开发接入');
  assert.deepEqual(websiteSidebars({}), {});
  assert.equal(documentSidebar({}, 'developer-guide/index.md'), undefined);
});

test('website configuration rejects ambiguous routes and documents assigned to multiple sidebars', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'website-navigation-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  await mkdir(path.join(root, 'docs'), {recursive: true});
  const navigation = await loadNavigation();
  navigation.groups = [];
  const save = () => writeFile(path.join(root, 'docs/navigation.yaml'), JSON.stringify(navigation));
  await save();
  await loadNavigation(root);
  navigation.website.navbar[0].to = '/docs/../user-guide/';
  await save();
  await assert.rejects(loadNavigation(root), /needs one absolute route or HTTPS URL/);
  navigation.website.navbar[0].to = '/docs/user-guide/';
  navigation.website.sidebars.developerGuide.push({id: 'duplicate', label: {en: 'SDK', zh: 'SDK'},
    doc: 'developer-guide/sdk/tokenless/index'});
  await save();
  await assert.rejects(loadNavigation(root), /duplicate sidebar document developer-guide\/sdk\/tokenless\/index/);
});

test('old routes redirect directly to flat destinations with locale, prefix, and fragments', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'manual-redirect-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const redirects = await loadDocumentRedirects();
  assert.equal(redirects['user-guide/user-entrypoint/aw'], 'user-guide/aw');
  assert.equal(redirects['user-guide/user-entrypoint/aw-preview'], 'user-guide/aw/installation');
  assert.equal(redirects['user-guide/user-entrypoint/aw-sec-core'], 'user-guide/aw/security');
  assert.equal(redirects['user-guide/agent-security/agent-sec-core/quickstart'], 'user-guide/agent-sec-core');
  assert.equal(redirects['user-guide/token-saving/tokenless/user-manual'], 'user-guide/tokenless');
  assert.equal(redirects['user-guide/agent-security/owasp-agentic-top10'], 'user-guide/agent-sec-core/owasp-agentic-top10');
  assert.equal(redirects['user-guide/runtime/skillfs-kubernetes-sidecar'], 'user-guide/skillfs/kubernetes-sidecar');
  assert.equal(redirects.quickstart, 'user-guide/getting-started/quickstart');
  assert.equal(redirects.building, 'user-guide/getting-started/building');
  for (const [from, to] of Object.entries(redirects)) {
    assert.notEqual(from, to);
    assert.ok(!Object.hasOwn(redirects, to.split('#')[0]), `redirect chain: ${from} -> ${to}`);
  }
  for (const locale of ['en', 'zh']) {
    const routes = [];
    const plugin = await redirectPlugin({i18n: {currentLocale: locale}});
    await plugin.contentLoaded({actions: {
      createData: async (name) => name,
      addRoute: (route) => routes.push(route),
    }});
    const prefix = `${process.env.BASE_URL ?? '/'}${locale === 'zh' ? 'zh/' : ''}`;
    assert.ok(routes.some((route) => route.path === `${prefix}docs/`));
    await plugin.postBuild({outDir: path.join(root, locale)});
    const redirect = await readFile(path.join(root, locale,
      'docs/user-guide/user-entrypoint/cosh-ng/quickstart/index.html'), 'utf8');
    assert.ok(redirect.includes(`${prefix}docs/user-guide/cosh-ng/#quick-start`));
    assert.ok(redirect.includes('if(location.hash)target.hash=location.hash'));
    const landing = await readFile(path.join(root, locale, 'docs/index.html'), 'utf8');
    assert.ok(landing.includes(`${prefix}docs/user-guide/`));
    const aw = await readFile(path.join(root, locale, 'docs/user-guide/user-entrypoint/aw/index.html'), 'utf8');
    assert.ok(aw.includes(`${prefix}docs/user-guide/aw/`));
    assert.ok(aw.includes('name="robots" content="noindex"'));
  }
});
