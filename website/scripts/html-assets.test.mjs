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

test('missing emitted resources fail the gate with the page and URL', async (t) => {
  const root = await fixture(t, {'index.html': '<img src="/missing.png"><script src="/missing.js"></script><link rel="stylesheet" href="/missing.css"><picture><source srcset="/missing-2x.png 2x, /missing-1x.png 1x"></picture>'});
  const result = check(root);
  assert.equal(result.status, 1);
  for (const name of ['missing.png', 'missing.js', 'missing.css', 'missing-2x.png', 'missing-1x.png']) {
    assert.ok(result.stderr.includes(`index.html: broken resource /${name}`), result.stderr);
  }
});
test('emitted, remote and inline resources remain valid under a base path', async (t) => {
  const root = await fixture(t, {
    'index.html': '<img src="assets/a.png"><img srcset="data:image/png;base64,AAAA 1x, assets/b.png 2x"><script src="https://cdn.example/a.js"></script><link rel="stylesheet" href="/ANOLISA/assets/a.css">',
    'assets/a.png': 'a', 'assets/b.png': 'b', 'assets/a.css': 'body {}',
  });
  const result = check(root, '/ANOLISA/');
  assert.equal(result.status, 0, result.stderr);
});

test('srcset preserves internal commas in local URL tokens', async (t) => {
  const root = await fixture(t, {
    'index.html': '<img srcset="assets/photo,a.png 1x, assets/photo,b.png 2x">',
    'assets/photo,a.png': 'a', 'assets/photo,b.png': 'b',
  });
  const result = check(root, '/ANOLISA/');
  assert.equal(result.status, 0, result.stderr);
});

test('missing comma-containing srcset URLs retain complete diagnostics', async (t) => {
  const root = await fixture(t, {
    'index.html': '<source srcset="/assets/missing,a.png 1x, /assets/present,b.png 2x">',
    'assets/present,b.png': 'b',
  });
  const result = check(root);
  assert.equal(result.status, 1);
  assert.ok(result.stderr.includes('index.html: broken resource /assets/missing,a.png'), result.stderr);
  assert.ok(result.stderr.includes('failed with 1 error(s)'), result.stderr);
});

test('descriptor-free candidates strip trailing separator commas only', async (t) => {
  const root = await fixture(t, {
    'index.html': '<img srcset="/assets/photo,a.png, /assets/photo,b.png,">',
    'assets/photo,a.png': 'a', 'assets/photo,b.png': 'b',
  });
  const result = check(root);
  assert.equal(result.status, 0, result.stderr);
});

test('data URL candidates do not swallow the following local candidate', async (t) => {
  const root = await fixture(t, {
    'index.html': '<img srcset="data:image/png;base64,AAAA, /missing,a.png 2x"><source srcset="data:image/svg+xml,%3Csvg%3E 1x, /assets/photo,b.png 2x">',
    'assets/photo,b.png': 'b',
  });
  const result = check(root);
  assert.equal(result.status, 1);
  assert.ok(result.stderr.includes('index.html: broken resource /missing,a.png'), result.stderr);
  assert.ok(result.stderr.includes('failed with 1 error(s)'), result.stderr);
});
