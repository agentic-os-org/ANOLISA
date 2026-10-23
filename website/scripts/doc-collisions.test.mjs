import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
async function fixture(t, extra) {
  const root = await mkdtemp(path.join(tmpdir(), 'prepare-docs-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const files = Object.fromEntries(['README', 'QUICKSTART', 'BUILDING'].flatMap((name) => [
    [`docs/${name}.md`, `# ${name}\n`], [`docs/${name}_zh.md`, `# ${name}\n`],
  ]));
  Object.assign(files, extra);
  for (const [file, content] of Object.entries(files)) {
    const target = path.join(root, file);
    await mkdir(path.dirname(target), {recursive: true});
    await writeFile(target, content);
  }
  for (const section of ['user-guide', 'developer-guide']) for (const locale of ['en', 'zh']) {
    await mkdir(path.join(root, 'docs', section, locale), {recursive: true});
  }
  await mkdir(path.join(root, 'website/scripts'), {recursive: true});
  for (const name of ['prepare-docs.mjs', 'lib.mjs']) {
    await copyFile(path.join(scriptsDir, name), path.join(root, 'website/scripts', name));
  }
  await symlink(path.resolve(scriptsDir, "../node_modules"), path.join(root, "website/node_modules"), "junction");
  return root;
}
function prepare(root) {
  const result = spawnSync(process.execPath, [path.join(root, 'website/scripts/prepare-docs.mjs')], {
    cwd: root, encoding: 'utf8', timeout: 30_000,
    env: {...process.env, SITE_URL: 'https://example.com', BASE_URL: '/'},
  });
  assert.ifError(result.error);
  return result;
}

for (const [locale, left, right] of [
  ['en', 'README.md', 'index.md'],
  ['zh', 'README.md', 'index.md'],
  ['en', 'tool.md', 'tool/README.md'],
]) {
  test(`reject ${locale} collision ${left} and ${right} before replacing output`, async (t) => {
    const prefix = `docs/user-guide/${locale}/`;
    const root = await fixture(t, {
      [prefix + left]: '# First\n', [prefix + right]: '# Second\n',
      'website/.generated/docs/previous.txt': 'last known good output',
    });
    const result = prepare(root);
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes(prefix + left), result.stderr);
    assert.ok(result.stderr.includes(prefix + right), result.stderr);
    assert.equal(await readFile(path.join(root, 'website/.generated/docs/previous.txt'), 'utf8'), 'last known good output');
  });
}
test('same source path in distinct locales remains valid', async (t) => {
  const root = await fixture(t, {'docs/user-guide/en/tool.md': '# Tool\n', 'docs/user-guide/zh/tool.md': '# 宸ュ叿\n'});
  const result = prepare(root);
  assert.equal(result.status, 0, result.stderr);
});
