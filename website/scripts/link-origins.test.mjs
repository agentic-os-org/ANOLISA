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

test('external URLs are skipped according to resolved origin', async (t) => {
  const root = await fixture(t, {'index.html': '<a href="//external.example/missing">external</a><a href="https://other.example/missing">other</a>'});
  const result = check(root);
  assert.equal(result.status, 0, result.stderr);
});
test('absolute internal URLs must resolve to an emitted page and fragment', async (t) => {
  const root = await fixture(t, {'index.html': '<a href="https://example.com/missing/">missing</a><a href="//example.com/docs/#missing">fragment</a>', 'docs/index.html': '<h1 id="valid">Docs</h1>'});
  const result = check(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /broken link https:\/\/example.com\/missing\//);
  assert.match(result.stderr, /missing fragment \/\/example.com\/docs\/#missing/);
});
