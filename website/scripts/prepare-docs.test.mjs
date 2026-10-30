import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {exists, walkFiles} from './lib.mjs';
import {loadDocumentRedirects, redirectDestination, writeDocumentRedirects} from './document-routes.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));

test('publishes current guides and changelogs without Copilot Shell entry points', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'prepare-docs-test-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const files = {};
  for (const locale of ['en', 'zh']) {
    const suffix = locale === 'zh' ? '_zh' : '';
    for (const name of ['README']) {
      files[`docs/${name}${suffix}.md`] = `# ${name}\n`;
    }
    files[`CHANGELOG${suffix}.md`] = '# Changelog\n\ncopilot-shell release history. [Historical details](deprecated/copilot-shell/CHANGELOG.md)\n';
    files[`src/current/CHANGELOG${suffix}.md`] = '# Current component history\n';
    files[`deprecated/copilot-shell/CHANGELOG${suffix}.md`] = '# Archived component history\n';
    files[`docs/user-guide/${locale}/index.md`] = '# User Guide\n\n<!-- navigation:start -->\n<!-- navigation:end -->\n\n[historical source evidence](https://github.com/alibaba/anolisa/blob/old-commit/src/copilot-shell/core.ts#L10)\n\n## Copilot Shell (cosh)\n\nHistorical integration details remain.\n';
    files[`docs/user-guide/${locale}/getting-started/quickstart.md`] = '# Quickstart\n\n[cosh-ng](../cosh-ng/index.md)\n';
    files[`docs/user-guide/${locale}/getting-started/support.md`] = '# Support\n';
    files[`docs/user-guide/${locale}/cosh-ng/index.md`] = `# cosh-ng\n\n## ${locale === 'zh' ? '快速上手' : 'Quick start'}\n`;
    files[`docs/user-guide/${locale}/cosh-ng/configuration.md`] = '# Configuration\n';
    files[`docs/user-guide/${locale}/tokenless/index.md`] = '# Tokenless\n';
    files[`docs/user-guide/${locale}/aw/index.md`] = '# AW\n';
    files[`docs/user-guide/${locale}/aw/installation.md`] = '# Install AW\n';
    files[`docs/developer-guide/${locale}/index.md`] = '# Developer Guide\n';
    files[`docs/developer-guide/${locale}/aw/index.md`] = '# AW Integration\n';
    files[`docs/developer-guide/${locale}/aw/providers/index.md`] = '# Providers\n';
    files[`docs/developer-guide/${locale}/aw/providers/tokenless.md`] = '# Tokenless Provider\n\n[SDK](../../sdk/tokenless/index.md)\n';
    files[`docs/developer-guide/${locale}/aw/providers/protocol.md`] = '# Provider Protocol\n\n[Schema](../../../../../src/aw/crates/aw-provider/schemas/request.schema.json)\n\n[Example](../../../../../src/aw/crates/aw-provider/examples/policy.rs#L1)\n';
    files[`docs/developer-guide/${locale}/sdk/tokenless/index.md`] = '# Tokenless SDK\n\n[AgentScope](agentscope.md)\n';
    files[`docs/developer-guide/${locale}/sdk/tokenless/agentscope.md`] = '# AgentScope SDK\n';
    files[`CONTRIBUTING${suffix}.md`] = `# Contribute\n\n[AW](src/aw/CONTRIBUTING${suffix}.md)\n\n[CI](.github/workflows/ci.yaml)\n\n[Sources](src/aw/)\n\n[Website](/docs/user-guide/)\n\n[External](https://example.com/source.yaml#section)\n`;
    files[`src/aw/CONTRIBUTING${suffix}.md`] = `# Contribute to AW\n\n[Build](../../contribute/${locale}/aw/building.md)\n\n[Toolchain](rust-toolchain.toml)\n\n[Check](scripts/check_layout.py)\n`;
    files[`src/cosh-ng/CONTRIBUTING${suffix}.md`] = '# Contribute to cosh-ng\n';
    files[`contribute/${locale}/aw/building.md`] = `# Build AW\n\n[Contribute](../../../CONTRIBUTING${suffix}.md)\n\n[Design](../../../src/aw/docs/design/${locale}/architecture.md)\n`;
    files[`src/aw/docs/design/${locale}/architecture.md`] = '# AW Architecture\n';
    files[`deprecated/copilot-shell/docs/user-guide/${locale}/QUICKSTART.md`] = '# Archived quickstart\n';
    files[`deprecated/copilot-shell/docs/developer-guide/${locale}/architecture.md`] = '# Archived architecture\n';
    files[`deprecated/copilot-shell/docs/developer-guide/${locale}/hooks/index.md`] = '# Archived hooks\n';
  }
  files['.github/workflows/ci.yaml'] = 'name: CI\n';
  files['src/aw/rust-toolchain.toml'] = '[toolchain]\nchannel = "stable"\n';
  files['src/aw/scripts/check_layout.py'] = '# Layout check\n';
  files['src/aw/crates/aw-provider/schemas/request.schema.json'] = '{}\n';
  files['src/aw/crates/aw-provider/examples/policy.rs'] = 'fn main() {}\n';
  files['docs/navigation.yaml'] = `version: 1
groups:
  - id: start
    label: {en: Start, zh: 开始}
    items:
      - path: getting-started/quickstart.md
        label: {en: Quickstart, zh: 快速开始}
      - component: cosh-ng
        label: {en: cosh, zh: cosh}
        description:
          en: Use Agents and Shell commands in the same terminal.
          zh: 在同一个终端中使用 Agent 和 Shell 命令。
  - id: resources
    label: {en: Support resources, zh: 支持资源}
    items:
      - path: getting-started/support.md
        label: {en: Support, zh: 支持}
  - id: capabilities
    label: {en: Runtime services, zh: 运行服务}
    items:
      - component: tokenless
        label: {en: Output compaction, zh: 输出精简}
        description:
          en: Compact tool output for Agents.
          zh: 为 Agent 精简工具输出。
      - component: aw
        label: {en: Agent runtime, zh: Agent 运行时}
        description:
          en: Connect Agents to system services.
          zh: 将 Agent 接入系统服务。
`;
  const website = {
    navbar: [{id: 'sdk', label: {en: 'SDK Catalog', zh: 'SDK 目录'}, to: '/docs/developer-guide/sdk/'}],
    footer: [{id: 'guides', label: {en: 'Guides', zh: '指南'},
      items: [{label: {en: 'SDK Catalog', zh: 'SDK 目录'}, to: '/docs/developer-guide/sdk/'}]}],
    sidebars: {
      sdkGuide: [{id: 'sdk', label: {en: 'SDK Catalog', zh: 'SDK 目录'}, generated: '/developer-guide/sdk',
        items: [{id: 'tokenless', label: {en: 'Tokenless Integration API', zh: 'Tokenless 集成 API'},
          doc: 'developer-guide/sdk/tokenless/index', items: [{id: 'agentscope',
            label: {en: 'AgentScope', zh: 'AgentScope'}, doc: 'developer-guide/sdk/tokenless/agentscope'}]}]}],
      awGuide: [{id: 'providers', label: {en: 'Provider Interfaces', zh: 'Provider 接口'},
        doc: 'developer-guide/aw/providers/index', items: [{id: 'protocol',
          label: {en: 'Protocol', zh: '协议'}, doc: 'developer-guide/aw/providers/protocol'}]}],
      contributeGuide: [{id: 'aw', label: {en: 'AW Contributors', zh: 'AW 贡献者'},
        doc: 'contribute/aw/index', items: [{id: 'building', label: {en: 'Build', zh: '构建'},
          doc: 'contribute/aw/building'}]}],
    },
  };
  files['docs/navigation.yaml'] += `website: ${JSON.stringify(website)}\n`;
  for (const [relativePath, content] of Object.entries(files)) {
    const target = path.join(root, relativePath);
    await mkdir(path.dirname(target), {recursive: true});
    await writeFile(target, content);
  }
  await mkdir(path.join(root, 'website/scripts'), {recursive: true});
  for (const name of ['prepare-docs.mjs', 'generate-changelog.mjs', 'lib.mjs', 'navigation.mjs', 'document-routes.mjs']) {
    await copyFile(path.join(scriptsDir, name), path.join(root, 'website/scripts', name));
  }

  await symlink(path.join(scriptsDir, '../node_modules'), path.join(root, 'website/node_modules'), 'dir');

  for (const script of ['prepare-docs.mjs', 'generate-changelog.mjs']) {
    const result = spawnSync(process.execPath, [path.join(root, 'website/scripts', script)], {
      cwd: root,
      encoding: 'utf8',
      timeout: 30_000,
      env: {...process.env, BASE_URL: '/ANOLISA/'},
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
  }
  const homepageComponents = JSON.parse(await readFile(
    path.join(root, 'website/.generated/homepage-components.json'), 'utf8'));
  assert.deepEqual(homepageComponents, {
    'cosh-ng': {
      label: {en: 'cosh', zh: 'cosh'},
      description: {
        en: 'Use Agents and Shell commands in the same terminal.',
        zh: '在同一个终端中使用 Agent 和 Shell 命令。',
      },
      to: '/docs/user-guide/cosh-ng/',
    },
    tokenless: {
      label: {en: 'Output compaction', zh: '输出精简'},
      description: {
        en: 'Compact tool output for Agents.',
        zh: '为 Agent 精简工具输出。',
      },
      to: '/docs/user-guide/tokenless/',
    },
    aw: {
      label: {en: 'Agent runtime', zh: 'Agent 运行时'},
      description: {
        en: 'Connect Agents to system services.',
        zh: '将 Agent 接入系统服务。',
      },
      to: '/docs/user-guide/aw/',
    },
  });
  const homepageLayers = JSON.parse(await readFile(
    path.join(root, 'website/.generated/homepage-layers.json'), 'utf8'));
  assert.deepEqual(homepageLayers, [
    {id: 'start', label: {en: 'Start', zh: '开始'}, components: ['cosh-ng']},
    {id: 'capabilities', label: {en: 'Runtime services', zh: '运行服务'},
      components: ['tokenless', 'aw']},
  ], 'Homepage layers must preserve navigation order and labels while excluding ordinary document entries');
  for (const locale of ['en', 'zh']) {
    const output = path.join(root, 'website/.generated', locale === 'en'
      ? 'docs'
      : 'i18n/zh/docusaurus-plugin-content-docs/current');
    assert.ok(await exists(path.join(output, 'user-guide/getting-started/quickstart.md')));
    const userGuide = await readFile(path.join(output, 'user-guide/index.md'), 'utf8');
    assert.ok(!userGuide.includes('navigation:start'));
    assert.ok(!userGuide.includes('navigation:end'));
    const sourceGuide = await readFile(path.join(root, `docs/user-guide/${locale}/index.md`), 'utf8');
    assert.ok(sourceGuide.includes('<!-- navigation:start -->'));
    assert.ok(sourceGuide.includes('<!-- navigation:end -->'));
    assert.ok(userGuide.includes('historical source evidence'));
    assert.ok(userGuide.includes(locale === 'zh' ? '## 开始' : '## Start'));
    assert.ok(userGuide.includes(locale === 'zh'
      ? '[cosh](/docs/user-guide/cosh-ng)：在同一个终端中使用 Agent 和 Shell 命令。'
      : '[cosh](/docs/user-guide/cosh-ng): Use Agents and Shell commands in the same terminal.'));
    assert.ok(userGuide.includes('/docs/user-guide/getting-started/quickstart'));
    const entry = await readFile(path.join(output, 'user-guide/cosh-ng/index.md'), 'utf8');
    assert.ok(entry.includes(`## ${locale === 'zh' ? '快速上手' : 'Quick start'} {#quick-start}`));
    const quickstart = await readFile(path.join(output, 'user-guide/getting-started/quickstart.md'), 'utf8');
    assert.ok(quickstart.includes('[cosh-ng](/docs/user-guide/cosh-ng)'));
    const aw = await readFile(path.join(output, 'user-guide/aw/installation.md'), 'utf8');
    assert.match(aw, /slug: "\/user-guide\/aw\/installation"/);
    const contribute = await readFile(path.join(output, 'contribute/index.md'), 'utf8');
    assert.ok(contribute.includes('[AW](/docs/contribute/aw)'));
    assert.ok(contribute.includes('[CI](https://github.com/agentic-os-org/ANOLISA/blob/main/.github/workflows/ci.yaml)'));
    assert.ok(contribute.includes('[Sources](https://github.com/agentic-os-org/ANOLISA/tree/main/src/aw/)'));
    assert.ok(contribute.includes('[Website](/docs/user-guide/)'));
    assert.ok(contribute.includes('[External](https://example.com/source.yaml#section)'));
    assert.ok(contribute.includes(`custom_edit_url: "https://github.com/agentic-os-org/ANOLISA/edit/main/CONTRIBUTING${locale === 'zh' ? '_zh' : ''}.md"`));
    const componentContribute = await readFile(path.join(output, 'contribute/aw/index.md'), 'utf8');
    assert.ok(componentContribute.includes(`sidebar_label: ${JSON.stringify(website.sidebars.contributeGuide[0].label[locale])}`));
    assert.ok(componentContribute.includes('[Build](/docs/contribute/aw/building)'));
    assert.ok(componentContribute.includes('[Toolchain](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/rust-toolchain.toml)'));
    assert.ok(componentContribute.includes('[Check](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py)'));
    const build = await readFile(path.join(output, 'contribute/aw/building.md'), 'utf8');
    assert.ok(build.includes('[Contribute](/docs/contribute)'));
    assert.ok(build.includes(`[Design](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/${locale}/architecture.md)`));
    assert.ok(!(await exists(path.join(output, 'developer-guide/aw/architecture.md'))));
    assert.ok(!(await exists(path.join(output, 'src/aw/docs/design'))));
    const sdk = await readFile(path.join(output, 'developer-guide/sdk/tokenless/index.md'), 'utf8');
    assert.ok(sdk.includes(`sidebar_label: ${JSON.stringify(website.sidebars.sdkGuide[0].items[0].label[locale])}`));
    assert.ok(sdk.includes('[AgentScope](/docs/developer-guide/sdk/tokenless/agentscope)'));
    const provider = await readFile(path.join(output, 'developer-guide/aw/providers/tokenless.md'), 'utf8');
    assert.ok(provider.includes('[SDK](/docs/developer-guide/sdk/tokenless)'));
    const protocol = await readFile(path.join(output, 'developer-guide/aw/providers/protocol.md'), 'utf8');
    assert.ok(protocol.includes('[Schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/schemas/request.schema.json)'));
    assert.ok(protocol.includes('[Example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs#L1)'));
    for (const [directory, label] of [
      ['developer-guide/sdk', website.sidebars.sdkGuide[0].label[locale]],
      ['developer-guide/sdk/tokenless', website.sidebars.sdkGuide[0].items[0].label[locale]],
      ['developer-guide/aw/providers', website.sidebars.awGuide[0].label[locale]],
      ['contribute/aw', website.sidebars.contributeGuide[0].label[locale]],
    ]) {
      const category = JSON.parse(await readFile(path.join(output, directory, '_category_.json'), 'utf8'));
      assert.equal(category.label, label, 'Category metadata must follow the canonical navigation labels');
    }
    assert.ok(!(await exists(path.join(output, 'index.md'))));
    assert.ok(!userGuide.includes('old-commit'));
    assert.ok(userGuide.includes('**Copilot Shell (cosh)**'));
    assert.ok(!userGuide.includes('## Copilot Shell'));
    assert.ok(!(await walkFiles(output)).some((file) => file.includes('copilot-shell')));
    for (const file of await walkFiles(output)) {
      const content = await readFile(file, 'utf8');
      assert.ok(!content.includes('copilot-shell'));
      assert.ok(!content.includes('Archived'));
    }
    const changelog = JSON.parse(await readFile(path.join(root, `website/.generated/data/changelog-${locale}.json`), 'utf8'));
    assert.deepEqual(changelog.map(({name}) => name), ['ANOLISA', 'current']);
    assert.ok(changelog[0].markdown.includes('copilot-shell release history. Historical details'));
    assert.ok(!changelog[0].markdown.includes('deprecated/copilot-shell'));
  }
  const translations = await readFile(path.join(root, 'website/.generated/i18n/zh/docusaurus-plugin-content-docs/current.json'), 'utf8');
  assert.ok(!translations.includes('copilot-shell'));
  const sidebar = JSON.parse(await readFile(path.join(root, 'website/.generated/user-guide-sidebar.json'), 'utf8'));
  assert.equal(sidebar[1].label, 'Start');
  assert.equal(sidebar[1].items[1].link.id, 'user-guide/cosh-ng/index');
  assert.ok(!(await exists(path.join(root, 'website/.generated/static/docs'))));
});

test('migrated developer, SDK, and design routes redirect directly with locale and incoming fragments', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'developer-redirects-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const redirects = await loadDocumentRedirects();
  const migrated = {
    'developer-guide/aw/building': 'contribute/aw/building',
    'developer-guide/cosh-ng/testing': 'contribute/cosh-ng/testing',
    'developer-guide/ws-ckpt/packaging': 'contribute/ws-ckpt/packaging',
    'developer-guide/aw/packages': 'developer-guide/aw/reference/packages',
    'user-guide/aw/packages': 'developer-guide/aw/reference/packages',
    'user-guide/user-entrypoint/aw/packages': 'developer-guide/aw/reference/packages',
    'developer-guide/aw/tokenless': 'developer-guide/aw/providers/tokenless',
    'developer-guide/aw/sec-core': 'developer-guide/aw/providers/sec-core',
    'user-guide/tokenless/sdk': 'developer-guide/sdk/tokenless',
    'user-guide/tokenless/sdk/agentscope': 'developer-guide/sdk/tokenless/agentscope',
    'user-guide/token-saving/tokenless/sdk': 'developer-guide/sdk/tokenless',
    'user-guide/token-saving/tokenless/sdk/agentscope': 'developer-guide/sdk/tokenless/agentscope',
  };
  for (const [from, to] of Object.entries(migrated)) {
    assert.equal(redirects[from], to);
    assert.ok(!Object.hasOwn(redirects, to), `${from} must redirect directly to the current route`);
  }
  // The Provider entry keeps its public URL when providers.md becomes providers/index.md.
  assert.ok(!Object.hasOwn(redirects, 'developer-guide/aw/providers'));
  for (const locale of ['en', 'zh']) {
    assert.equal(redirectDestination(redirects['user-guide/tokenless/sdk'], locale, '/ANOLISA/'),
      `/ANOLISA/${locale === 'zh' ? 'zh/' : ''}docs/developer-guide/sdk/tokenless/`);
    const design = redirectDestination(redirects['developer-guide/aw/architecture'], locale, '/ANOLISA/');
    assert.equal(design,
      `https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/${locale}/architecture.md`);
    await writeDocumentRedirects(path.join(root, locale), locale, redirects, '/ANOLISA/');
    const sourceRedirect = await readFile(path.join(root, locale,
      'docs/developer-guide/aw/architecture/index.html'), 'utf8');
    assert.ok(sourceRedirect.includes(design));
    assert.ok(sourceRedirect.includes('if(location.hash)target.hash=location.hash'));
    assert.ok(!sourceRedirect.includes('{locale}'));
  }
});
