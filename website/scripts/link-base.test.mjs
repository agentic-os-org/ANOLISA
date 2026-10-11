import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readdir, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
async function fixture(t, files) {
  const root = await mkdtemp(path.join(tmpdir(), 'static-links-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  await mkdir(path.join(root, 'scripts'), {recursive: true});
  for (const name of await readdir(scriptsDir)) {
    if (!name.endsWith('.mjs') || name.endsWith('.test.mjs')) continue;
    await copyFile(path.join(scriptsDir, name), path.join(root, 'scripts', name));
  }
  for (const [file, content] of Object.entries(files)) {
    const target = path.join(root, 'build', file);
    await mkdir(path.dirname(target), {recursive: true});
    await writeFile(target, content);
  }
  return root;
}
function check(root, base = '/') {
  const result = spawnSync(process.execPath, [path.join(root, 'scripts/check-links.mjs')], {
    cwd: root, encoding: 'utf8', timeout: 30_000,
    env: {...process.env, SITE_URL: 'https://example.com', BASE_URL: base},
  });
  assert.ifError(result.error);
  return result;
}

test('relative root links and self fragments use the deployed directory', async (t) => {
  const root = await fixture(t, {
    'index.html': '<h1 id="root">Root</h1><a href="#root">self</a><a href="docs/">docs</a>',
    'docs/index.html': '<h1 id="intro">Docs</h1><a href="#intro">self</a><a href="../">home</a>',
  });
  const result = check(root, '/ANOLISA/');
  assert.equal(result.status, 0, result.stderr);
});
test('missing relative root link stays inside the deployment', async (t) => {
  const root = await fixture(t, {'index.html': '<a href="missing/">missing</a>'});
  const result = check(root, '/ANOLISA/');
  assert.equal(result.status, 1);
  assert.match(result.stderr, /broken link missing\//);
});

test('deployment prefix is removed only once from root-relative resolution', async (t) => {
  const root = await fixture(t, {
    'index.html': '<a href="ANOLISA/docs/#nested">nested</a>',
    'docs/index.html': '<h1 id="other">Other</h1>',
    'ANOLISA/docs/index.html': '<h1 id="nested">Nested</h1>',
  });
  const result = check(root, '/ANOLISA/');
  assert.equal(result.status, 0, result.stderr);
});
