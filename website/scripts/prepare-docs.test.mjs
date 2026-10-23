import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {exists, walkFiles} from './lib.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));

test('publishes current guides and changelogs without Copilot Shell entry points', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'prepare-docs-test-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const files = {};
  for (const locale of ['en', 'zh']) {
    const suffix = locale === 'zh' ? '_zh' : '';
    for (const name of ['README', 'QUICKSTART', 'BUILDING']) {
      files[`docs/${name}${suffix}.md`] = `# ${name}\n`;
    }
    files[`CHANGELOG${suffix}.md`] = '# Changelog\n\ncopilot-shell release history. [Historical details](deprecated/copilot-shell/CHANGELOG.md)\n';
    files[`src/current/CHANGELOG${suffix}.md`] = '# Current component history\n';
    files[`deprecated/copilot-shell/CHANGELOG${suffix}.md`] = '# Archived component history\n';
    files[`docs/user-guide/${locale}/README.md`] = '# User Guide\n\n[historical source evidence](https://github.com/alibaba/anolisa/blob/old-commit/src/copilot-shell/core.ts#L10)\n\n## Copilot Shell (cosh)\n\nHistorical integration details remain.\n';
    files[`docs/developer-guide/${locale}/README.md`] = '# Developer Guide\n';
    files[`deprecated/copilot-shell/docs/user-guide/${locale}/QUICKSTART.md`] = '# Archived quickstart\n';
    files[`deprecated/copilot-shell/docs/developer-guide/${locale}/architecture.md`] = '# Archived architecture\n';
    files[`deprecated/copilot-shell/docs/developer-guide/${locale}/hooks/index.md`] = '# Archived hooks\n';
  }
  for (const [relativePath, content] of Object.entries(files)) {
    const target = path.join(root, relativePath);
    await mkdir(path.dirname(target), {recursive: true});
    await writeFile(target, content);
  }
  await mkdir(path.join(root, 'website/scripts'), {recursive: true});
  for (const name of ['prepare-docs.mjs', 'generate-changelog.mjs', 'lib.mjs']) {
    await copyFile(path.join(scriptsDir, name), path.join(root, 'website/scripts', name));
  }

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
  for (const locale of ['en', 'zh']) {
    const output = path.join(root, 'website/.generated', locale === 'en'
      ? 'docs'
      : 'i18n/zh/docusaurus-plugin-content-docs/current');
    assert.ok(await exists(path.join(output, 'quickstart.md')));
    const userGuide = await readFile(path.join(output, 'user-guide/index.md'), 'utf8');
    assert.ok(userGuide.includes('historical source evidence'));
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
  assert.ok(!(await exists(path.join(root, 'website/.generated/static/docs'))));
});
