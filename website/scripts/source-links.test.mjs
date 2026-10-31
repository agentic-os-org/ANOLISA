import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {exists, repoRoot} from './lib.mjs';
import {checkMarkdownSourceLink, githubSourceAnchors, markdownSourceLinks,
  resolveMarkdownSourceLink} from './source-links.mjs';

test('GitHub source links reject MDX heading IDs in both languages', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'source-links-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  for (const [locale, heading] of [['en', 'Quick start'], ['zh', '快速上手']]) {
    await mkdir(path.join(root, locale));
    await writeFile(path.join(root, locale, 'index.md'), `## ${heading} {#quick-start}\n`);
    assert.match(await checkMarkdownSourceLink(root, 'entry.md', `${locale}/index.md#quick-start`),
      /missing GitHub source fragment/);
    await writeFile(path.join(root, locale, 'index.md'), `## ${heading}\n`);
    assert.equal(await checkMarkdownSourceLink(root, 'entry.md', `${locale}/index.md`), undefined);
    assert.equal(await checkMarkdownSourceLink(root, 'entry.md',
      `${locale}/index.md#${encodeURIComponent(locale === 'en' ? 'quick-start' : '快速上手')}`), undefined);
  }
  assert.match(await checkMarkdownSourceLink(root, 'entry.md', 'missing/index.md'),
    /missing Markdown target/);
});

test('GitHub anchors include formatted headings and explicit HTML anchors, excluding code', () => {
  const anchors = githubSourceAnchors('## **Read** `CLI`\n\n## Read CLI\n\n'
    + '<a name="first-run"></a>\n<span id="details"></span>\n\n'
    + '<!-- <a name="commented"></a> -->\n\n```md\n## Hidden\n<a name="example"></a>\n```\n');
  assert.deepEqual([...anchors], ['read-cli', 'read-cli-1', 'first-run', 'details']);
  assert.deepEqual(markdownSourceLinks('[Start][entry]\n\n[entry]: zh/index.md\n\n'
    + '```md\n[example](missing.md)\n```\n'), [{url: 'zh/index.md', line: 1}]);
});

test('migrated component entry links resolve in the repository Markdown', async () => {
  const entries = new Set();
  for (const locale of ['en', 'zh']) {
    for (const component of ['cosh-ng', 'agentsight', 'tokenless']) {
      const source = `docs/user-guide/${locale}/${component}/index.md`;
      entries.add(path.join(repoRoot, source));
      const markdown = await readFile(path.join(repoRoot, source), 'utf8');
      assert.ok(githubSourceAnchors(markdown).has(locale === 'en' ? 'quick-start' : '快速上手'),
        `${source}: Quickstart heading must work in GitHub Markdown`);
    }
  }
  const sources = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard',
    'docs/**/*.md', 'contribute/**/*.md', ':(glob)*.md', ':(glob)src/*/README*.md',
    ':(glob)src/*/python/*/README*.md', ':(glob)distribution/*/README*.md',
    ':(glob)deprecated/copilot-shell/README*.md', ':(glob)deprecated/copilot-shell/docs/**/*.md'],
  {cwd: repoRoot, encoding: 'utf8'}).trim().split('\n');
  const errors = [];
  let checked = 0;
  for (const source of sources) {
    // Git still lists the old pathname before a working-tree move is staged.
    if (!(await exists(path.join(repoRoot, source)))) continue;
    const markdown = await readFile(path.join(repoRoot, source), 'utf8');
    for (const {url, line} of markdownSourceLinks(markdown)) {
      const resolved = resolveMarkdownSourceLink(repoRoot, source, url);
      if (!resolved || !entries.has(resolved.target)) continue;
      // This gate covers migrated landing links and their old Quickstart IDs.
      // Older links to other Tokenless sections are outside this migration.
      if (resolved.fragment && resolved.fragment !== 'quick-start') continue;
      checked++;
      const error = await checkMarkdownSourceLink(repoRoot, source, url);
      if (error) errors.push(`${error} (line ${line})`);
    }
  }
  assert.ok(checked > 0, 'Expected incoming links to migrated component entries');
  assert.deepEqual(errors, []);
});

test('a quoted greater-than in an earlier HTML attribute does not hide a real anchor', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'source-anchor-quotes-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  for (const quote of ['"', "'"]) {
    const markdown = `<span title=${quote}left > right${quote} id="details">Details</span>\n`;
    const target = path.join(root, 'target.md');
    await writeFile(target, markdown, 'utf8');
    assert.equal(await checkMarkdownSourceLink(root, 'entry.md', 'target.md#details'), undefined);
    assert.equal(await readFile(target, 'utf8'), markdown);
  }
});

test('id and name text inside another quoted attribute cannot satisfy a fragment', async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), 'source-anchor-values-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  for (const attribute of ['id', 'name']) {
    const markdown = `<span title=" ${attribute}=ghost " id="real">Details</span>\n`;
    const target = path.join(root, 'target.md');
    await writeFile(target, markdown, 'utf8');
    assert.equal(await checkMarkdownSourceLink(root, 'entry.md', 'target.md#real'), undefined);
    assert.match(
      await checkMarkdownSourceLink(root, 'entry.md', 'target.md#ghost'),
      /missing GitHub source fragment/,
    );
    assert.equal(await readFile(target, 'utf8'), markdown);
  }
});

test('ordinary heading, ID and named HTML anchors retain their values', () => {
  assert.deepEqual(
    [...githubSourceAnchors('## Ordinary heading\n\n<span id="details"></span>\n'
      + "<a name='legacy'></a>\n<SPAN ID=upper></SPAN>\n")],
    ['ordinary-heading', 'details', 'legacy', 'upper'],
  );
});

test('HTML comments and code examples do not introduce source anchors', () => {
  assert.deepEqual(
    [...githubSourceAnchors('<!-- <span id="commented"></span> -->\n\n'
      + '```html\n<span title="left > right" id="example"></span>\n```\n\n'
      + '<span title="ordinary" id="real"></span>\n')],
    ['real'],
  );
});
