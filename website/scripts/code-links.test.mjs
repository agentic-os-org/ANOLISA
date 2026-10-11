import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {compile, createProcessor} from '@mdx-js/mdx';
import remarkGfm from 'remark-gfm';
const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
function fencedValues(markdown) {
  const values = [];
  function visit(node) {
    if (node.type === 'code') values.push(node.value);
    for (const child of node.children || []) visit(child);
  }
  visit(createProcessor({remarkPlugins: [remarkGfm]}).parse(markdown));
  return values;
}
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
  // The copied scripts use the same dependencies installed by npm ci in this
  // package. Junctions work on Windows without symlink privileges.
  await symlink(path.resolve(scriptsDir, '../node_modules'), path.join(root, 'website/node_modules'), 'junction');
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

test('rewrite prose links while retaining Markdown code examples', async (t) => {
  const example = '[sample](BUILDING.md)';
  const markdown = [
    '# Quickstart', '', `Prose ${example}`, '',
    '`' + example + '`', '', '``' + example + ' and `nested` ``', '',
    '````markdown', example, '```', example, '````', '',
    '~~~markdown', example, '~~~', '',
    '![Image](images/test.png)', '',
  ].join('\n');
  const root = await fixture(t, {'docs/QUICKSTART.md': markdown, 'docs/images/test.png': 'fixture image'});
  const result = prepare(root);
  assert.equal(result.status, 0, result.stderr);
  const generated = await readFile(path.join(root, 'website/.generated/docs/quickstart.md'), 'utf8');
  assert.ok(generated.includes('Prose [sample](/docs/building)'));
  assert.ok(generated.includes('`' + example + '`'));
  assert.ok(generated.includes('``' + example + ' and `nested` ``'));
  assert.ok(generated.includes('````markdown\n' + example + '\n```\n' + example + '\n````'));
  assert.ok(generated.includes('~~~markdown\n' + example + '\n~~~'));
  assert.ok(generated.includes('![Image](/images/test.png)'));
  assert.equal(await readFile(path.join(root, 'website/.generated/static/images/test.png'), 'utf8'), 'fixture image');
});

const nestedExamples = [
  ['ordered list backticks', '1. Build\n\n    ```sh\n    echo left | cat\n    [sample](BUILDING.md)\n    ```'],
  ['ordered list tildes', '1. Build\n\n    ~~~sh\n    echo {value} | cat\n    [sample](BUILDING.md)\n    ~~~'],
  ['blockquote backticks', '> ```sh\n> echo {value} | cat\n> [sample](BUILDING.md)\n> ```'],
  ['blockquote tildes', '> ~~~sh\n> echo {value} | cat\n> [sample](BUILDING.md)\n> ~~~'],
  ['nested list and quote', '- Build\n\n  > ```sh\n  > echo {value} | cat\n  > [sample](BUILDING.md)\n  > ```'],
];
for (const [name, example] of nestedExamples) {
  test(`retain fenced examples inside ${name}`, async (t) => {
    const root = await fixture(t, {'docs/QUICKSTART.md': `# Quickstart\n\n${example}\n\nProse [sample](BUILDING.md)\n`});
    const result = prepare(root);
    assert.equal(result.status, 0, result.stderr);
    const generated = await readFile(path.join(root, 'website/.generated/docs/quickstart.md'), 'utf8');
    assert.ok(generated.includes(example), generated);
    assert.ok(generated.includes('Prose [sample](/docs/building)'));
    assert.deepEqual(fencedValues(generated), fencedValues(example));
    await compile(generated, {remarkPlugins: [remarkGfm]});
  });
}

const proseBoundaries = [
  ['blank paragraph', 'a ` stray\n\n[sample](BUILDING.md) {not valid}\n\nend ` mark'],
  ['heading', 'a ` stray\n# [sample](BUILDING.md) {not valid}\nend ` mark'],
  ['list', 'a ` stray\n- [sample](BUILDING.md) {not valid}\n- end ` mark'],
  ['table cells', '| First | Second |\n| --- | --- |\n| ` stray | [sample](BUILDING.md) {not valid} |\n| end ` | prose |'],
  ['backticks in destinations', '[one](https://example.com/`) [sample](BUILDING.md) {not valid} [two](https://example.com/`)'],
];
for (const [name, prose] of proseBoundaries) {
  test(`do not pair inline code across ${name}`, async (t) => {
    const root = await fixture(t, {'docs/QUICKSTART.md': `# Quickstart\n\n${prose}\n`});
    const result = prepare(root);
    assert.equal(result.status, 0, result.stderr);
    const generated = await readFile(path.join(root, 'website/.generated/docs/quickstart.md'), 'utf8');
    assert.ok(generated.includes('[sample](/docs/building)'), generated);
    assert.ok(generated.includes('&#123;not valid&#125;'), generated);
    await compile(generated, {remarkPlugins: [remarkGfm]});
  });
}

test('retain multiline inline code within a single paragraph', async (t) => {
  const example = '`first line\n[sample](BUILDING.md) {literal}\nlast line`';
  const root = await fixture(t, {'docs/QUICKSTART.md': `# Quickstart\n\n${example}\n\nProse [sample](BUILDING.md)\n`});
  const result = prepare(root);
  assert.equal(result.status, 0, result.stderr);
  const generated = await readFile(path.join(root, 'website/.generated/docs/quickstart.md'), 'utf8');
  assert.ok(generated.includes(example), generated);
  assert.ok(generated.includes('Prose [sample](/docs/building)'));
});

test('preserve locale-like links in code while stripping the prose switch', async (t) => {
  const fenced = '````markdown\n```\n[English](BUILDING.md)\n````';
  const inline = '`first\n[English](BUILDING.md)\nlast`';
  const root = await fixture(t, {'docs/QUICKSTART.md': `# Quickstart\n\n[English](BUILDING.md)\n\n${fenced}\n\n${inline}\n`});
  const result = prepare(root);
  assert.equal(result.status, 0, result.stderr);
  const generated = await readFile(path.join(root, 'website/.generated/docs/quickstart.md'), 'utf8');
  assert.ok(generated.includes(fenced), generated);
  assert.ok(generated.includes(inline), generated);
  assert.equal(generated.match(/\[English\]/g)?.length, 2);
  await compile(generated, {remarkPlugins: [remarkGfm]});
});
