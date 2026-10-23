import assert from 'node:assert/strict';
import {mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {insertNavigation, loadNavigation, userGuideSidebar} from './navigation.mjs';
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
