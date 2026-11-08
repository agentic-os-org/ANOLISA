import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const scripts = path.dirname(fileURLToPath(import.meta.url));

async function check(t, html, extraFiles = {}) {
  const root = await mkdtemp(path.join(tmpdir(), 'anolisa-link-check-'));
  assert.equal(path.dirname(root), path.resolve(tmpdir()));
  t.after(() => rm(root, {recursive: true, force: true}));
  const website = path.join(root, 'website');
  await mkdir(path.join(website, 'scripts'), {recursive: true});
  await mkdir(path.join(website, 'build'));
  for (const file of ['check-links.mjs', 'lib.mjs']) {
    await copyFile(path.join(scripts, file), path.join(website, 'scripts', file));
  }
  for (const [file, content] of Object.entries({'index.html': html, ...extraFiles})) {
    const output = path.join(website, 'build', file);
    await mkdir(path.dirname(output), {recursive: true});
    await writeFile(output, content);
  }
  return spawnSync(process.execPath, [path.join(website, 'scripts/check-links.mjs')], {
    encoding: 'utf8',
    env: {...process.env, SITE_URL: 'https://site.example', BASE_URL: '/'},
  });
}

test('non-local URI forms do not become build paths', async (t) => {
  for (const href of ['//outside.example/docs/', 'HTTPS://outside.example/docs/',
    'MAILTO:help@example.com', 'ftp://outside.example/file', 'data:text/plain,hello']) {
    await t.test(href, async (t) => {
      const result = await check(t, `<a href="${href}">External</a>`);
      assert.equal(result.status, 0, result.stdout + result.stderr);
    });
  }
});

test('existing explicit URL schemes still pass', async (t) => {
  const result = await check(t, '<a href="https://outside.example/">External</a>'
    + '<a href="mailto:help@example.com">Mail</a><a href="tel:123">Phone</a>');
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test('invalid scheme-like text does not bypass validation', async (t) => {
  const result = await check(t, '<a href="file_name:missing.html">Invalid</a>');
  assert.equal(result.status, 1);
});

test('local files and fragments are still validated', async (t) => {
  const result = await check(t, '<a href="child.html#section">Child</a>'
    + '<a href="#home">Home</a><h1 id="home">Home</h1>', {
    'child.html': '<h1 id="section">Child</h1>',
  });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test('missing local files remain errors', async (t) => {
  const result = await check(t, '<a href="missing.html">Missing</a>');
  assert.equal(result.status, 1);
  assert.match(result.stderr, /broken link missing\.html/);
});

test('missing local fragments remain errors', async (t) => {
  const result = await check(t, '<a href="#missing">Missing</a>');
  assert.equal(result.status, 1);
  assert.match(result.stderr, /missing fragment #missing/);
});

test('CSS assets are still checked', async (t) => {
  const result = await check(t, '<h1>Home</h1>', {'styles.css': 'body {background:url(missing.png)}'});
  assert.equal(result.status, 1);
  assert.match(result.stderr, /broken CSS asset missing\.png/);
});
